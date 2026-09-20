//! Authoritative source for bounded, bidirectional Agent interactions.
//!
//! `lenso.agent@3:run_turn` remains a compatible turn protocol. This Capability
//! is for a different interaction shape: a selected Provider owns one ordered
//! duplex stream and explicitly documents its reconnection behavior.

use lenso_contract_authoring as lenso;

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ConnectOpen {
    /// Application-defined protocol identifier, for example
    /// `example.realtime-audio@1`.
    #[schemars(length(min = 1, max = 192))]
    pub protocol: String,
    #[schemars(length(min = 1, max = 128))]
    pub interaction_id: String,
    /// An opaque Provider cursor. Providers that do not support resumption
    /// must reject this at open rather than silently creating a fresh session.
    #[schemars(length(min = 1, max = 4_096))]
    pub resume_from: Option<String>,
    /// Required bounded flow policy negotiated by the caller and Provider.
    #[schemars(range(min = 1, max = 128))]
    pub max_pending_frames: u32,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct InteractionFrame {
    #[schemars(length(min = 1, max = 192))]
    pub protocol: String,
    /// Independent, strictly increasing decimal sequence in each direction.
    #[schemars(extend("format" = "uint64"))]
    pub sequence: String,
    pub direction: FrameDirection,
    pub kind: FrameKind,
    #[schemars(length(max = 128))]
    pub correlation_id: String,
    /// A structured, bounded frame. It may contain fake audio sample metadata
    /// or binary references, but not an unbounded Base64 transcript blob.
    #[schemars(length(min = 2, max = 65_536))]
    pub payload_json: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameDirection {
    ClientToAgent,
    AgentToClient,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameKind {
    Input,
    Output,
    Interrupt,
    Acknowledgement,
    Notice,
}

#[derive(lenso::DomainError)]
pub enum InteractionError {
    UnsupportedProtocol,
    ResumeUnsupported,
    InvalidFrame,
    Interrupted,
    CapacityExceeded,
    OutputLimitExceeded,
}

#[lenso::capability(
    id = "lenso.agent.interaction",
    major = 1,
    version = "1.0.0",
    portable = true,
    cross_lane_transfer = false
)]
pub trait Interaction {
    async fn connect(
        &self,
        context: lenso::Ctx<'_>,
        request: ConnectOpen,
    ) -> lenso::Stream<InteractionFrame, InteractionError>;
}
