//! One Host-selected remote Management edge; authentication and policy stay at the application.
use std::{cell::RefCell, fmt, fs, path::Path, rc::Rc, time::Duration};

use lenso::{CtxExt, Lifecycle};
use lenso_capability_agent::AgentTaskBinding;
use lenso_capability_management as management;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use serde::Deserialize;
use zeroize::Zeroizing;

const MAX_BODY_BYTES: usize = 2_097_152;

#[derive(Clone, Debug, Deserialize, lenso::PluginConfig)]
#[serde(deny_unknown_fields)]
struct ConnectionConfig {
    origin: String,
    #[serde(default)]
    delegated_route_prefix: String,
    deployment: String,
    credential_file: String,
    task_id: String,
    agent_session_id: String,
    delegate_caller: String,
    request_timeout_millis: u64,
}

impl ConnectionConfig {
    fn binding(&self) -> AgentTaskBinding {
        AgentTaskBinding {
            task_id: self.task_id.clone(),
            agent_session_id: self.agent_session_id.clone(),
            delegate_caller: self.delegate_caller.clone(),
        }
    }
}

fn validate(config: &ConnectionConfig) -> Result<(), lenso::RuntimeFailure> {
    let origin = reqwest::Url::parse(&config.origin).map_err(|_| unavailable())?;
    if origin.origin().ascii_serialization() != config.origin
        || !origin.username().is_empty()
        || origin.password().is_some()
        || origin.query().is_some()
        || origin.fragment().is_some()
        || !(origin.scheme() == "https"
            || origin.scheme() == "http"
                && matches!(origin.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")))
        || config.deployment.is_empty()
        || config.deployment.len() > 128
        || !matches!(config.delegated_route_prefix.as_str(), "" | "/agent")
        || !Path::new(&config.credential_file).is_absolute()
        || !(1..=60_000).contains(&config.request_timeout_millis)
        || config.binding().validate().is_err()
    {
        return Err(unavailable());
    }
    Ok(())
}

#[lenso::plugin(lifecycle, validate = validate)]
#[derive(Clone, Debug)]
struct ManagementConnection {
    #[config]
    config: ConnectionConfig,
    state: Rc<RefCell<Option<Rc<Connection>>>>,
}

struct Connection {
    client: reqwest::Client,
    credential: Zeroizing<String>,
}

impl fmt::Debug for Connection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Connection { credential: [redacted] }")
    }
}

fn unavailable() -> lenso::RuntimeFailure {
    lenso::RuntimeFailure::PluginFailure {
        detail: "selected management delegation is unavailable".into(),
    }
}

impl ManagementConnection {
    fn prepare_connection(&self) -> Result<(), lenso::RuntimeFailure> {
        let metadata =
            fs::symlink_metadata(&self.config.credential_file).map_err(|_| unavailable())?;
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 1024 {
            return Err(unavailable());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(unavailable());
            }
        }
        let credential = Zeroizing::new(
            fs::read_to_string(&self.config.credential_file).map_err(|_| unavailable())?,
        );
        if credential.is_empty()
            || !credential
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(unavailable());
        }
        let mut headers = HeaderMap::new();
        let mut bearer = HeaderValue::from_str(&format!("Bearer {}", credential.as_str()))
            .map_err(|_| unavailable())?;
        bearer.set_sensitive(true);
        headers.insert(AUTHORIZATION, bearer);
        for (name, value) in [
            ("x-lenso-task-id", self.config.task_id.as_str()),
            (
                "x-lenso-agent-session-id",
                self.config.agent_session_id.as_str(),
            ),
            (
                "x-lenso-delegate-caller",
                self.config.delegate_caller.as_str(),
            ),
        ] {
            headers.insert(
                name,
                HeaderValue::from_str(value).map_err(|_| unavailable())?,
            );
        }
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(Duration::from_millis(self.config.request_timeout_millis))
            .build()
            .map_err(|_| unavailable())?;
        self.state
            .replace(Some(Rc::new(Connection { client, credential })));
        Ok(())
    }
}

impl Lifecycle for ManagementConnection {
    fn prepare(
        &self,
        _: lenso::PrepareContext,
    ) -> impl std::future::Future<Output = Result<(), lenso::RuntimeFailure>> {
        std::future::ready(self.prepare_connection())
    }

    fn deactivate(
        &self,
        _: lenso::DeactivateContext,
    ) -> impl std::future::Future<Output = Result<(), lenso::RuntimeFailure>> {
        self.state.borrow_mut().take();
        std::future::ready(Ok(()))
    }
}

#[derive(Clone, Copy, Debug)]
enum Failure {
    Denied,
    NotFound,
    Conflict,
    InvalidInput,
    Unavailable,
    Uncertain,
}

enum Route<'a> {
    Catalog,
    Invoke,
    Status(&'a str),
}

impl Route<'_> {
    fn endpoint(&self, origin: &str, prefix: &str) -> Result<reqwest::Url, Failure> {
        if !matches!(prefix, "" | "/agent") {
            return Err(Failure::InvalidInput);
        }
        let mut endpoint = reqwest::Url::parse(origin).map_err(|_| Failure::Unavailable)?;
        {
            let mut segments = endpoint
                .path_segments_mut()
                .map_err(|()| Failure::Unavailable)?;
            segments.clear();
            if prefix == "/agent" {
                segments.push("agent");
            }
            segments.push("management");
            match self {
                Self::Catalog => {
                    segments.push("catalog");
                }
                Self::Invoke => {
                    segments.push("invoke");
                }
                Self::Status(id) => {
                    if id.is_empty()
                        || id.len() > 128
                        || matches!(*id, "." | "..")
                        || !id.bytes().all(|byte| {
                            byte.is_ascii_alphanumeric()
                                || matches!(byte, b'-' | b'_' | b'.' | b':')
                        })
                    {
                        return Err(Failure::InvalidInput);
                    }
                    segments.push("operations").push(id);
                }
            }
        }
        Ok(endpoint)
    }
}

impl ManagementConnection {
    async fn request(
        &self,
        context: &lenso::Ctx,
        route: Route<'_>,
        body: Option<String>,
    ) -> Result<String, Failure> {
        let binding = context
            .typed_extension::<AgentTaskBinding>()
            .map_err(|_| Failure::Denied)?
            .ok_or(Failure::Denied)?;
        if binding != self.config.binding() || binding.validate().is_err() || context.is_cancelled()
        {
            return Err(Failure::Denied);
        }
        let connection = self.state.borrow().clone().ok_or(Failure::Unavailable)?;
        let endpoint = route.endpoint(&self.config.origin, &self.config.delegated_route_prefix)?;
        let request = body.map_or_else(
            || connection.client.get(endpoint.clone()),
            |body| {
                connection
                    .client
                    .post(endpoint.clone())
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(body)
            },
        );
        let cancellation = context.cancellation();
        tokio::select! {
            () = cancellation.cancelled() => Err(Failure::Uncertain),
            result = async {
                let mut response = request.send().await.map_err(|_| Failure::Uncertain)?;
                if !response.status().is_success() {
                    return Err(match response.status().as_u16() {
                        401 | 403 => Failure::Denied, 404 => Failure::NotFound,
                        409 => Failure::Conflict, 400 | 422 => Failure::InvalidInput,
                        _ => Failure::Uncertain,
                    });
                }
                let mut bytes = Zeroizing::new(Vec::new());
                while let Some(chunk) = response.chunk().await.map_err(|_| Failure::Uncertain)? {
                    if bytes.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
                        return Err(Failure::Uncertain);
                    }
                    bytes.extend_from_slice(&chunk);
                }
                if bytes.windows(connection.credential.len())
                    .any(|window| window == connection.credential.as_bytes())
                {
                    return Err(Failure::Uncertain);
                }
                String::from_utf8(bytes.to_vec()).map_err(|_| Failure::Uncertain)
            } => result,
        }
    }
}

#[lenso::provides(management::Management)]
impl ManagementConnection {
    async fn catalog(
        &self,
        context: lenso::Ctx,
        _: management::CatalogRequest,
    ) -> Result<management::CatalogResponse, management::CatalogError> {
        let wire = self
            .request(&context, Route::Catalog, None)
            .await
            .map_err(|failure| {
                if matches!(failure, Failure::Denied) {
                    management::CatalogError::PermissionDenied
                } else {
                    management::CatalogError::Unavailable
                }
            })?;
        let response = management::decode_catalog_response(&wire)
            .map_err(|_| management::CatalogError::Unavailable)?;
        if response.deployment != self.config.deployment {
            return Err(management::CatalogError::PermissionDenied);
        }
        Ok(response)
    }

    async fn invoke(
        &self,
        context: lenso::Ctx,
        request: management::InvokeRequest,
    ) -> Result<management::InvokeResponse, management::InvokeError> {
        let body = management::encode_invoke_request(&request)
            .map_err(|_| management::InvokeError::InvalidInput)?;
        match self.request(&context, Route::Invoke, Some(body)).await {
            Ok(wire) => {
                Ok(management::decode_invoke_response(&wire).unwrap_or_else(|_| uncertain()))
            }
            Err(Failure::Uncertain) => Ok(uncertain()),
            Err(failure) => Err(match failure {
                Failure::Denied => management::InvokeError::PermissionDenied,
                Failure::NotFound => management::InvokeError::NotFound,
                Failure::Conflict => management::InvokeError::Conflict,
                Failure::InvalidInput => management::InvokeError::InvalidInput,
                _ => management::InvokeError::Unavailable,
            }),
        }
    }

    async fn status(
        &self,
        context: lenso::Ctx,
        request: management::StatusRequest,
    ) -> Result<management::InvokeResponse, management::StatusError> {
        let wire = self
            .request(&context, Route::Status(&request.operation_id), None)
            .await
            .map_err(|failure| match failure {
                Failure::Denied => management::StatusError::PermissionDenied,
                Failure::NotFound => management::StatusError::NotFound,
                Failure::Conflict | Failure::InvalidInput => management::StatusError::Conflict,
                _ => management::StatusError::Unavailable,
            })?;
        management::decode_status_response(&wire).map_err(|_| management::StatusError::Unavailable)
    }
}

fn uncertain() -> management::InvokeResponse {
    management::InvokeResponse {
        operation_id: None,
        state: management::InvocationState::Unknown,
        audit_pending: true,
        result_json: None,
        receipt: None,
    }
}

#[cfg(test)]
mod tests;
