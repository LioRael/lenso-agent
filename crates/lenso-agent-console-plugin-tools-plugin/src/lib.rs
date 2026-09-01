//! Reviewed Plugin Root inspection and configuration tools for Console Agent.

use lenso::Port;
use lenso_agent_tool_sdk::prelude::*;
use lenso_capability_agent_plugin_configuration_authority as configuration_contract;
use lenso_capability_agent_tool_provider::ExecutionFailedPayload;
use lenso_kernel::RuntimeFailure;
use schemars::JsonSchema;
use serde::Serialize;

pub const INSPECT_APP_TOOL: &str = "inspect_app";
pub const LIST_PLUGINS_TOOL: &str = "list_plugins";
pub const INSPECT_PLUGIN_TOOL: &str = "inspect_plugin";
pub const CHECK_PLUGIN_CHANGE_TOOL: &str = "check_plugin_change";
pub const APPLY_PLUGIN_CHANGE_TOOL: &str = "apply_plugin_change";
pub const PLUGIN_PACKAGE_ID: &str = "lenso.agent.console-plugin-tools";

const MAX_CONFIGURATION_BYTES: usize = 7 * 1024;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsolePluginToolsConfig {
    max_output_bytes: usize,
}

#[derive(JsonSchema, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArguments {}

#[derive(JsonSchema, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ListPluginsArguments {
    #[schemars(length(max = 256))]
    query: Option<String>,
}

#[derive(JsonSchema, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct InspectPluginArguments {
    #[schemars(length(min = 1, max = 128))]
    plugin_id: String,
}

#[derive(JsonSchema, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckPluginChangeArguments {
    #[schemars(length(min = 1, max = 7_168))]
    configuration_toml: String,
    #[schemars(length(min = 71, max = 71))]
    expected_revision: String,
    #[schemars(length(min = 1, max = 128))]
    instance: String,
    #[schemars(length(min = 1, max = 128))]
    plugin_id: String,
}

#[derive(JsonSchema, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyPluginChangeArguments {
    #[schemars(length(min = 1, max = 7_168))]
    configuration_toml: String,
    #[schemars(length(min = 71, max = 71))]
    expected_revision: String,
    #[schemars(length(min = 71, max = 71))]
    proposal_digest: String,
    #[schemars(length(min = 1, max = 128))]
    instance: String,
    #[schemars(length(min = 1, max = 128))]
    plugin_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppInspection {
    authority: configuration_contract::AuthoritySource,
    binding_count: i64,
    enabled_instance_count: i64,
    plugin_count: usize,
    revision: String,
    schema: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginList {
    authority: configuration_contract::AuthoritySource,
    plugins: Vec<PluginSummary>,
    query: String,
    revision: String,
    schema: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginSummary {
    enabled_instance_count: usize,
    instance_count: usize,
    package_id: String,
    package_revision: String,
    source: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginInspection {
    authority: configuration_contract::AuthoritySource,
    instances: Vec<PluginInstanceInspection>,
    package_id: String,
    package_revision: String,
    revision: String,
    schema: &'static str,
    source: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginInstanceInspection {
    disableable: bool,
    has_root_difference: bool,
    instance_key: String,
    origin: String,
    root_configuration_bytes: Option<i64>,
    selection: String,
    source_digest: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProposalInspection {
    application: String,
    authority: configuration_contract::AuthoritySource,
    base_revision: String,
    base_source_digest: String,
    candidate_revision: String,
    diagnostics: Vec<ProposalDiagnostic>,
    instance: String,
    plugin_id: String,
    proposal_digest: String,
    schema: String,
    status: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProposalDiagnostic {
    code: String,
    detail: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PublicationInspection {
    authority: configuration_contract::AuthoritySource,
    base_revision: String,
    base_source_digest: String,
    proposal_digest: String,
    revision: String,
    schema: String,
    status: &'static str,
}

fn validate_config(config: &ConsolePluginToolsConfig) -> Result<(), RuntimeFailure> {
    if !(4_096..=262_144).contains(&config.max_output_bytes) {
        return Err(RuntimeFailure::InvalidResolvedPlan {
            detail: "Console Plugin Tools max_output_bytes must be between 4096 and 262144"
                .to_owned(),
        });
    }
    Ok(())
}

#[lenso::plugin(configuration_schema = "config.schema.json", validate = validate_config)]
#[derive(Clone, Debug)]
struct ConsolePluginTools {
    #[config]
    config: ConsolePluginToolsConfig,
    authority: Port<configuration_contract::PluginConfigurationAuthorityClient>,
}

#[lenso_agent_tool_sdk::tool_provider]
impl ConsolePluginTools {
    #[tool(
        name = "inspect_app",
        description = "Inspect the selected Plugin configuration authority, current desired revision, and resolved App size.",
        execution = "parallel_safe"
    )]
    async fn inspect_app(
        &self,
        _arguments: EmptyArguments,
    ) -> Result<ExecuteResponse, ExecuteError> {
        let state = self
            .authority
            .inspect(configuration_contract::InspectRequest {})
            .await
            .map_err(map_inspect_error)?;
        self.json_response(
            INSPECT_APP_TOOL,
            &AppInspection {
                authority: state.authority,
                binding_count: state.binding_count,
                enabled_instance_count: state.enabled_instance_count,
                plugin_count: state.plugins.len(),
                revision: state.revision,
                schema: "lenso.agent.console-app-inspection.v1",
            },
        )
    }

    #[tool(
        name = "list_plugins",
        description = "List Plugins visible through the selected Console Agent Plugin configuration authority.",
        execution = "parallel_safe"
    )]
    async fn list_plugins(
        &self,
        arguments: ListPluginsArguments,
    ) -> Result<ExecuteResponse, ExecuteError> {
        let state = self
            .authority
            .inspect(configuration_contract::InspectRequest {})
            .await
            .map_err(map_inspect_error)?;
        let query = arguments.query.unwrap_or_default();
        let folded = query.to_lowercase();
        let plugins = state
            .plugins
            .into_iter()
            .filter(|plugin| {
                folded.is_empty() || plugin.package_id.to_lowercase().contains(&folded)
            })
            .map(|plugin| PluginSummary {
                enabled_instance_count: plugin
                    .instances
                    .iter()
                    .filter(|instance| instance.selection == "enabled")
                    .count(),
                instance_count: plugin.instances.len(),
                package_id: plugin.package_id,
                package_revision: plugin.package_revision,
                source: plugin.source,
            })
            .collect();
        self.json_response(
            LIST_PLUGINS_TOOL,
            &PluginList {
                authority: state.authority,
                plugins,
                query,
                revision: state.revision,
                schema: "lenso.agent.console-plugin-list.v1",
            },
        )
    }

    #[tool(
        name = "inspect_plugin",
        description = "Inspect one Plugin and its exact Instance differences through the selected configuration authority.",
        execution = "parallel_safe"
    )]
    async fn inspect_plugin(
        &self,
        arguments: InspectPluginArguments,
    ) -> Result<ExecuteResponse, ExecuteError> {
        let state = self
            .authority
            .inspect(configuration_contract::InspectRequest {})
            .await
            .map_err(map_inspect_error)?;
        let plugin = state
            .plugins
            .into_iter()
            .find(|plugin| plugin.package_id == arguments.plugin_id)
            .ok_or(ExecuteError::NotFound)?;
        self.json_response(
            INSPECT_PLUGIN_TOOL,
            &PluginInspection {
                authority: state.authority,
                instances: plugin
                    .instances
                    .into_iter()
                    .map(|instance| PluginInstanceInspection {
                        disableable: instance.disableable,
                        has_root_difference: instance.has_root_difference,
                        instance_key: instance.instance_key,
                        origin: instance.origin,
                        root_configuration_bytes: instance
                            .root_configuration_present
                            .then_some(instance.root_configuration_bytes),
                        selection: instance.selection,
                        source_digest: instance.source_digest,
                    })
                    .collect(),
                package_id: plugin.package_id,
                package_revision: plugin.package_revision,
                revision: state.revision,
                schema: "lenso.agent.console-plugin-inspection.v1",
                source: plugin.source,
            },
        )
    }

    #[tool(
        name = "check_plugin_change",
        description = "Validate one exact Plugin configuration candidate through the selected authority without publishing it.",
        execution = "parallel_safe"
    )]
    async fn check_plugin_change(
        &self,
        arguments: CheckPluginChangeArguments,
    ) -> Result<ExecuteResponse, ExecuteError> {
        ensure_configuration_size(&arguments.configuration_toml)?;
        let proposal = self
            .authority
            .propose(configuration_contract::ProposeRequest {
                configuration_toml: arguments.configuration_toml,
                expected_revision: arguments.expected_revision,
                instance: arguments.instance,
                plugin_id: arguments.plugin_id,
            })
            .await
            .map_err(map_propose_error)?;
        self.json_response(
            CHECK_PLUGIN_CHANGE_TOOL,
            &ProposalInspection {
                application: proposal.application,
                authority: proposal.authority,
                base_revision: proposal.base_revision,
                base_source_digest: proposal.base_source_digest,
                candidate_revision: proposal.candidate_revision,
                diagnostics: proposal
                    .diagnostics
                    .into_iter()
                    .map(|diagnostic| ProposalDiagnostic {
                        code: diagnostic.code,
                        detail: diagnostic.detail,
                    })
                    .collect(),
                instance: proposal.instance,
                plugin_id: proposal.plugin_id,
                proposal_digest: proposal.proposal_digest,
                schema: proposal.schema,
                status: proposal.status,
            },
        )
    }

    #[tool(
        name = "apply_plugin_change",
        description = "Publish one reviewed Plugin configuration proposal through the selected authority after exact revision and digest checks.",
        execution = "exclusive"
    )]
    async fn apply_plugin_change(
        &self,
        arguments: ApplyPluginChangeArguments,
    ) -> Result<ExecuteResponse, ExecuteError> {
        ensure_configuration_size(&arguments.configuration_toml)?;
        let publication = self
            .authority
            .publish(configuration_contract::PublishRequest {
                configuration_toml: arguments.configuration_toml,
                expected_revision: arguments.expected_revision,
                instance: arguments.instance,
                plugin_id: arguments.plugin_id,
                proposal_digest: arguments.proposal_digest,
            })
            .await
            .map_err(map_publish_error)?;
        self.json_response(
            APPLY_PLUGIN_CHANGE_TOOL,
            &PublicationInspection {
                authority: publication.authority,
                base_revision: publication.base_revision,
                base_source_digest: publication.base_source_digest,
                proposal_digest: publication.proposal_digest,
                revision: publication.revision,
                schema: publication.schema,
                status: "published_desired_state",
            },
        )
    }

    fn json_response(
        &self,
        operation: &str,
        value: &impl Serialize,
    ) -> Result<ExecuteResponse, ExecuteError> {
        let content = serde_json::to_string(value).map_err(|error| {
            execution_failed(
                "response_encoding_failed",
                &format!("Console Tool response could not be encoded: {error}"),
            )
        })?;
        if content.len() > self.config.max_output_bytes {
            return Err(ExecuteError::OutputLimitExceeded);
        }
        Ok(ExecuteResponse {
            content_blocks: None,
            content,
            content_type: ContentType::Text,
            metadata_json: serde_json::json!({ "operation": operation })
                .to_string()
                .try_into()
                .expect("Tool metadata must be valid JSON"),
        })
    }
}

fn ensure_configuration_size(value: &str) -> Result<(), ExecuteError> {
    if value.len() > MAX_CONFIGURATION_BYTES {
        Err(ExecuteError::InvalidArguments)
    } else {
        Ok(())
    }
}

fn invalid_request() -> ExecuteError {
    ExecuteError::InvalidArguments
}

fn not_found() -> ExecuteError {
    ExecuteError::NotFound
}

fn conflict() -> ExecuteError {
    execution_failed(
        "configuration_conflict",
        "The Plugin configuration revision changed before this operation.",
    )
}

fn proposal_mismatch() -> ExecuteError {
    execution_failed(
        "proposal_mismatch",
        "Plugin configuration no longer matches the reviewed proposal.",
    )
}

fn proposal_not_ready() -> ExecuteError {
    execution_failed(
        "proposal_not_ready",
        "Plugin configuration proposal did not pass candidate validation.",
    )
}

fn unknown_rejection() -> ExecuteError {
    execution_failed(
        "configuration_authority_rejected",
        "The selected Plugin configuration authority rejected the operation.",
    )
}

fn map_inspect_error(
    error: configuration_contract::PluginConfigurationAuthorityInspectInvocationError,
) -> ExecuteError {
    match error {
        configuration_contract::PluginConfigurationAuthorityInspectInvocationError::Domain(
            error,
        ) => match error {
            configuration_contract::InspectError::InvalidRequest => invalid_request(),
            configuration_contract::InspectError::NotFound => not_found(),
            configuration_contract::InspectError::Conflict => conflict(),
            configuration_contract::InspectError::ProposalMismatch => proposal_mismatch(),
            configuration_contract::InspectError::ProposalNotReady => proposal_not_ready(),
            configuration_contract::InspectError::Unknown(_) => unknown_rejection(),
        },
        configuration_contract::PluginConfigurationAuthorityInspectInvocationError::Runtime(
            error,
        ) => map_runtime_error(error),
    }
}

fn map_propose_error(
    error: configuration_contract::PluginConfigurationAuthorityProposeInvocationError,
) -> ExecuteError {
    match error {
        configuration_contract::PluginConfigurationAuthorityProposeInvocationError::Domain(
            error,
        ) => match error {
            configuration_contract::ProposeError::InvalidRequest => invalid_request(),
            configuration_contract::ProposeError::NotFound => not_found(),
            configuration_contract::ProposeError::Conflict => conflict(),
            configuration_contract::ProposeError::ProposalMismatch => proposal_mismatch(),
            configuration_contract::ProposeError::ProposalNotReady => proposal_not_ready(),
            configuration_contract::ProposeError::Unknown(_) => unknown_rejection(),
        },
        configuration_contract::PluginConfigurationAuthorityProposeInvocationError::Runtime(
            error,
        ) => map_runtime_error(error),
    }
}

fn map_publish_error(
    error: configuration_contract::PluginConfigurationAuthorityPublishInvocationError,
) -> ExecuteError {
    match error {
        configuration_contract::PluginConfigurationAuthorityPublishInvocationError::Domain(
            error,
        ) => match error {
            configuration_contract::PublishError::InvalidRequest => invalid_request(),
            configuration_contract::PublishError::NotFound => not_found(),
            configuration_contract::PublishError::Conflict => conflict(),
            configuration_contract::PublishError::ProposalMismatch => proposal_mismatch(),
            configuration_contract::PublishError::ProposalNotReady => proposal_not_ready(),
            configuration_contract::PublishError::Unknown(_) => unknown_rejection(),
        },
        configuration_contract::PluginConfigurationAuthorityPublishInvocationError::Runtime(
            error,
        ) => map_runtime_error(error),
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "map_err transfers the owned Runtime failure into the Tool domain"
)]
fn map_runtime_error(error: RuntimeFailure) -> ExecuteError {
    execution_failed(
        "configuration_authority_failed",
        &format!("Selected Plugin configuration authority failed: {error:?}"),
    )
}

fn execution_failed(reason_code: &str, message: &str) -> ExecuteError {
    ExecuteError::ExecutionFailed {
        payload: ExecutionFailedPayload {
            details_json: "{}".try_into().expect("static JSON must be valid"),
            message: message.to_owned(),
            reason_code: reason_code.to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_plugin_descriptor_requires_the_shared_authority_capability() {
        let descriptor: serde_json::Value = serde_json::from_str(PLUGIN_DESCRIPTOR_JSON).unwrap();
        assert_ne!(
            configuration_contract::CAPABILITY_ID,
            "lenso.agent.plugin-configuration@1",
            "the Host-private authority role must not collide with Console's cross-Agent capability"
        );
        assert_eq!(descriptor["plugin_id"], PLUGIN_PACKAGE_ID);
        assert_eq!(descriptor["root_slot"], "tool-providers");
        assert_eq!(
            descriptor["provided_capabilities"][0]["capability_id"],
            "lenso.agent.tool-provider@2"
        );
        assert_eq!(
            descriptor["required_capabilities"][0]["capability_id"],
            configuration_contract::CAPABILITY_ID
        );
        assert_eq!(descriptor["required_capabilities"][0]["cardinality"], "one");
    }
}
