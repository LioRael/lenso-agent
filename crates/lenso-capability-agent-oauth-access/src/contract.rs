//! Authoritative source for short-lived OAuth access.

use lenso_contract_authoring as lenso;

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct AccessRequest {
    #[schemars(length(min = 8, max = 4096))]
    pub resource_uri: String,
    #[schemars(length(max = 64))]
    pub scopes: Vec<String>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct AccessResponse {
    #[schemars(length(min = 1, max = 16384), extend("x-lenso-sensitive" = true))]
    pub access_token: String,
    #[schemars(length(min = 1, max = 32))]
    pub token_type: String,
    #[schemars(length(min = 1, max = 20), regex(pattern = r"^(0|[1-9][0-9]*)$"))]
    pub expires_at_millis: String,
    #[schemars(length(max = 64))]
    pub scopes: Vec<String>,
}

#[derive(lenso::DomainError)]
pub enum AccessError {
    UnknownResource,
    CredentialUnavailable,
    DiscoveryRejected,
    AuthorizationRejected,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct InvalidateRequest {
    #[schemars(length(min = 8, max = 4096))]
    pub resource_uri: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct InvalidateResponse {
    pub invalidated: bool,
}

#[derive(lenso::DomainError)]
pub enum InvalidateError {
    UnknownResource,
}

#[lenso::capability(
    id = "lenso.agent.oauth-access",
    major = 1,
    version = "1.0.0",
    portable = false,
    cross_lane_transfer = false
)]
pub trait OauthAccess {
    async fn access(
        &self,
        context: lenso::Ctx<'_>,
        request: AccessRequest,
    ) -> Result<AccessResponse, AccessError>;

    async fn invalidate(
        &self,
        context: lenso::Ctx<'_>,
        request: InvalidateRequest,
    ) -> Result<InvalidateResponse, InvalidateError>;
}
