//! Provider-owned identity capture. The scope contains no credential material.
use lenso_contract_authoring as lenso;
#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct CaptureRequest {
    #[schemars(length(min = 36, max = 36))]
    pub scope_id: String,
}
#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct CaptureResponse {}
#[derive(lenso::DomainError)]
pub enum CaptureError {
    Unavailable,
    CapacityExceeded,
}
#[lenso::capability(
    id = "lenso.agent.turn-binding",
    major = 1,
    version = "1.0.0",
    portable = false,
    cross_lane_transfer = false
)]
pub trait TurnBinding {
    /// Capture identity now, retaining it only until the invocation cancellation
    /// token is cancelled. Completion of this request does not release the lease.
    async fn capture(
        &self,
        context: lenso::Ctx<'_>,
        request: CaptureRequest,
    ) -> Result<CaptureResponse, CaptureError>;
}
