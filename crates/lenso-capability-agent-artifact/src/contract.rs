//! Authoritative source for durable, content-addressed Agent Artifacts.

use lenso_contract_authoring as lenso;

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct PutRequest {
    #[schemars(length(min = 1, max = 128))]
    pub session_id: String,
    #[schemars(length(min = 1, max = 256))]
    pub name: String,
    #[schemars(length(min = 1, max = 128))]
    pub media_type: String,
    #[schemars(length(min = 1, max = 22_369_624))]
    pub data_base64: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct PutResponse {
    #[schemars(length(min = 16, max = 160))]
    pub handle: String,
    #[schemars(length(min = 71, max = 71), regex(pattern = r"^sha256:[0-9a-f]{64}$"))]
    pub digest: String,
    #[schemars(length(min = 1, max = 20), regex(pattern = r"^(0|[1-9][0-9]*)$"))]
    pub size: String,
}

#[derive(lenso::DomainError)]
pub enum PutError {
    InvalidRequest,
    InvalidData,
    TooLarge,
    CapacityExceeded,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ReadRequest {
    #[schemars(length(min = 16, max = 160))]
    pub handle: String,
    #[schemars(length(min = 1, max = 20), regex(pattern = r"^(0|[1-9][0-9]*)$"))]
    pub offset: String,
    pub max_bytes: u32,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ReadResponse {
    #[schemars(length(max = 5_592_408))]
    pub data_base64: String,
    #[schemars(length(min = 1, max = 20), regex(pattern = r"^(0|[1-9][0-9]*)$"))]
    pub total_size: String,
    #[schemars(length(min = 1, max = 20), regex(pattern = r"^(0|[1-9][0-9]*)$"))]
    pub next_offset: String,
    pub complete: bool,
}

#[derive(lenso::DomainError)]
pub enum ReadError {
    InvalidHandle,
    InvalidRange,
    NotFound,
}

#[lenso::capability(
    id = "lenso.agent.artifact",
    major = 1,
    version = "1.0.0",
    portable = true,
    cross_lane_transfer = false
)]
pub trait Artifact {
    async fn put(
        &self,
        context: lenso::Ctx<'_>,
        request: PutRequest,
    ) -> Result<PutResponse, PutError>;

    async fn read(
        &self,
        context: lenso::Ctx<'_>,
        request: ReadRequest,
    ) -> Result<ReadResponse, ReadError>;
}
