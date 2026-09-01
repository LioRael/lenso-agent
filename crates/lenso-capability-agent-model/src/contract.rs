//! Authoritative source for the Agent Model Capability contract.

use lenso_contract_authoring as lenso;

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct CatalogRequest {}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct CatalogResponse {
    #[schemars(length(min = 1, max = 128))]
    pub models: Vec<CatalogModel>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "portable model feature flags are independent Provider facts"
)]
pub struct CatalogModel {
    #[schemars(length(min = 1, max = 256))]
    pub id: String,
    #[schemars(length(min = 1, max = 256))]
    pub display_name: String,
    #[schemars(length(max = 4_096))]
    pub description: String,
    pub hidden: bool,
    pub limits: CatalogModelLimits,
    #[schemars(length(min = 1, max = 3))]
    pub input_modalities: Vec<CatalogInputModality>,
    pub text_output: bool,
    pub tool_calls: bool,
    pub parallel_tool_calls: bool,
    pub reasoning: CatalogControl,
    pub service_tiers: CatalogControl,
    pub wire_protocol: CatalogWireProtocol,
    #[schemars(length(min = 1, max = 128))]
    pub compaction_compatibility: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
#[allow(
    clippy::struct_field_names,
    reason = "portable token limits retain explicit units on every field"
)]
pub struct CatalogModelLimits {
    #[schemars(extend("format" = "uint64"))]
    pub context_window_tokens: Option<String>,
    #[schemars(extend("format" = "uint64"))]
    pub max_input_tokens: Option<String>,
    #[schemars(extend("format" = "uint64"))]
    pub max_output_tokens: Option<String>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogInputModality {
    Text,
    Image,
    Audio,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct CatalogControl {
    pub status: CatalogControlStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<CatalogControlMode>,
    #[schemars(length(max = 16))]
    pub options: Vec<CatalogControlOption>,
    #[schemars(length(min = 1, max = 32))]
    pub default: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_tokens: Option<CatalogTokenBudget>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogControlStatus {
    Unknown,
    Unsupported,
    Selectable,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogControlMode {
    Effort,
    Toggle,
    BudgetTokens,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct CatalogTokenBudget {
    #[schemars(extend("format" = "uint64"))]
    pub minimum: String,
    #[schemars(extend("format" = "uint64"))]
    pub maximum: String,
    #[schemars(extend("format" = "uint64"))]
    pub default: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct CatalogControlOption {
    #[schemars(length(min = 1, max = 32))]
    pub id: String,
    #[schemars(length(min = 1, max = 128))]
    pub name: String,
    #[schemars(length(max = 1_024))]
    pub description: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogWireProtocol {
    Fixture,
    OpenaiResponses,
    OpenaiChatCompletions,
}

#[derive(lenso::DomainError)]
pub enum CatalogError {
    CatalogInvalid,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct CompleteOpen {
    #[schemars(length(min = 1, max = 256))]
    pub model: String,
    #[schemars(length(max = 256))]
    pub messages: Vec<CompleteMessageInput>,
    #[schemars(length(max = 256))]
    pub tools: Vec<CompleteTool>,
    #[schemars(range(min = 0, max = 2))]
    pub temperature: f64,
    #[schemars(range(min = 1, max = 1_000_000))]
    pub max_output_tokens: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String", length(min = 1, max = 32))]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "bool")]
    pub reasoning_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String", extend("format" = "uint64"))]
    pub reasoning_budget_tokens: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String", length(min = 1, max = 32))]
    pub service_tier: Option<String>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct CompleteMessageInput {
    pub role: CompleteMessageRole,
    #[schemars(length(max = 1_048_576))]
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String", length(max = 128))]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String", length(max = 128))]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String", length(max = 262_144))]
    pub arguments_json: Option<String>,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompleteMessageRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct CompleteTool {
    #[schemars(length(min = 1, max = 128))]
    pub name: String,
    #[schemars(length(max = 4_096))]
    pub description: String,
    #[schemars(length(min = 2, max = 65_536))]
    pub input_schema_json: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct CompleteMessage {
    #[schemars(extend("format" = "uint64"))]
    pub sequence: String,
    pub kind: CompleteMessageKind,
    #[schemars(length(max = 65_536))]
    pub text: String,
    #[schemars(length(max = 128))]
    pub tool_call_id: String,
    #[schemars(length(max = 128))]
    pub tool_name: String,
    #[schemars(length(max = 262_144))]
    pub arguments_json: String,
    #[schemars(extend("format" = "uint64"))]
    pub input_tokens: String,
    #[schemars(extend("format" = "uint64"))]
    pub output_tokens: String,
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompleteMessageKind {
    ReasoningSummaryDelta,
    TextDelta,
    ToolCall,
    Usage,
}

#[derive(lenso::DomainError)]
pub enum CompleteError {
    InvalidRequest,
    UnsupportedModel,
    ContentRejected,
    RateLimited,
    Overloaded,
    ContextOverflow,
    ProviderFailure { payload: ProviderFailurePayload },
}

#[derive(lenso::JsonSchema, serde::Deserialize)]
#[schemars(deny_unknown_fields)]
pub struct ProviderFailurePayload {
    #[schemars(length(min = 1, max = 128))]
    pub reason_code: String,
    #[schemars(length(min = 1, max = 4_096))]
    pub message: String,
    pub retryable: bool,
}

#[lenso::capability(
    id = "lenso.agent.model",
    major = 3,
    version = "3.0.0",
    portable = true,
    cross_lane_transfer = false
)]
pub trait Model {
    async fn catalog(
        &self,
        context: lenso::Ctx<'_>,
        request: CatalogRequest,
    ) -> Result<CatalogResponse, CatalogError>;

    async fn complete(
        &self,
        context: lenso::Ctx<'_>,
        request: CompleteOpen,
    ) -> lenso::Stream<CompleteMessage, CompleteError>;
}
