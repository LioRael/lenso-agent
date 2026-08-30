//! OAuth client-credentials broker for remote Agent services.

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    env,
    rc::Rc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use lenso::prelude::*;
use lenso_capability_agent_oauth_access::{
    self as oauth_contract, AccessError, AccessRequest, AccessResponse, InvalidateError,
    InvalidateRequest, InvalidateResponse,
};
use lenso_kernel::RuntimeFailure;
use reqwest::Url;

const MAX_METADATA_BYTES: usize = 65_536;
const MAX_TOKEN_BYTES: usize = 65_536;

#[derive(Clone, Copy, Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum ClientAuthenticationMethod {
    Basic,
    Post,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceConfig {
    resource_uri: String,
    client_id_environment: String,
    client_secret_environment: String,
    #[serde(default)]
    scopes: Vec<String>,
    #[serde(default)]
    protected_resource_metadata_url: Option<String>,
    #[serde(default)]
    authorization_server: Option<String>,
    client_authentication_method: ClientAuthenticationMethod,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OauthConfig {
    resources: Vec<ResourceConfig>,
    request_timeout_ms: u64,
    refresh_margin_seconds: u64,
}

#[derive(Clone, Debug)]
struct CachedToken {
    access_token: String,
    token_type: String,
    expires_at_millis: u64,
    scopes: Vec<String>,
}

fn validate_config(config: &OauthConfig) -> Result<(), RuntimeFailure> {
    if config.resources.len() > 64
        || !(1..=60_000).contains(&config.request_timeout_ms)
        || config.refresh_margin_seconds > 600
    {
        return Err(invalid_plan("OAuth broker limits are invalid"));
    }
    let mut resources = BTreeSet::new();
    for resource in &config.resources {
        if !safe_service_url(&resource.resource_uri)
            || !resources.insert(resource.resource_uri.as_str())
            || !valid_environment_name(&resource.client_id_environment)
            || !valid_environment_name(&resource.client_secret_environment)
            || resource.scopes.len() > 64
            || resource.scopes.iter().any(|scope| !valid_scope(scope))
            || resource.scopes.iter().collect::<BTreeSet<_>>().len() != resource.scopes.len()
            || resource
                .protected_resource_metadata_url
                .as_deref()
                .is_some_and(|url| !safe_service_url(url))
            || resource
                .authorization_server
                .as_deref()
                .is_some_and(|url| !safe_service_url(url))
        {
            return Err(invalid_plan(
                "OAuth resources require unique safe URIs, environment references, and bounded scopes",
            ));
        }
    }
    Ok(())
}

#[lenso::plugin(configuration_schema = "config.schema.json", validate = validate_config)]
#[derive(Clone, Debug)]
struct ClientCredentialsOauthPlugin {
    #[config]
    config: OauthConfig,
    client: reqwest::Client,
    cache: Rc<RefCell<BTreeMap<String, CachedToken>>>,
}

#[lenso::provides(oauth_contract::OauthAccess)]
impl ClientCredentialsOauthPlugin {
    async fn access(
        &self,
        _context: Ctx,
        request: AccessRequest,
    ) -> PluginResult<AccessResponse, AccessError> {
        let resource = self
            .config
            .resources
            .iter()
            .find(|resource| resource.resource_uri == request.resource_uri)
            .cloned()
            .ok_or_else(|| PluginError::domain(AccessError::UnknownResource))?;
        let scopes = selected_scopes(&resource, &request.scopes)
            .ok_or_else(|| PluginError::domain(AccessError::AuthorizationRejected))?;
        let cache_key = cache_key(&resource.resource_uri, &scopes);
        let refresh_at =
            now_millis().saturating_add(self.config.refresh_margin_seconds.saturating_mul(1_000));
        if let Some(cached) = self.cache.borrow().get(&cache_key)
            && cached.expires_at_millis > refresh_at
        {
            return Ok(access_response(cached));
        }
        let client_id = env::var(&resource.client_id_environment)
            .map_err(|_| PluginError::domain(AccessError::CredentialUnavailable))?;
        let client_secret = env::var(&resource.client_secret_environment)
            .map_err(|_| PluginError::domain(AccessError::CredentialUnavailable))?;
        if client_id.is_empty() || client_secret.is_empty() {
            return Err(PluginError::domain(AccessError::CredentialUnavailable));
        }
        let token = discover_and_request_token(
            &self.client,
            &resource,
            &scopes,
            &client_id,
            &client_secret,
            self.config.request_timeout_ms,
        )
        .await
        .map_err(|error| match error {
            OAuthFailure::Discovery => PluginError::domain(AccessError::DiscoveryRejected),
            OAuthFailure::Rejected => PluginError::domain(AccessError::AuthorizationRejected),
            OAuthFailure::Runtime(error) => PluginError::runtime(error),
        })?;
        let response = access_response(&token);
        self.cache.borrow_mut().insert(cache_key, token);
        Ok(response)
    }

    async fn invalidate(
        &self,
        _context: Ctx,
        request: InvalidateRequest,
    ) -> PluginResult<InvalidateResponse, InvalidateError> {
        if !self
            .config
            .resources
            .iter()
            .any(|resource| resource.resource_uri == request.resource_uri)
        {
            return Err(PluginError::domain(InvalidateError::UnknownResource));
        }
        let prefix = format!("{}\n", request.resource_uri);
        let before = self.cache.borrow().len();
        self.cache
            .borrow_mut()
            .retain(|key, _| !key.starts_with(&prefix));
        Ok(InvalidateResponse {
            invalidated: self.cache.borrow().len() != before,
        })
    }
}

fn selected_scopes(resource: &ResourceConfig, requested: &[String]) -> Option<Vec<String>> {
    let selected = if requested.is_empty() {
        resource.scopes.clone()
    } else {
        if requested
            .iter()
            .any(|scope| !resource.scopes.contains(scope))
        {
            return None;
        }
        requested.to_vec()
    };
    let mut selected = selected;
    selected.sort();
    selected.dedup();
    Some(selected)
}

fn cache_key(resource_uri: &str, scopes: &[String]) -> String {
    format!("{resource_uri}\n{}", scopes.join(" "))
}

fn access_response(token: &CachedToken) -> AccessResponse {
    AccessResponse {
        access_token: token.access_token.clone(),
        token_type: token.token_type.clone(),
        expires_at_millis: token.expires_at_millis.to_string(),
        scopes: token.scopes.clone(),
    }
}

#[derive(Debug)]
enum OAuthFailure {
    Discovery,
    Rejected,
    Runtime(RuntimeFailure),
}

#[derive(serde::Deserialize)]
struct ProtectedResourceMetadata {
    resource: String,
    authorization_servers: Vec<String>,
}

#[derive(serde::Deserialize)]
struct AuthorizationServerMetadata {
    issuer: String,
    token_endpoint: String,
    #[serde(default)]
    grant_types_supported: Vec<String>,
}

#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    token_type: String,
    expires_in: u64,
    #[serde(default)]
    scope: Option<String>,
}

async fn discover_and_request_token(
    client: &reqwest::Client,
    resource: &ResourceConfig,
    scopes: &[String],
    client_id: &str,
    client_secret: &str,
    timeout_ms: u64,
) -> Result<CachedToken, OAuthFailure> {
    let protected = discover_protected_resource(client, resource, timeout_ms).await?;
    if protected.resource != resource.resource_uri || protected.authorization_servers.is_empty() {
        return Err(OAuthFailure::Discovery);
    }
    let authorization_server = match &resource.authorization_server {
        Some(expected) if protected.authorization_servers.contains(expected) => expected.clone(),
        None if protected.authorization_servers.len() == 1 => {
            protected.authorization_servers[0].clone()
        }
        Some(_) | None => return Err(OAuthFailure::Discovery),
    };
    if !safe_service_url(&authorization_server) {
        return Err(OAuthFailure::Discovery);
    }
    let metadata_url = authorization_metadata_url(&authorization_server)?;
    let metadata: AuthorizationServerMetadata =
        get_json(client, metadata_url, timeout_ms, MAX_METADATA_BYTES).await?;
    if metadata.issuer != authorization_server
        || !safe_service_url(&metadata.token_endpoint)
        || (!metadata.grant_types_supported.is_empty()
            && !metadata
                .grant_types_supported
                .iter()
                .any(|grant| grant == "client_credentials"))
    {
        return Err(OAuthFailure::Discovery);
    }
    let token_endpoint =
        Url::parse(&metadata.token_endpoint).map_err(|_| OAuthFailure::Discovery)?;
    let scope = scopes.join(" ");
    let mut form = vec![
        ("grant_type", "client_credentials".to_owned()),
        ("resource", resource.resource_uri.clone()),
    ];
    if !scope.is_empty() {
        form.push(("scope", scope));
    }
    let mut request = client
        .post(token_endpoint)
        .header(reqwest::header::ACCEPT, "application/json")
        .timeout(Duration::from_millis(timeout_ms));
    match resource.client_authentication_method {
        ClientAuthenticationMethod::Basic => {
            request = request.basic_auth(client_id, Some(client_secret));
        }
        ClientAuthenticationMethod::Post => {
            form.push(("client_id", client_id.to_owned()));
            form.push(("client_secret", client_secret.to_owned()));
        }
    }
    let response = request
        .form(&form)
        .send()
        .await
        .map_err(|_| OAuthFailure::Runtime(auth_failure("OAuth token request failed")))?;
    if !response.status().is_success() {
        return Err(OAuthFailure::Rejected);
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| OAuthFailure::Runtime(auth_failure("OAuth token response failed")))?;
    if bytes.len() > MAX_TOKEN_BYTES {
        return Err(OAuthFailure::Runtime(auth_failure(
            "OAuth token response exceeded the byte limit",
        )));
    }
    let token =
        serde_json::from_slice::<TokenResponse>(&bytes).map_err(|_| OAuthFailure::Rejected)?;
    if token.access_token.is_empty()
        || token.access_token.len() > 16_384
        || !token.token_type.eq_ignore_ascii_case("bearer")
        || !(1..=86_400).contains(&token.expires_in)
    {
        return Err(OAuthFailure::Rejected);
    }
    let returned_scopes = token.scope.map_or_else(
        || scopes.to_vec(),
        |value| value.split_ascii_whitespace().map(str::to_owned).collect(),
    );
    if returned_scopes.iter().any(|scope| !valid_scope(scope)) {
        return Err(OAuthFailure::Rejected);
    }
    Ok(CachedToken {
        access_token: token.access_token,
        token_type: "Bearer".to_owned(),
        expires_at_millis: now_millis().saturating_add(token.expires_in.saturating_mul(1_000)),
        scopes: returned_scopes,
    })
}

async fn discover_protected_resource(
    client: &reqwest::Client,
    resource: &ResourceConfig,
    timeout_ms: u64,
) -> Result<ProtectedResourceMetadata, OAuthFailure> {
    if let Some(url) = &resource.protected_resource_metadata_url {
        return get_json(client, parse_safe_url(url)?, timeout_ms, MAX_METADATA_BYTES).await;
    }
    let resource_url = parse_safe_url(&resource.resource_uri)?;
    let mut candidates = Vec::new();
    let path = resource_url.path().trim_start_matches('/');
    if !path.is_empty() {
        let mut path_specific = resource_url.clone();
        path_specific.set_path(&format!("/.well-known/oauth-protected-resource/{path}"));
        path_specific.set_query(None);
        candidates.push(path_specific);
    }
    let mut root = resource_url;
    root.set_path("/.well-known/oauth-protected-resource");
    root.set_query(None);
    candidates.push(root);
    for candidate in candidates {
        match get_json(client, candidate, timeout_ms, MAX_METADATA_BYTES).await {
            Ok(metadata) => return Ok(metadata),
            Err(OAuthFailure::Discovery) => {}
            Err(error) => return Err(error),
        }
    }
    Err(OAuthFailure::Discovery)
}

async fn get_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: Url,
    timeout_ms: u64,
    max_bytes: usize,
) -> Result<T, OAuthFailure> {
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .timeout(Duration::from_millis(timeout_ms))
        .send()
        .await
        .map_err(|_| OAuthFailure::Runtime(auth_failure("OAuth discovery request failed")))?;
    if !response.status().is_success() {
        return Err(OAuthFailure::Discovery);
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| OAuthFailure::Runtime(auth_failure("OAuth discovery response failed")))?;
    if bytes.len() > max_bytes {
        return Err(OAuthFailure::Runtime(auth_failure(
            "OAuth discovery response exceeded the byte limit",
        )));
    }
    serde_json::from_slice(&bytes).map_err(|_| OAuthFailure::Discovery)
}

fn authorization_metadata_url(issuer: &str) -> Result<Url, OAuthFailure> {
    let issuer = parse_safe_url(issuer)?;
    let path = issuer.path().trim_matches('/');
    let mut metadata = issuer.clone();
    let metadata_path = if path.is_empty() {
        "/.well-known/oauth-authorization-server".to_owned()
    } else {
        format!("/.well-known/oauth-authorization-server/{path}")
    };
    metadata.set_path(&metadata_path);
    metadata.set_query(None);
    Ok(metadata)
}

fn parse_safe_url(value: &str) -> Result<Url, OAuthFailure> {
    if !safe_service_url(value) {
        return Err(OAuthFailure::Discovery);
    }
    Url::parse(value).map_err(|_| OAuthFailure::Discovery)
}

fn safe_service_url(value: &str) -> bool {
    Url::parse(value).is_ok_and(|url| {
        (url.scheme() == "https"
            || (url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "::1"))))
            && url.username().is_empty()
            && url.password().is_none()
            && url.fragment().is_none()
    })
}

fn valid_environment_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    value.len() <= 128
        && bytes
            .next()
            .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn valid_scope(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_graphic() && byte != b'"' && byte != b'\\')
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn invalid_plan(detail: impl Into<String>) -> RuntimeFailure {
    RuntimeFailure::InvalidResolvedPlan {
        detail: detail.into(),
    }
}

fn auth_failure(detail: impl Into<String>) -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::{Router, body::Bytes, extract::State, http::HeaderMap, routing::get};

    use super::*;

    #[derive(Clone)]
    struct TestState {
        origin: String,
        token_requests: Arc<Mutex<Vec<String>>>,
    }

    async fn protected(State(state): State<TestState>) -> axum::Json<serde_json::Value> {
        axum::Json(serde_json::json!({
            "resource": format!("{}/mcp", state.origin),
            "authorization_servers": [state.origin]
        }))
    }

    async fn metadata(State(state): State<TestState>) -> axum::Json<serde_json::Value> {
        axum::Json(serde_json::json!({
            "issuer": state.origin,
            "token_endpoint": format!("{}/token", state.origin),
            "grant_types_supported": ["client_credentials"]
        }))
    }

    async fn token(
        State(state): State<TestState>,
        headers: HeaderMap,
        body: Bytes,
    ) -> axum::Json<serde_json::Value> {
        state.token_requests.lock().unwrap().push(format!(
            "{} {}",
            headers
                .get(reqwest::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default(),
            String::from_utf8_lossy(&body)
        ));
        axum::Json(serde_json::json!({
            "access_token": "short-lived-token",
            "token_type": "Bearer",
            "expires_in": 300,
            "scope": "tools.read"
        }))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn discovers_metadata_and_binds_client_credentials_to_the_resource() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let state = TestState {
            origin: origin.clone(),
            token_requests: Arc::clone(&requests),
        };
        let app = Router::new()
            .route("/.well-known/oauth-protected-resource/mcp", get(protected))
            .route("/.well-known/oauth-authorization-server", get(metadata))
            .route("/token", axum::routing::post(token))
            .with_state(state);
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let resource = ResourceConfig {
            resource_uri: format!("{origin}/mcp"),
            client_id_environment: "TEST_ID".to_owned(),
            client_secret_environment: "TEST_SECRET".to_owned(),
            scopes: vec!["tools.read".to_owned()],
            protected_resource_metadata_url: None,
            authorization_server: None,
            client_authentication_method: ClientAuthenticationMethod::Basic,
        };
        let token = discover_and_request_token(
            &reqwest::Client::new(),
            &resource,
            &resource.scopes,
            "client-id",
            "client-secret",
            5_000,
        )
        .await
        .unwrap();
        assert_eq!(token.access_token, "short-lived-token");
        let request = requests.lock().unwrap().pop().unwrap();
        assert!(request.starts_with("Basic "));
        assert!(request.contains("resource=http%3A%2F%2F127.0.0.1"));
        assert!(request.contains("scope=tools.read"));
        server.abort();
    }

    #[test]
    fn rejects_credential_urls_and_scope_escalation() {
        assert!(!safe_service_url("https://secret@example.com/mcp"));
        let resource = ResourceConfig {
            resource_uri: "https://example.com/mcp".to_owned(),
            client_id_environment: "ID".to_owned(),
            client_secret_environment: "SECRET".to_owned(),
            scopes: vec!["tools.read".to_owned()],
            protected_resource_metadata_url: None,
            authorization_server: None,
            client_authentication_method: ClientAuthenticationMethod::Post,
        };
        assert!(selected_scopes(&resource, &["tools.write".to_owned()]).is_none());
    }
}
