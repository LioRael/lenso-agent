//! `ChatGPT` OAuth credential owner for direct Codex model access.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use directories::BaseDirs;
use fs2::FileExt;
use futures::future::LocalBoxFuture;
use lenso_capability_agent_auth_openai_codex::{
    self as auth_contract, AccessError, AccessRequest, AccessResponse, OpenaiCodexProvider,
};
use lenso_kernel::{InvocationContext, RuntimeFailure};
use sha2::{Digest as _, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

/// Public OAuth client used by Codex-compatible clients.
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const DEFAULT_ISSUER: &str = "https://auth.openai.com";
const DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
const BROWSER_CALLBACK_PATH: &str = "/auth/callback";
const DEFAULT_BROWSER_CALLBACK_PORT: u16 = 1455;
const BROWSER_LOGIN_TIMEOUT: Duration = Duration::from_mins(5);
const DEFAULT_REFRESH_MARGIN_SECONDS: u64 = 60;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthConfig {
    issuer: String,
    profile: String,
    #[serde(default)]
    credential_file: Option<PathBuf>,
    #[serde(default = "default_refresh_margin_seconds")]
    refresh_margin_seconds: u64,
}

fn default_refresh_margin_seconds() -> u64 {
    DEFAULT_REFRESH_MARGIN_SECONDS
}

impl AuthConfig {
    fn validate(self) -> Result<Self, RuntimeFailure> {
        validate_issuer(&self.issuer)?;
        if self.profile.is_empty()
            || !self.profile.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            })
        {
            return Err(invalid_plan("OpenAI Codex Auth profile is invalid"));
        }
        if self.refresh_margin_seconds > 600 {
            return Err(invalid_plan(
                "OpenAI Codex Auth refresh margin must not exceed 600 seconds",
            ));
        }
        Ok(self)
    }

    fn credential_path(&self) -> Result<PathBuf, RuntimeFailure> {
        if let Some(path) = &self.credential_file {
            return Ok(path.clone());
        }
        let base = BaseDirs::new()
            .ok_or_else(|| plugin_failure("the user home directory is unavailable"))?;
        Ok(base.home_dir().join(".lenso/agent/auth.json"))
    }

    fn credential_key(&self) -> String {
        if self.profile == "default" {
            "openai-codex".to_owned()
        } else {
            format!("openai-codex:{}", self.profile)
        }
    }
}

/// Options used by the CLI authoring surface.
#[derive(Clone, Debug)]
pub struct DirectAuthOptions {
    /// OAuth issuer. Production callers should use `https://auth.openai.com`.
    pub issuer: String,
    /// App-local credential profile.
    pub profile: String,
    /// Optional explicit credential path, primarily for isolated tests.
    pub credential_file: Option<PathBuf>,
    /// Browser callback port. Port zero selects an ephemeral port for tests.
    pub callback_port: u16,
}

impl Default for DirectAuthOptions {
    fn default() -> Self {
        Self {
            issuer: DEFAULT_ISSUER.to_owned(),
            profile: "default".to_owned(),
            credential_file: None,
            callback_port: DEFAULT_BROWSER_CALLBACK_PORT,
        }
    }
}

impl DirectAuthOptions {
    fn into_config(self) -> Result<AuthConfig, RuntimeFailure> {
        AuthConfig {
            issuer: self.issuer,
            profile: self.profile,
            credential_file: self.credential_file,
            refresh_margin_seconds: DEFAULT_REFRESH_MARGIN_SECONDS,
        }
        .validate()
    }
}

/// Information the user needs to complete a device-code login.
#[derive(Clone, Debug)]
pub struct PendingDeviceLogin {
    /// URL the user opens in a browser.
    pub verification_url: String,
    /// One-time code entered by the user.
    pub user_code: String,
    device_auth_id: String,
    interval: Duration,
}

/// Pending browser PKCE login with a loopback callback listener.
pub struct PendingBrowserLogin {
    /// Authorization URL to open in the user's browser.
    pub authorization_url: String,
    listener: TcpListener,
    verifier: String,
    state: String,
    redirect_uri: String,
}

impl std::fmt::Debug for PendingBrowserLogin {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingBrowserLogin")
            .field("authorization_url", &"<redacted>")
            .field("verifier", &"<redacted>")
            .field("state", &"<redacted>")
            .field("redirect_uri", &self.redirect_uri)
            .finish_non_exhaustive()
    }
}

/// Non-secret status for one direct-auth profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectAuthStatus {
    /// Whether stored credentials exist.
    pub authenticated: bool,
    /// Stored token expiry as Unix milliseconds when authenticated.
    pub expires_at: Option<u64>,
}

fn validate_config(config: &AuthConfig) -> Result<(), RuntimeFailure> {
    config.clone().validate().map(|_| ())
}

#[lenso::plugin(configuration_schema = "config.schema.json", validate = validate_config)]
#[derive(Clone, Debug)]
struct CodexAuth {
    #[config]
    config: AuthConfig,
    client: reqwest::Client,
}

#[lenso::provides(auth_contract::OpenaiCodex)]
impl OpenaiCodexProvider for CodexAuth {
    fn access(
        &self,
        _context: InvocationContext,
        _request: AccessRequest,
    ) -> LocalBoxFuture<'static, Result<Result<AccessResponse, AccessError>, RuntimeFailure>> {
        let config = self.config.clone();
        let client = self.client.clone();
        Box::pin(async move {
            let path = config.credential_path()?;
            let _lock = CredentialLock::acquire(&path)
                .map_err(|_| plugin_failure("direct-auth credential store is busy"))?;
            let key = config.credential_key();
            let mut credentials = match read_credentials(&path) {
                Ok(credentials) => credentials,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    return Ok(Err(AccessError::NotAuthenticated));
                }
                Err(_) => return Err(plugin_failure("failed to read direct-auth credentials")),
            };
            let Some(mut credential) = credential_from_store(&credentials, &key)
                .map_err(|_| plugin_failure("direct-auth credential is invalid"))?
            else {
                return Ok(Err(AccessError::NotAuthenticated));
            };
            let refresh_at = now_millis().saturating_add(config.refresh_margin_seconds * 1_000);
            if credential.expires_at <= refresh_at {
                credential = match refresh(&client, &config, &credential).await {
                    Ok(credential) => credential,
                    Err(RefreshFailure::Rejected) => {
                        return Ok(Err(AccessError::RefreshRejected));
                    }
                    Err(RefreshFailure::Runtime) => {
                        return Err(plugin_failure("ChatGPT token refresh failed"));
                    }
                };
                credentials.insert(
                    key,
                    serde_json::to_value(&credential)
                        .map_err(|_| plugin_failure("failed to encode refreshed credentials"))?,
                );
                write_credentials(&path, &credentials)
                    .map_err(|_| plugin_failure("failed to persist refreshed credentials"))?;
            }
            Ok(Ok(credential.access_response()))
        })
    }
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredCredential {
    #[serde(rename = "type")]
    credential_type: String,
    #[serde(rename = "access")]
    access_token: String,
    #[serde(rename = "refresh")]
    refresh_token: String,
    #[serde(rename = "accountId")]
    account_id: String,
    #[serde(rename = "expires")]
    expires_at: u64,
}

impl std::fmt::Debug for StoredCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StoredCredential")
            .field("credential_type", &self.credential_type)
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("account_id", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl StoredCredential {
    fn access_response(&self) -> AccessResponse {
        AccessResponse {
            access_token: self.access_token.clone(),
            account_id: self.account_id.clone(),
            expires_at: self.expires_at.to_string(),
        }
    }
}

#[derive(Debug)]
struct CredentialLock(File);

impl CredentialLock {
    fn acquire(credential_path: &Path) -> io::Result<Self> {
        let parent = credential_path
            .parent()
            .ok_or_else(|| io::Error::other("credential path has no parent"))?;
        secure_create_dir_all(parent)?;
        let file = secure_open(&credential_path.with_extension("lock"), false)?;
        file.try_lock_exclusive()?;
        Ok(Self(file))
    }
}

impl Drop for CredentialLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Starts browser OAuth with PKCE and a loopback callback, matching Pi's default flow.
pub async fn begin_browser_login(
    options: DirectAuthOptions,
) -> Result<PendingBrowserLogin, String> {
    let config = options
        .clone()
        .into_config()
        .map_err(|error| runtime_text(&error))?;
    let listener = TcpListener::bind(("127.0.0.1", options.callback_port))
        .await
        .map_err(|_| {
            format!(
                "failed to bind the browser OAuth callback on port {}; use --device-auth for a headless login",
                options.callback_port
            )
        })?;
    let callback_port = listener
        .local_addr()
        .map_err(|_| "failed to inspect the browser OAuth callback".to_owned())?
        .port();
    let redirect_uri = format!("http://localhost:{callback_port}{BROWSER_CALLBACK_PATH}");
    let verifier = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = uuid::Uuid::new_v4().simple().to_string();
    let mut authorization_url = reqwest::Url::parse(&format!(
        "{}/oauth/authorize",
        config.issuer.trim_end_matches('/')
    ))
    .map_err(|_| "failed to construct the browser OAuth URL".to_owned())?;
    authorization_url
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("scope", "openid profile email offline_access")
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state)
        .append_pair("id_token_add_organizations", "true")
        .append_pair("codex_cli_simplified_flow", "true")
        .append_pair("originator", "lenso");
    Ok(PendingBrowserLogin {
        authorization_url: authorization_url.into(),
        listener,
        verifier,
        state,
        redirect_uri,
    })
}

/// Waits for the browser callback, exchanges its code, and persists the profile.
pub async fn complete_browser_login(
    options: DirectAuthOptions,
    pending: PendingBrowserLogin,
) -> Result<DirectAuthStatus, String> {
    let config = options
        .into_config()
        .map_err(|error| runtime_text(&error))?;
    let code = tokio::time::timeout(
        BROWSER_LOGIN_TIMEOUT,
        wait_for_browser_code(&pending.listener, &pending.state),
    )
    .await
    .map_err(|_| "browser OAuth callback timed out; run login again".to_owned())??;
    let response = reqwest::Client::new()
        .post(format!(
            "{}/oauth/token",
            config.issuer.trim_end_matches('/')
        ))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", pending.redirect_uri.as_str()),
            ("client_id", CLIENT_ID),
            ("code_verifier", pending.verifier.as_str()),
        ])
        .send()
        .await
        .map_err(|_| "failed to exchange the browser authorization code".to_owned())?;
    let tokens = read_token_response(response, None)
        .await
        .map_err(|_| "ChatGPT rejected the browser token exchange".to_owned())?;
    persist_credential(&config, tokens)
}

async fn wait_for_browser_code(
    listener: &TcpListener,
    expected_state: &str,
) -> Result<String, String> {
    loop {
        let (mut stream, _) = listener
            .accept()
            .await
            .map_err(|_| "failed to accept the browser OAuth callback".to_owned())?;
        let mut request = Vec::new();
        loop {
            let mut buffer = [0_u8; 2048];
            let read = stream
                .read(&mut buffer)
                .await
                .map_err(|_| "failed to read the browser OAuth callback".to_owned())?;
            if read == 0 || request.len().saturating_add(read) > 16 * 1024 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
            if request.windows(4).any(|part| part == b"\r\n\r\n") {
                break;
            }
        }
        let target = std::str::from_utf8(&request)
            .ok()
            .and_then(|request| request.lines().next())
            .and_then(|line| line.split_whitespace().nth(1));
        let Some(callback) = target
            .and_then(|target| reqwest::Url::parse(&format!("http://localhost{target}")).ok())
        else {
            write_callback_response(&mut stream, 400, "Invalid OAuth callback.").await;
            continue;
        };
        if callback.path() != BROWSER_CALLBACK_PATH {
            write_callback_response(&mut stream, 404, "OAuth callback route not found.").await;
            continue;
        }
        let parameters = callback.query_pairs().collect::<BTreeMap<_, _>>();
        if parameters.get("state").map(std::convert::AsRef::as_ref) != Some(expected_state) {
            write_callback_response(&mut stream, 400, "OAuth state mismatch.").await;
            continue;
        }
        let Some(code) = parameters
            .get("code")
            .map(std::string::ToString::to_string)
            .filter(|code| !code.is_empty())
        else {
            write_callback_response(&mut stream, 400, "Authorization code is missing.").await;
            continue;
        };
        write_callback_response(
            &mut stream,
            200,
            "Lenso authentication completed. You can close this window.",
        )
        .await;
        return Ok(code);
    }
}

async fn write_callback_response(stream: &mut tokio::net::TcpStream, status: u16, message: &str) {
    let reason = if status == 200 { "OK" } else { "Bad Request" };
    let body = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>Lenso authentication</title><p>{message}</p>"
    );
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Starts direct `ChatGPT` device authentication without invoking Codex CLI.
pub async fn begin_device_login(options: DirectAuthOptions) -> Result<PendingDeviceLogin, String> {
    let config = options
        .into_config()
        .map_err(|error| runtime_text(&error))?;
    let response = reqwest::Client::new()
        .post(format!(
            "{}/api/accounts/deviceauth/usercode",
            config.issuer.trim_end_matches('/')
        ))
        .header("User-Agent", "lenso-agent/0.1.0")
        .json(&serde_json::json!({ "client_id": CLIENT_ID }))
        .send()
        .await
        .map_err(|_| "failed to initiate ChatGPT device authentication".to_owned())?;
    if !response.status().is_success() {
        return Err("ChatGPT rejected device authentication initiation".to_owned());
    }
    let body = response
        .json::<DeviceCodeResponse>()
        .await
        .map_err(|_| "ChatGPT returned an invalid device authentication response".to_owned())?;
    let interval = body.interval.parse::<u64>().unwrap_or(5).max(1);
    Ok(PendingDeviceLogin {
        verification_url: format!("{}/codex/device", config.issuer.trim_end_matches('/')),
        user_code: body.user_code,
        device_auth_id: body.device_auth_id,
        interval: Duration::from_secs(interval + 3),
    })
}

/// Polls a pending device login and stores the resulting refresh credential.
pub async fn complete_device_login(
    options: DirectAuthOptions,
    pending: PendingDeviceLogin,
) -> Result<DirectAuthStatus, String> {
    let config = options
        .into_config()
        .map_err(|error| runtime_text(&error))?;
    let client = reqwest::Client::new();
    let authorization = loop {
        let response = client
            .post(format!(
                "{}/api/accounts/deviceauth/token",
                config.issuer.trim_end_matches('/')
            ))
            .header("User-Agent", "lenso-agent/0.1.0")
            .json(&serde_json::json!({
                "device_auth_id": pending.device_auth_id,
                "user_code": pending.user_code
            }))
            .send()
            .await
            .map_err(|_| "failed while polling ChatGPT device authentication".to_owned())?;
        if response.status().is_success() {
            break response
                .json::<DeviceAuthorizationResponse>()
                .await
                .map_err(|_| "ChatGPT returned an invalid device authorization".to_owned())?;
        }
        if !matches!(response.status().as_u16(), 403 | 404) {
            return Err("ChatGPT rejected device authentication".to_owned());
        }
        tokio::time::sleep(pending.interval).await;
    };
    let response = client
        .post(format!(
            "{}/oauth/token",
            config.issuer.trim_end_matches('/')
        ))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", authorization.authorization_code.as_str()),
            ("redirect_uri", DEVICE_REDIRECT_URI),
            ("client_id", CLIENT_ID),
            ("code_verifier", authorization.code_verifier.as_str()),
        ])
        .send()
        .await
        .map_err(|_| "failed to exchange the ChatGPT authorization code".to_owned())?;
    let tokens = read_token_response(response, None)
        .await
        .map_err(|_| "ChatGPT rejected the token exchange".to_owned())?;
    persist_credential(&config, tokens)
}

/// Reads non-secret status for the direct-auth profile.
pub fn direct_auth_status(options: DirectAuthOptions) -> Result<DirectAuthStatus, String> {
    let config = options
        .into_config()
        .map_err(|error| runtime_text(&error))?;
    let path = config
        .credential_path()
        .map_err(|error| runtime_text(&error))?;
    match read_credentials(&path) {
        Ok(credentials) => match credential_from_store(&credentials, &config.credential_key()) {
            Err(_) => Err("direct-auth credential is invalid".to_owned()),
            Ok(None) => Ok(DirectAuthStatus {
                authenticated: false,
                expires_at: None,
            }),
            Ok(Some(credential)) => Ok(DirectAuthStatus {
                authenticated: true,
                expires_at: Some(credential.expires_at),
            }),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(DirectAuthStatus {
            authenticated: false,
            expires_at: None,
        }),
        Err(_) => Err("failed to read direct-auth status".to_owned()),
    }
}

/// Removes the direct-auth profile without touching Codex CLI credentials.
pub fn direct_logout(options: DirectAuthOptions) -> Result<(), String> {
    let config = options
        .into_config()
        .map_err(|error| runtime_text(&error))?;
    let path = config
        .credential_path()
        .map_err(|error| runtime_text(&error))?;
    let _lock = CredentialLock::acquire(&path)
        .map_err(|_| "failed to lock the direct-auth credential store".to_owned())?;
    let mut credentials = match read_credentials(&path) {
        Ok(credentials) => credentials,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("failed to read direct-auth credentials".to_owned()),
    };
    credentials.remove(&config.credential_key());
    if credentials.is_empty() {
        fs::remove_file(path).map_err(|_| "failed to remove direct-auth credentials".to_owned())
    } else {
        write_credentials(&path, &credentials)
            .map_err(|_| "failed to persist direct-auth logout".to_owned())
    }
}

#[derive(Debug, serde::Deserialize)]
struct DeviceCodeResponse {
    device_auth_id: String,
    user_code: String,
    interval: String,
}

#[derive(Debug, serde::Deserialize)]
struct DeviceAuthorizationResponse {
    authorization_code: String,
    code_verifier: String,
}

#[derive(Debug, serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_in: Option<u64>,
}

#[derive(Clone, Copy, Debug)]
enum RefreshFailure {
    Rejected,
    Runtime,
}

async fn refresh(
    client: &reqwest::Client,
    config: &AuthConfig,
    current: &StoredCredential,
) -> Result<StoredCredential, RefreshFailure> {
    let response = client
        .post(format!(
            "{}/oauth/token",
            config.issuer.trim_end_matches('/')
        ))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", current.refresh_token.as_str()),
            ("client_id", CLIENT_ID),
        ])
        .send()
        .await
        .map_err(|_| RefreshFailure::Runtime)?;
    read_token_response(response, Some(current)).await
}

async fn read_token_response(
    response: reqwest::Response,
    previous: Option<&StoredCredential>,
) -> Result<StoredCredential, RefreshFailure> {
    if !response.status().is_success() {
        return Err(RefreshFailure::Rejected);
    }
    let token = response
        .json::<TokenResponse>()
        .await
        .map_err(|_| RefreshFailure::Runtime)?;
    let account_id = token
        .id_token
        .as_deref()
        .and_then(extract_account_id)
        .or_else(|| extract_account_id(&token.access_token))
        .or_else(|| previous.map(|credential| credential.account_id.clone()))
        .ok_or(RefreshFailure::Runtime)?;
    let refresh_token = token
        .refresh_token
        .or_else(|| previous.map(|credential| credential.refresh_token.clone()))
        .ok_or(RefreshFailure::Runtime)?;
    Ok(StoredCredential {
        credential_type: "oauth".to_owned(),
        access_token: token.access_token,
        refresh_token,
        account_id,
        expires_at: now_millis().saturating_add(token.expires_in.unwrap_or(3_600) * 1_000),
    })
}

fn extract_account_id(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let claims = serde_json::from_slice::<serde_json::Value>(&bytes).ok()?;
    claims
        .get("chatgpt_account_id")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            claims
                .get("https://api.openai.com/auth")
                .and_then(|auth| auth.get("chatgpt_account_id"))
                .and_then(serde_json::Value::as_str)
        })
        .or_else(|| {
            claims
                .get("organizations")
                .and_then(serde_json::Value::as_array)
                .and_then(|organizations| organizations.first())
                .and_then(|organization| organization.get("id"))
                .and_then(serde_json::Value::as_str)
        })
        .map(str::to_owned)
}

fn persist_credential(
    config: &AuthConfig,
    credential: StoredCredential,
) -> Result<DirectAuthStatus, String> {
    let path = config
        .credential_path()
        .map_err(|error| runtime_text(&error))?;
    let _lock = CredentialLock::acquire(&path)
        .map_err(|_| "failed to lock the direct-auth credential store".to_owned())?;
    let mut credentials = match read_credentials(&path) {
        Ok(credentials) => credentials,
        Err(error) if error.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
        Err(_) => return Err("failed to read the direct-auth credential store".to_owned()),
    };
    let expires_at = credential.expires_at;
    credentials.insert(
        config.credential_key(),
        serde_json::to_value(credential)
            .map_err(|_| "failed to encode the direct-auth credential".to_owned())?,
    );
    write_credentials(&path, &credentials)
        .map_err(|_| "failed to persist the direct-auth credential".to_owned())?;
    Ok(DirectAuthStatus {
        authenticated: true,
        expires_at: Some(expires_at),
    })
}

fn read_credentials(path: &Path) -> io::Result<BTreeMap<String, serde_json::Value>> {
    serde_json::from_slice(&fs::read(path)?).map_err(io::Error::other)
}

fn credential_from_store(
    credentials: &BTreeMap<String, serde_json::Value>,
    key: &str,
) -> io::Result<Option<StoredCredential>> {
    let Some(value) = credentials.get(key) else {
        return Ok(None);
    };
    let credential =
        serde_json::from_value::<StoredCredential>(value.clone()).map_err(io::Error::other)?;
    if credential.credential_type != "oauth" {
        return Err(io::Error::other("unsupported credential type"));
    }
    Ok(Some(credential))
}

fn write_credentials(
    path: &Path,
    credentials: &BTreeMap<String, serde_json::Value>,
) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("credential path has no parent"))?;
    secure_create_dir_all(parent)?;
    let temporary = parent.join(format!(".auth-{}.tmp", uuid::Uuid::new_v4()));
    let mut file = secure_open(&temporary, true)?;
    serde_json::to_writer_pretty(&mut file, credentials).map_err(io::Error::other)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    Ok(())
}

fn secure_create_dir_all(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn secure_open(path: &Path, truncate: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .truncate(truncate);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn validate_issuer(issuer: &str) -> Result<(), RuntimeFailure> {
    let url = reqwest::Url::parse(issuer)
        .map_err(|_| invalid_plan("OpenAI Codex Auth issuer is invalid"))?;
    let official = url.as_str().trim_end_matches('/') == DEFAULT_ISSUER;
    let loopback = url.scheme() == "http"
        && url
            .host_str()
            .is_some_and(|host| matches!(host, "127.0.0.1" | "localhost" | "::1"));
    if (!official && !loopback)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid_plan(
            "OpenAI Codex Auth issuer must be auth.openai.com or loopback HTTP",
        ));
    }
    Ok(())
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

fn plugin_failure(detail: impl Into<String>) -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: detail.into(),
    }
}

fn runtime_text(error: &RuntimeFailure) -> String {
    format!("{error:?}")
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        thread,
    };

    use super::{
        DirectAuthOptions, StoredCredential, begin_browser_login, begin_device_login,
        complete_browser_login, complete_device_login, credential_from_store, extract_account_id,
        read_credentials, write_credentials,
    };
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    #[test]
    fn credential_round_trip_is_private_and_debug_is_redacted() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("auth.json");
        let credential = StoredCredential {
            credential_type: "oauth".to_owned(),
            access_token: "access-secret".to_owned(),
            refresh_token: "refresh-secret".to_owned(),
            account_id: "account-secret".to_owned(),
            expires_at: 42,
        };
        let mut credentials = BTreeMap::new();
        credentials.insert(
            "openai-codex".to_owned(),
            serde_json::to_value(credential).unwrap(),
        );
        credentials.insert(
            "future-provider".to_owned(),
            serde_json::json!({ "type": "api_key", "key": "preserve-me" }),
        );
        write_credentials(&path, &credentials).unwrap();
        let credentials = read_credentials(&path).unwrap();
        assert_eq!(credentials["future-provider"]["key"], "preserve-me");
        let loaded = credential_from_store(&credentials, "openai-codex")
            .unwrap()
            .unwrap();
        assert_eq!(loaded.access_token, "access-secret");
        let debug = format!("{loaded:?}");
        assert!(!debug.contains("access-secret"));
        assert!(!debug.contains("refresh-secret"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn extracts_namespaced_chatgpt_account_id() {
        let claims = serde_json::json!({
            "https://api.openai.com/auth": { "chatgpt_account_id": "acct-1" }
        });
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        assert_eq!(
            extract_account_id(&format!("header.{payload}.signature")).as_deref(),
            Some("acct-1")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn browser_login_uses_pkce_callback_and_persists_the_default_profile() {
        let temporary = tempfile::tempdir().unwrap();
        let credential_path = temporary.path().join("auth.json");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let claims = serde_json::json!({
            "https://api.openai.com/auth": { "chatgpt_account_id": "acct-browser" }
        });
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        let access_token = format!("header.{payload}.signature");
        let expected_access_token = access_token.clone();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            write_json_response(
                &mut stream,
                &serde_json::json!({
                    "access_token": access_token,
                    "refresh_token": "refresh-browser",
                    "expires_in": 3600
                }),
            );
            request
        });
        let options = DirectAuthOptions {
            issuer,
            profile: "default".to_owned(),
            credential_file: Some(credential_path.clone()),
            callback_port: 0,
        };
        let pending = begin_browser_login(options.clone()).await.unwrap();
        let authorization_url = reqwest::Url::parse(&pending.authorization_url).unwrap();
        let parameters = authorization_url.query_pairs().collect::<BTreeMap<_, _>>();
        assert_eq!(parameters.get("code_challenge_method").unwrap(), "S256");
        assert_eq!(parameters.get("codex_cli_simplified_flow").unwrap(), "true");
        assert_eq!(parameters.get("originator").unwrap(), "lenso");
        let mut callback_url =
            reqwest::Url::parse(parameters.get("redirect_uri").unwrap()).unwrap();
        callback_url
            .query_pairs_mut()
            .append_pair("code", "authorization-browser")
            .append_pair("state", parameters.get("state").unwrap());
        let callback = async move {
            reqwest::get(callback_url)
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
        };
        let completion = complete_browser_login(options, pending);
        let (status, ()) = tokio::join!(completion, callback);
        assert!(status.unwrap().authenticated);
        let credentials = read_credentials(&credential_path).unwrap();
        let stored = credential_from_store(&credentials, "openai-codex")
            .unwrap()
            .unwrap();
        assert_eq!(stored.access_token, expected_access_token);
        assert_eq!(stored.refresh_token, "refresh-browser");
        assert_eq!(stored.account_id, "acct-browser");

        let token_request = server.join().unwrap();
        assert!(token_request.starts_with("POST /oauth/token "));
        assert!(token_request.contains("code=authorization-browser"));
        assert!(token_request.contains("code_verifier="));
        assert!(token_request.contains("redirect_uri=http%3A%2F%2Flocalhost%3A"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn device_login_exchanges_and_persists_a_private_credential() {
        let temporary = tempfile::tempdir().unwrap();
        let credential_path = temporary.path().join("auth.json");
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let issuer = format!("http://{}", listener.local_addr().unwrap());
        let claims = serde_json::json!({
            "https://api.openai.com/auth": { "chatgpt_account_id": "acct-device" }
        });
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).unwrap());
        let access_token = format!("header.{payload}.signature");
        let expected_access_token = access_token.clone();
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            for body in [
                serde_json::json!({
                    "device_auth_id": "device-1",
                    "user_code": "ABCD-EFGH",
                    "interval": "1"
                }),
                serde_json::json!({
                    "authorization_code": "authorization-1",
                    "code_verifier": "verifier-1"
                }),
                serde_json::json!({
                    "access_token": access_token,
                    "refresh_token": "refresh-device",
                    "expires_in": 3600
                }),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                requests.push(read_http_request(&mut stream));
                write_json_response(&mut stream, &body);
            }
            requests
        });
        let options = DirectAuthOptions {
            issuer: issuer.clone(),
            profile: "integration".to_owned(),
            credential_file: Some(credential_path.clone()),
            callback_port: 0,
        };
        let pending = begin_device_login(options.clone()).await.unwrap();
        assert_eq!(pending.verification_url, format!("{issuer}/codex/device"));
        assert_eq!(pending.user_code, "ABCD-EFGH");
        let status = complete_device_login(options, pending).await.unwrap();
        assert!(status.authenticated);
        let credentials = read_credentials(&credential_path).unwrap();
        let stored = credential_from_store(&credentials, "openai-codex:integration")
            .unwrap()
            .unwrap();
        assert_eq!(stored.access_token, expected_access_token);
        assert_eq!(stored.refresh_token, "refresh-device");
        assert_eq!(stored.account_id, "acct-device");

        let requests = server.join().unwrap();
        assert!(requests[0].starts_with("POST /api/accounts/deviceauth/usercode "));
        assert!(requests[1].starts_with("POST /api/accounts/deviceauth/token "));
        assert!(requests[2].starts_with("POST /oauth/token "));
        assert!(requests[2].contains("grant_type=authorization_code"));
        assert!(requests[2].contains("code=authorization-1"));
        assert!(requests[2].contains("code_verifier=verifier-1"));
    }

    fn read_http_request(stream: &mut TcpStream) -> String {
        let mut bytes = Vec::new();
        let header_end = loop {
            let mut buffer = [0_u8; 2048];
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0);
            bytes.extend_from_slice(&buffer[..read]);
            if let Some(position) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                break position + 4;
            }
        };
        let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        while bytes.len() - header_end < content_length {
            let mut buffer = [0_u8; 2048];
            let read = stream.read(&mut buffer).unwrap();
            assert!(read > 0);
            bytes.extend_from_slice(&buffer[..read]);
        }
        String::from_utf8(bytes[..header_end + content_length].to_vec()).unwrap()
    }

    fn write_json_response(stream: &mut TcpStream, body: &serde_json::Value) {
        let body = serde_json::to_vec(body).unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(&body).unwrap();
        stream.flush().unwrap();
    }
}
