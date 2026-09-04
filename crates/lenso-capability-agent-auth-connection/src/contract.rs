//! Source of the provider-owned authentication connection interface.

use lenso_contract_authoring as lenso;

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct StatusRequest {}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginMethod {
    DeviceCode,
    BrowserLoopback,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct StatusResponse {
    /// Provider-owned display name, not a package identifier.
    pub label: String,
    /// Presence of a stored grant, not proof that a remote service accepts it.
    pub connected: bool,
    pub methods: Vec<LoginMethod>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct BeginRequest {
    pub method: LoginMethod,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct BeginResponse {
    #[schemars(length(min = 1, max = 128), extend("x-lenso-sensitive" = true))]
    pub attempt_id: String,
    #[schemars(length(min = 1, max = 8192), extend("x-lenso-sensitive" = true))]
    pub authorization_url: String,
    /// Empty for browser login. Never an access or refresh token.
    #[schemars(length(max = 128), extend("x-lenso-sensitive" = true))]
    pub user_code: String,
    /// Decimal Unix milliseconds; attempts are bounded and generation-local.
    pub expires_at_millis: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct AttemptRequest {
    #[schemars(length(min = 1, max = 128), extend("x-lenso-sensitive" = true))]
    pub attempt_id: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginState {
    Pending,
    Connected,
    Failed,
    Cancelled,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct PollResponse {
    pub state: LoginState,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct CancelResponse {
    pub cancelled: bool,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct DisconnectRequest {}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct DisconnectResponse {
    /// Local grant removed; remote revocation is not implied.
    pub disconnected: bool,
}

#[derive(lenso::DomainError)]
pub enum ConnectionError {
    UnsupportedMethod,
    AttemptInProgress,
    UnknownAttempt,
    AuthorizationRejected,
}

#[lenso::capability(
    id = "lenso.agent.auth-connection",
    major = 1,
    version = "1.0.0",
    portable = false,
    cross_lane_transfer = false
)]
pub trait AuthConnection {
    async fn status(
        &self,
        context: lenso::Ctx<'_>,
        request: StatusRequest,
    ) -> Result<StatusResponse, ConnectionError>;
    async fn begin(
        &self,
        context: lenso::Ctx<'_>,
        request: BeginRequest,
    ) -> Result<BeginResponse, ConnectionError>;
    async fn poll(
        &self,
        context: lenso::Ctx<'_>,
        request: AttemptRequest,
    ) -> Result<PollResponse, ConnectionError>;
    async fn cancel(
        &self,
        context: lenso::Ctx<'_>,
        request: AttemptRequest,
    ) -> Result<CancelResponse, ConnectionError>;
    async fn disconnect(
        &self,
        context: lenso::Ctx<'_>,
        request: DisconnectRequest,
    ) -> Result<DisconnectResponse, ConnectionError>;
}
