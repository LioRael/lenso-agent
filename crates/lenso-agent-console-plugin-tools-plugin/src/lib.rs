//! Reviewed Plugin Root inspection and configuration tools for Console Agent.

use lenso::Port;
use lenso_agent_tool_sdk::prelude::*;
use lenso_capability_agent_plugin_management_target as target_contract;
use lenso_capability_agent_tool_provider::ExecutionFailedPayload;
use lenso_kernel::RuntimeFailure;
use schemars::JsonSchema;
use serde::Serialize;

pub const INSPECT_APP_TOOL: &str = "inspect_app";
pub const LIST_PLUGINS_TOOL: &str = "list_plugins";
pub const INSPECT_PLUGIN_TOOL: &str = "inspect_plugin";
pub const CHECK_PLUGIN_CHANGE_TOOL: &str = "check_plugin_change";
pub const APPLY_PLUGIN_CHANGE_TOOL: &str = "apply_plugin_change";
pub const LIST_PLUGIN_CHANGES_TOOL: &str = "list_plugin_changes";
pub const CHECK_PLUGIN_ROLLBACK_TOOL: &str = "check_plugin_rollback";
pub const APPLY_PLUGIN_ROLLBACK_TOOL: &str = "apply_plugin_rollback";
pub const SET_PLUGIN_ENABLED_TOOL: &str = "set_plugin_enabled";
pub const PLUGIN_PACKAGE_ID: &str = "lenso.agent.console-plugin-tools";

const MAX_CONFIGURATION_BYTES: usize = 7 * 1024;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ConsolePluginToolsConfig {
    max_output_bytes: usize,
}

#[derive(JsonSchema, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetArguments {
    #[schemars(length(min = 1, max = 64))]
    agent_id: String,
}

#[derive(JsonSchema, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ListPluginsArguments {
    #[schemars(length(min = 1, max = 64))]
    agent_id: String,
    #[schemars(length(max = 256))]
    query: Option<String>,
}

#[derive(JsonSchema, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct InspectPluginArguments {
    #[schemars(length(min = 1, max = 64))]
    agent_id: String,
    #[schemars(length(min = 1, max = 128))]
    plugin_id: String,
}

#[derive(JsonSchema, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckPluginChangeArguments {
    #[schemars(length(min = 1, max = 64))]
    agent_id: String,
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
    #[schemars(length(min = 1, max = 64))]
    agent_id: String,
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

#[derive(JsonSchema, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SetPluginEnabledArguments {
    #[schemars(length(min = 1, max = 64))]
    agent_id: String,
    enabled: bool,
    #[schemars(length(min = 71, max = 71))]
    expected_revision: String,
    #[schemars(length(min = 1, max = 128))]
    instance: String,
    #[schemars(length(min = 1, max = 128))]
    plugin_id: String,
}

#[derive(JsonSchema, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ListPluginChangesArguments {
    #[schemars(length(min = 1, max = 64))]
    agent_id: String,
    #[schemars(length(min = 1, max = 128))]
    instance: String,
    #[schemars(range(min = 1, max = 50))]
    limit: Option<u32>,
    #[schemars(length(min = 1, max = 128))]
    plugin_id: String,
}

#[derive(JsonSchema, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckPluginRollbackArguments {
    #[schemars(length(min = 1, max = 64))]
    agent_id: String,
    #[schemars(length(min = 71, max = 71))]
    expected_revision: String,
    #[schemars(length(min = 1, max = 128))]
    instance: String,
    #[schemars(length(min = 1, max = 128))]
    plugin_id: String,
    #[schemars(length(min = 71, max = 71))]
    publication_proposal_digest: String,
}

#[derive(JsonSchema, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyPluginRollbackArguments {
    #[schemars(length(min = 1, max = 64))]
    agent_id: String,
    #[schemars(length(min = 71, max = 71))]
    expected_revision: String,
    #[schemars(length(min = 1, max = 128))]
    instance: String,
    #[schemars(length(min = 1, max = 128))]
    plugin_id: String,
    #[schemars(length(min = 71, max = 71))]
    proposal_digest: String,
    #[schemars(length(min = 71, max = 71))]
    publication_proposal_digest: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AppInspection {
    agent_id: String,
    authority: target_contract::AuthoritySource,
    binding_count: i64,
    enabled_instance_count: i64,
    plugin_count: usize,
    revision: String,
    schema: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginList {
    agent_id: String,
    authority: target_contract::AuthoritySource,
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
    agent_id: String,
    authority: target_contract::AuthoritySource,
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
    agent_id: String,
    application: String,
    authority: target_contract::AuthoritySource,
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
    agent_id: String,
    authority: target_contract::AuthoritySource,
    base_revision: String,
    base_source_digest: String,
    proposal_digest: String,
    revision: String,
    schema: String,
    status: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SelectionInspection {
    agent_id: String,
    authority: target_contract::AuthoritySource,
    base_revision: String,
    enabled: bool,
    instance: String,
    plugin_id: String,
    revision: String,
    schema: String,
    status: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginChangeHistory {
    agent_id: String,
    authority: target_contract::AuthoritySource,
    instance: String,
    plugin_id: String,
    publications: Vec<PluginChangeSummary>,
    revision: String,
    schema: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginChangeSummary {
    base_revision: String,
    base_source_digest: Option<String>,
    proposal_digest: String,
    published_at_unix_ms: i64,
    revision: String,
    rollback_of_proposal_digest: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RollbackProposalInspection {
    agent_id: String,
    application: String,
    authority: target_contract::AuthoritySource,
    base_revision: String,
    base_source_digest: String,
    candidate_revision: String,
    diagnostics: Vec<ProposalDiagnostic>,
    instance: String,
    plugin_id: String,
    proposal_digest: String,
    rollback_of_proposal_digest: String,
    schema: String,
    status: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RollbackPublicationInspection {
    agent_id: String,
    authority: target_contract::AuthoritySource,
    base_revision: String,
    base_source_digest: String,
    proposal_digest: String,
    revision: String,
    rollback_of_proposal_digest: String,
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
    target: Port<target_contract::PluginManagementTargetClient>,
}

#[lenso_agent_tool_sdk::tool_provider]
impl ConsolePluginTools {
    #[tool(
        name = "inspect_app",
        description = "Inspect the Plugin configuration authority, desired revision, and resolved size of one exact target Agent.",
        execution = "parallel_safe"
    )]
    async fn inspect_app(
        &self,
        arguments: TargetArguments,
    ) -> Result<ExecuteResponse, ExecuteError> {
        let state = self
            .target
            .inspect(target_contract::InspectRequest {
                agent_id: arguments.agent_id,
            })
            .await
            .map_err(map_inspect_error)?;
        self.json_response(
            INSPECT_APP_TOOL,
            &AppInspection {
                agent_id: state.agent_id,
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
        description = "List Plugins visible through one exact target Agent's configuration authority.",
        execution = "parallel_safe"
    )]
    async fn list_plugins(
        &self,
        arguments: ListPluginsArguments,
    ) -> Result<ExecuteResponse, ExecuteError> {
        let state = self
            .target
            .inspect(target_contract::InspectRequest {
                agent_id: arguments.agent_id,
            })
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
                agent_id: state.agent_id,
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
        description = "Inspect one Plugin and its Instance differences through one exact target Agent's configuration authority.",
        execution = "parallel_safe"
    )]
    async fn inspect_plugin(
        &self,
        arguments: InspectPluginArguments,
    ) -> Result<ExecuteResponse, ExecuteError> {
        let state = self
            .target
            .inspect(target_contract::InspectRequest {
                agent_id: arguments.agent_id,
            })
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
                agent_id: state.agent_id,
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
        description = "Validate one exact Plugin configuration candidate through one exact target Agent's authority without publishing it.",
        execution = "parallel_safe"
    )]
    async fn check_plugin_change(
        &self,
        arguments: CheckPluginChangeArguments,
    ) -> Result<ExecuteResponse, ExecuteError> {
        ensure_configuration_size(&arguments.configuration_toml)?;
        let proposal = self
            .target
            .propose(target_contract::ProposeRequest {
                agent_id: arguments.agent_id,
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
                agent_id: proposal.agent_id,
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
        description = "Publish one reviewed Plugin configuration proposal through one exact target Agent's authority after exact revision and digest checks.",
        execution = "exclusive"
    )]
    async fn apply_plugin_change(
        &self,
        arguments: ApplyPluginChangeArguments,
    ) -> Result<ExecuteResponse, ExecuteError> {
        ensure_configuration_size(&arguments.configuration_toml)?;
        let publication = self
            .target
            .publish(target_contract::PublishRequest {
                agent_id: arguments.agent_id,
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
                agent_id: publication.agent_id,
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

    #[tool(
        name = "list_plugin_changes",
        description = "List bounded publication metadata for one exact Plugin Instance without exposing historical configuration contents.",
        execution = "parallel_safe"
    )]
    async fn list_plugin_changes(
        &self,
        arguments: ListPluginChangesArguments,
    ) -> Result<ExecuteResponse, ExecuteError> {
        let history = self
            .target
            .history(target_contract::HistoryRequest {
                agent_id: arguments.agent_id,
                instance: arguments.instance,
                limit: i64::from(arguments.limit.unwrap_or(10)),
                plugin_id: arguments.plugin_id,
            })
            .await
            .map_err(map_history_error)?;
        self.json_response(
            LIST_PLUGIN_CHANGES_TOOL,
            &PluginChangeHistory {
                agent_id: history.agent_id,
                authority: history.authority,
                instance: history.instance,
                plugin_id: history.plugin_id,
                publications: history
                    .publications
                    .into_iter()
                    .map(|publication| PluginChangeSummary {
                        base_revision: publication.base_revision,
                        base_source_digest: publication.base_source_digest.flatten(),
                        proposal_digest: publication.proposal_digest,
                        published_at_unix_ms: publication.published_at_unix_ms,
                        revision: publication.revision,
                        rollback_of_proposal_digest: publication
                            .rollback_of_proposal_digest
                            .flatten(),
                    })
                    .collect(),
                revision: history.revision,
                schema: history.schema,
            },
        )
    }

    #[tool(
        name = "check_plugin_rollback",
        description = "Validate rollback to one exact historical Plugin publication without exposing or publishing its configuration.",
        execution = "parallel_safe"
    )]
    async fn check_plugin_rollback(
        &self,
        arguments: CheckPluginRollbackArguments,
    ) -> Result<ExecuteResponse, ExecuteError> {
        let proposal = self
            .target
            .propose_rollback(target_contract::ProposeRollbackRequest {
                agent_id: arguments.agent_id,
                expected_revision: arguments.expected_revision,
                instance: arguments.instance,
                plugin_id: arguments.plugin_id,
                publication_proposal_digest: arguments.publication_proposal_digest,
            })
            .await
            .map_err(map_propose_rollback_error)?;
        self.json_response(
            CHECK_PLUGIN_ROLLBACK_TOOL,
            &RollbackProposalInspection {
                agent_id: proposal.agent_id,
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
                rollback_of_proposal_digest: proposal.rollback_of_proposal_digest,
                schema: proposal.schema,
                status: proposal.status,
            },
        )
    }

    #[tool(
        name = "apply_plugin_rollback",
        description = "Publish one reviewed rollback through one exact target Agent's authority after exact revision, publication, and proposal digest checks.",
        execution = "exclusive"
    )]
    async fn apply_plugin_rollback(
        &self,
        arguments: ApplyPluginRollbackArguments,
    ) -> Result<ExecuteResponse, ExecuteError> {
        let publication = self
            .target
            .publish_rollback(target_contract::PublishRollbackRequest {
                agent_id: arguments.agent_id,
                expected_revision: arguments.expected_revision,
                instance: arguments.instance,
                plugin_id: arguments.plugin_id,
                proposal_digest: arguments.proposal_digest,
                publication_proposal_digest: arguments.publication_proposal_digest,
            })
            .await
            .map_err(map_publish_rollback_error)?;
        self.json_response(
            APPLY_PLUGIN_ROLLBACK_TOOL,
            &RollbackPublicationInspection {
                agent_id: publication.agent_id,
                authority: publication.authority,
                base_revision: publication.base_revision,
                base_source_digest: publication.base_source_digest,
                proposal_digest: publication.proposal_digest,
                revision: publication.revision,
                rollback_of_proposal_digest: publication.rollback_of_proposal_digest,
                schema: publication.schema,
                status: "published_desired_state",
            },
        )
    }

    #[tool(
        name = "set_plugin_enabled",
        description = "Enable or disable one exact Plugin Instance through one exact target Agent's selected authority.",
        execution = "exclusive"
    )]
    async fn set_plugin_enabled(
        &self,
        arguments: SetPluginEnabledArguments,
    ) -> Result<ExecuteResponse, ExecuteError> {
        let publication = self
            .target
            .set_enabled(target_contract::SetEnabledRequest {
                agent_id: arguments.agent_id,
                enabled: arguments.enabled,
                expected_revision: arguments.expected_revision,
                instance: arguments.instance,
                plugin_id: arguments.plugin_id,
            })
            .await
            .map_err(map_selection_error)?;
        self.json_response(
            SET_PLUGIN_ENABLED_TOOL,
            &SelectionInspection {
                agent_id: publication.agent_id,
                authority: publication.authority,
                base_revision: publication.base_revision,
                enabled: publication.enabled,
                instance: publication.instance,
                plugin_id: publication.plugin_id,
                revision: publication.revision,
                schema: publication.schema,
                status: if publication.enabled {
                    "enabled"
                } else {
                    "disabled"
                },
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

fn publication_not_found() -> ExecuteError {
    execution_failed(
        "plugin_publication_not_found",
        "The selected Plugin configuration publication was not found.",
    )
}

fn map_selection_error(
    error: target_contract::PluginManagementTargetSetEnabledInvocationError,
) -> ExecuteError {
    match error {
        target_contract::PluginManagementTargetSetEnabledInvocationError::Domain(error) => {
            match error {
                target_contract::SetEnabledError::InvalidRequest => invalid_request(),
                target_contract::SetEnabledError::TargetNotFound => target_not_found(),
                target_contract::SetEnabledError::Unsupported => target_unsupported(),
                target_contract::SetEnabledError::PluginNotFound => not_found(),
                target_contract::SetEnabledError::Conflict => conflict(),
                target_contract::SetEnabledError::NotDisableable => execution_failed(
                    "plugin_not_disableable",
                    "The selected Plugin Instance is required by the Host and cannot be disabled.",
                ),
                target_contract::SetEnabledError::AlreadySelected => execution_failed(
                    "plugin_already_selected",
                    "The selected Plugin Instance already has the requested enabled state.",
                ),
                target_contract::SetEnabledError::PublicationNotFound => publication_not_found(),
                target_contract::SetEnabledError::ProposalMismatch
                | target_contract::SetEnabledError::ProposalNotReady
                | target_contract::SetEnabledError::Unknown(_) => execution_failed(
                    "plugin_selection_rejected",
                    "The selected Host authority rejected the Plugin selection operation.",
                ),
            }
        }
        target_contract::PluginManagementTargetSetEnabledInvocationError::Runtime(error) => {
            execution_failed(
                "plugin_selection_failed",
                &format!("Plugin selection authority failed: {error:?}"),
            )
        }
    }
}

fn unknown_rejection() -> ExecuteError {
    execution_failed(
        "configuration_authority_rejected",
        "The selected Plugin configuration authority rejected the operation.",
    )
}

fn target_not_found() -> ExecuteError {
    execution_failed(
        "agent_target_not_found",
        "The requested Agent identity is not present in the Console Agent catalog.",
    )
}

fn target_unsupported() -> ExecuteError {
    execution_failed(
        "agent_target_unsupported",
        "The requested Agent does not expose Plugin configuration control.",
    )
}

fn map_inspect_error(
    error: target_contract::PluginManagementTargetInspectInvocationError,
) -> ExecuteError {
    match error {
        target_contract::PluginManagementTargetInspectInvocationError::Domain(error) => match error
        {
            target_contract::InspectError::InvalidRequest => invalid_request(),
            target_contract::InspectError::TargetNotFound => target_not_found(),
            target_contract::InspectError::Unsupported => target_unsupported(),
            target_contract::InspectError::PluginNotFound => not_found(),
            target_contract::InspectError::Conflict => conflict(),
            target_contract::InspectError::ProposalMismatch => proposal_mismatch(),
            target_contract::InspectError::ProposalNotReady => proposal_not_ready(),
            target_contract::InspectError::PublicationNotFound => publication_not_found(),
            target_contract::InspectError::NotDisableable
            | target_contract::InspectError::AlreadySelected
            | target_contract::InspectError::Unknown(_) => unknown_rejection(),
        },
        target_contract::PluginManagementTargetInspectInvocationError::Runtime(error) => {
            map_runtime_error(error)
        }
    }
}

fn map_propose_error(
    error: target_contract::PluginManagementTargetProposeInvocationError,
) -> ExecuteError {
    match error {
        target_contract::PluginManagementTargetProposeInvocationError::Domain(error) => match error
        {
            target_contract::ProposeError::InvalidRequest => invalid_request(),
            target_contract::ProposeError::TargetNotFound => target_not_found(),
            target_contract::ProposeError::Unsupported => target_unsupported(),
            target_contract::ProposeError::PluginNotFound => not_found(),
            target_contract::ProposeError::Conflict => conflict(),
            target_contract::ProposeError::ProposalMismatch => proposal_mismatch(),
            target_contract::ProposeError::ProposalNotReady => proposal_not_ready(),
            target_contract::ProposeError::PublicationNotFound => publication_not_found(),
            target_contract::ProposeError::NotDisableable
            | target_contract::ProposeError::AlreadySelected
            | target_contract::ProposeError::Unknown(_) => unknown_rejection(),
        },
        target_contract::PluginManagementTargetProposeInvocationError::Runtime(error) => {
            map_runtime_error(error)
        }
    }
}

fn map_publish_error(
    error: target_contract::PluginManagementTargetPublishInvocationError,
) -> ExecuteError {
    match error {
        target_contract::PluginManagementTargetPublishInvocationError::Domain(error) => match error
        {
            target_contract::PublishError::InvalidRequest => invalid_request(),
            target_contract::PublishError::TargetNotFound => target_not_found(),
            target_contract::PublishError::Unsupported => target_unsupported(),
            target_contract::PublishError::PluginNotFound => not_found(),
            target_contract::PublishError::Conflict => conflict(),
            target_contract::PublishError::ProposalMismatch => proposal_mismatch(),
            target_contract::PublishError::ProposalNotReady => proposal_not_ready(),
            target_contract::PublishError::PublicationNotFound => publication_not_found(),
            target_contract::PublishError::NotDisableable
            | target_contract::PublishError::AlreadySelected
            | target_contract::PublishError::Unknown(_) => unknown_rejection(),
        },
        target_contract::PluginManagementTargetPublishInvocationError::Runtime(error) => {
            map_runtime_error(error)
        }
    }
}

fn map_history_error(
    error: target_contract::PluginManagementTargetHistoryInvocationError,
) -> ExecuteError {
    match error {
        target_contract::PluginManagementTargetHistoryInvocationError::Domain(error) => match error
        {
            target_contract::HistoryError::InvalidRequest => invalid_request(),
            target_contract::HistoryError::TargetNotFound => target_not_found(),
            target_contract::HistoryError::Unsupported => target_unsupported(),
            target_contract::HistoryError::PluginNotFound => not_found(),
            target_contract::HistoryError::Conflict => conflict(),
            target_contract::HistoryError::PublicationNotFound => publication_not_found(),
            target_contract::HistoryError::ProposalMismatch => proposal_mismatch(),
            target_contract::HistoryError::ProposalNotReady => proposal_not_ready(),
            target_contract::HistoryError::NotDisableable
            | target_contract::HistoryError::AlreadySelected
            | target_contract::HistoryError::Unknown(_) => unknown_rejection(),
        },
        target_contract::PluginManagementTargetHistoryInvocationError::Runtime(error) => {
            map_runtime_error(error)
        }
    }
}

fn map_propose_rollback_error(
    error: target_contract::PluginManagementTargetProposeRollbackInvocationError,
) -> ExecuteError {
    match error {
        target_contract::PluginManagementTargetProposeRollbackInvocationError::Domain(error) => {
            match error {
                target_contract::ProposeRollbackError::InvalidRequest => invalid_request(),
                target_contract::ProposeRollbackError::TargetNotFound => target_not_found(),
                target_contract::ProposeRollbackError::Unsupported => target_unsupported(),
                target_contract::ProposeRollbackError::PluginNotFound => not_found(),
                target_contract::ProposeRollbackError::Conflict => conflict(),
                target_contract::ProposeRollbackError::PublicationNotFound => {
                    publication_not_found()
                }
                target_contract::ProposeRollbackError::ProposalMismatch => proposal_mismatch(),
                target_contract::ProposeRollbackError::ProposalNotReady => proposal_not_ready(),
                target_contract::ProposeRollbackError::NotDisableable
                | target_contract::ProposeRollbackError::AlreadySelected
                | target_contract::ProposeRollbackError::Unknown(_) => unknown_rejection(),
            }
        }
        target_contract::PluginManagementTargetProposeRollbackInvocationError::Runtime(error) => {
            map_runtime_error(error)
        }
    }
}

fn map_publish_rollback_error(
    error: target_contract::PluginManagementTargetPublishRollbackInvocationError,
) -> ExecuteError {
    match error {
        target_contract::PluginManagementTargetPublishRollbackInvocationError::Domain(error) => {
            match error {
                target_contract::PublishRollbackError::InvalidRequest => invalid_request(),
                target_contract::PublishRollbackError::TargetNotFound => target_not_found(),
                target_contract::PublishRollbackError::Unsupported => target_unsupported(),
                target_contract::PublishRollbackError::PluginNotFound => not_found(),
                target_contract::PublishRollbackError::Conflict => conflict(),
                target_contract::PublishRollbackError::PublicationNotFound => {
                    publication_not_found()
                }
                target_contract::PublishRollbackError::ProposalMismatch => proposal_mismatch(),
                target_contract::PublishRollbackError::ProposalNotReady => proposal_not_ready(),
                target_contract::PublishRollbackError::NotDisableable
                | target_contract::PublishRollbackError::AlreadySelected
                | target_contract::PublishRollbackError::Unknown(_) => unknown_rejection(),
            }
        }
        target_contract::PluginManagementTargetPublishRollbackInvocationError::Runtime(error) => {
            map_runtime_error(error)
        }
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
    fn generated_plugin_descriptor_requires_the_target_routing_capability() {
        let descriptor: serde_json::Value = serde_json::from_str(PLUGIN_DESCRIPTOR_JSON).unwrap();
        assert_ne!(
            target_contract::CAPABILITY_ID,
            "lenso.agent.plugin-configuration@1",
            "the Host-private target role must not collide with Console's advertised capability"
        );
        assert_eq!(descriptor["plugin_id"], PLUGIN_PACKAGE_ID);
        assert_eq!(descriptor["root_slot"], "tool-providers");
        assert_eq!(
            descriptor["provided_capabilities"][0]["capability_id"],
            "lenso.agent.tool-provider@2"
        );
        let requirements = descriptor["required_capabilities"].as_array().unwrap();
        let requirement = requirements
            .iter()
            .find(|requirement| requirement["capability_id"] == target_contract::CAPABILITY_ID)
            .expect("Console Plugin Tools should require one target router");
        assert_eq!(requirement["cardinality"], "one");
    }
}
