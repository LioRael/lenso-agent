use std::{
    collections::BTreeMap,
    fmt,
    net::IpAddr,
    sync::{Arc, RwLock},
    time::Duration,
};

use lenso_agent_host::{
    AgentToolTarget, PluginManagementTarget, agent_tool_target_contract as tool_contract,
};
use lenso_capability_agent_plugin_management_target as contract;
use lenso_kernel::RuntimeFailure;
use reqwest::{Method, Response, StatusCode, Url};
use serde::{Deserialize, de::DeserializeOwned};

use lenso_agent_web::{
    AgentToolCatalogResponse, AgentToolExecuteRequest, AgentToolExecuteResponse,
    AgentToolExecutionClass,
};

#[derive(Clone, Debug)]
pub(super) struct AppAgentAdapter {
    client: reqwest::Client,
    id: String,
    origin: reqwest::Url,
    plugin_configuration: bool,
    plugin_lifecycle: bool,
}

impl AppAgentAdapter {
    pub(super) fn parse(value: &str) -> Result<Self, String> {
        let (id, origin) = value
            .split_once('=')
            .ok_or_else(|| "managed Agent must use ID=URL".to_owned())?;
        if id.is_empty()
            || id == "console"
            || !id.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
            })
        {
            return Err("managed Agent identity is invalid".to_owned());
        }
        let origin = reqwest::Url::parse(origin.trim())
            .map_err(|error| format!("managed Agent URL is invalid: {error}"))?;
        let loopback = origin.host_str().is_some_and(|host| {
            host == "localhost" || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
        });
        if origin.scheme() != "http"
            || !loopback
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
        {
            return Err("managed Agent URL must be a clean loopback HTTP origin".to_owned());
        }
        let mut headers = reqwest::header::HeaderMap::new();
        if let Ok(token) = std::env::var(lenso_agent_web::CONTROL_TOKEN_ENV)
            && !token.trim().is_empty()
        {
            headers.insert(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {token}")
                    .parse()
                    .map_err(|error| format!("managed Agent control token is invalid: {error}"))?,
            );
        }
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(3))
            .redirect(reqwest::redirect::Policy::none())
            .default_headers(headers)
            .build()
            .map_err(|error| format!("managed Agent client is invalid: {error}"))?;
        Ok(Self {
            client,
            id: id.to_owned(),
            origin,
            plugin_configuration: true,
            plugin_lifecycle: true,
        })
    }
}

const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
struct FrozenToolRoute {
    agent: AppAgentAdapter,
    generation: String,
    target_name: String,
}

#[derive(Clone, Debug, Default)]
struct FrozenToolCatalog {
    routes: BTreeMap<String, FrozenToolRoute>,
    tools: Vec<tool_contract::ToolDefinition>,
}

#[derive(Clone)]
pub(super) struct AppAgentToolTarget {
    agents: Vec<AppAgentAdapter>,
    frozen: Arc<RwLock<Option<FrozenToolCatalog>>>,
}

impl AppAgentToolTarget {
    pub(super) fn new(agents: Vec<AppAgentAdapter>) -> Self {
        Self {
            agents,
            frozen: Arc::new(RwLock::new(None)),
        }
    }

    pub(super) async fn prepare(&self) -> Result<Vec<String>, RuntimeFailure> {
        let mut frozen = FrozenToolCatalog::default();
        for agent in &self.agents {
            let catalog: AgentToolCatalogResponse =
                match get_json(agent, &["tools"], "read Tool catalog").await {
                    Ok(catalog) => catalog,
                    Err(_) => continue,
                };
            for tool in catalog.tools {
                let name = proxy_tool_name(&agent.id, &tool.name)?;
                let route = FrozenToolRoute {
                    agent: agent.clone(),
                    generation: catalog.generation.clone(),
                    target_name: tool.name,
                };
                if frozen.routes.insert(name.clone(), route).is_some() {
                    return Err(runtime_failure(format!(
                        "App Agent Tool name `{name}` is ambiguous"
                    )));
                }
                frozen.tools.push(tool_contract::ToolDefinition {
                    description: format!("On App Agent `{}`: {}", agent.id, tool.description),
                    execution: match tool.execution {
                        AgentToolExecutionClass::ParallelSafe => {
                            tool_contract::ToolExecutionClass::ParallelSafe
                        }
                        AgentToolExecutionClass::Exclusive => {
                            tool_contract::ToolExecutionClass::Exclusive
                        }
                    },
                    input_schema_json: tool.input_schema_json,
                    name,
                });
            }
        }
        frozen
            .tools
            .sort_by(|left, right| left.name.cmp(&right.name));
        let names = frozen.tools.iter().map(|tool| tool.name.clone()).collect();
        *self
            .frozen
            .write()
            .map_err(|_| runtime_failure("App Agent Tool catalog lock is unavailable"))? =
            Some(frozen);
        Ok(names)
    }
}

impl fmt::Debug for AppAgentToolTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppAgentToolTarget")
            .field(
                "agent_ids",
                &self
                    .agents
                    .iter()
                    .map(|agent| &agent.id)
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl AgentToolTarget for AppAgentToolTarget {
    fn catalog(
        &self,
        _request: tool_contract::CatalogRequest,
    ) -> lenso_kernel::NativeRequestFuture<tool_contract::ToolTargetCatalog> {
        let result = self
            .frozen
            .read()
            .map_err(|_| runtime_failure("App Agent Tool catalog lock is unavailable"))
            .and_then(|catalog| {
                catalog
                    .as_ref()
                    .cloned()
                    .ok_or_else(|| runtime_failure("App Agent Tool catalog was not prepared"))
            })
            .map(|catalog| {
                Ok(tool_contract::CatalogResponse {
                    tools: catalog.tools,
                })
            });
        Box::pin(async move { result })
    }

    fn execute(
        &self,
        request: tool_contract::ExecuteRequest,
    ) -> lenso_kernel::NativeRequestFuture<tool_contract::ToolTargetExecute> {
        let route = self
            .frozen
            .read()
            .map_err(|_| runtime_failure("App Agent Tool catalog lock is unavailable"))
            .map(|catalog| {
                catalog
                    .as_ref()
                    .and_then(|catalog| catalog.routes.get(&request.name))
                    .cloned()
            });
        Box::pin(async move {
            let Some(route) = route? else {
                return Ok(Err(tool_contract::ExecuteError::ToolNotFound));
            };
            let body = serde_json::to_value(AgentToolExecuteRequest {
                arguments_json: request.arguments_json,
                expected_generation: route.generation,
                name: route.target_name,
            })
            .map_err(|error| runtime_failure(format!("Tool request failed: {error}")))?;
            let response: AgentToolExecuteResponse = send_json(
                &route.agent,
                Method::POST,
                &["tools", "execute"],
                body,
                "execute Tool",
            )
            .await
            .map_err(|error| match error {
                TargetRequestError::Domain(status) => {
                    runtime_failure(format!("App Agent Tool execution failed with {status}"))
                }
                TargetRequestError::Runtime(error) => error,
            })?;
            Ok(match response {
                AgentToolExecuteResponse::Succeeded { response } => {
                    let response_json = serde_json::to_string(&response)
                        .map_err(|error| runtime_failure(format!("Tool response failed: {error}")))?
                        .parse()
                        .map_err(|error| {
                            runtime_failure(format!("Tool response failed: {error}"))
                        })?;
                    Ok(tool_contract::ExecuteResponse { response_json })
                }
                AgentToolExecuteResponse::StaleCatalog { .. } => {
                    Err(tool_contract::ExecuteError::StaleCatalog)
                }
                AgentToolExecuteResponse::Failed {
                    code,
                    details_json,
                    message,
                } => Err(match code.as_str() {
                    "invalid_arguments" => tool_contract::ExecuteError::InvalidRequest,
                    "unknown_tool" => tool_contract::ExecuteError::ToolNotFound,
                    "tool_not_allowed" => tool_contract::ExecuteError::PermissionDenied,
                    _ => tool_contract::ExecuteError::ExecutionFailed {
                        payload: tool_contract::ExecutionFailedPayload {
                            details_json,
                            message,
                            reason_code: code,
                        },
                    },
                }),
            })
        })
    }
}

fn proxy_tool_name(agent_id: &str, tool_name: &str) -> Result<String, RuntimeFailure> {
    let agent = agent_id
        .chars()
        .map(|character| {
            if character.is_ascii_lowercase() || character.is_ascii_digit() {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    let name = format!("app_{agent}_{tool_name}");
    if name.len() > 64 {
        return Err(runtime_failure(format!(
            "App Agent Tool name `{name}` exceeds the model Tool name limit"
        )));
    }
    Ok(name)
}

#[derive(Clone)]
pub(super) struct AppAgentPluginManagementTarget {
    agents: Vec<AppAgentAdapter>,
}

impl AppAgentPluginManagementTarget {
    pub(super) fn new(agents: Vec<AppAgentAdapter>) -> Self {
        Self { agents }
    }

    fn configuration_target<E>(
        &self,
        agent_id: &str,
        missing: E,
        unsupported: E,
    ) -> Result<AppAgentAdapter, E> {
        let Some(agent) = self.agents.iter().find(|agent| agent.id == agent_id) else {
            return Err(missing);
        };
        if !agent.plugin_configuration {
            return Err(unsupported);
        }
        Ok(agent.clone())
    }

    fn lifecycle_target<E>(
        &self,
        agent_id: &str,
        missing: E,
        unsupported: E,
    ) -> Result<AppAgentAdapter, E> {
        let Some(agent) = self.agents.iter().find(|agent| agent.id == agent_id) else {
            return Err(missing);
        };
        if !agent.plugin_lifecycle {
            return Err(unsupported);
        }
        Ok(agent.clone())
    }
}

impl fmt::Debug for AppAgentPluginManagementTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AppAgentPluginManagementTarget")
            .field(
                "agent_ids",
                &self
                    .agents
                    .iter()
                    .map(|agent| &agent.id)
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl PluginManagementTarget for AppAgentPluginManagementTarget {
    fn history(
        &self,
        request: contract::HistoryRequest,
    ) -> lenso_kernel::NativeRequestFuture<contract::PluginManagementTargetHistory> {
        let target = self.configuration_target(
            &request.agent_id,
            contract::HistoryError::TargetNotFound,
            contract::HistoryError::Unsupported,
        );
        Box::pin(async move {
            let agent = match target {
                Ok(agent) => agent,
                Err(error) => return Ok(Err(error)),
            };
            let management: ManagementResponse =
                get_json(&agent, &["control", "plugins"], "inspect before history").await?;
            validate_management(&management)?;
            if management.configuration_authority.publication_history != Some(true) {
                return Ok(Err(contract::HistoryError::Unsupported));
            }
            if !has_managed_instance(&management, &request.plugin_id, &request.instance) {
                return Ok(Err(contract::HistoryError::PluginNotFound));
            }
            let history: HistoryResponse = match get_json_domain(
                &agent,
                &[
                    "control",
                    "plugins",
                    &request.plugin_id,
                    &request.instance,
                    "configuration",
                    "publications",
                ],
                "read publication history",
            )
            .await
            {
                Ok(value) => value,
                Err(TargetRequestError::Domain(status)) => {
                    return Ok(Err(map_history_status(status)));
                }
                Err(TargetRequestError::Runtime(error)) => return Err(error),
            };
            Ok(Ok(history_contract(request, management, history)?))
        })
    }

    fn inspect(
        &self,
        request: contract::InspectRequest,
    ) -> lenso_kernel::NativeRequestFuture<contract::PluginManagementTargetInspect> {
        let target = self.configuration_target(
            &request.agent_id,
            contract::InspectError::TargetNotFound,
            contract::InspectError::Unsupported,
        );
        Box::pin(async move {
            let agent = match target {
                Ok(agent) => agent,
                Err(error) => return Ok(Err(error)),
            };
            let management: ManagementResponse =
                get_json(&agent, &["control", "plugins"], "inspect").await?;
            validate_management(&management)?;
            Ok(Ok(contract::InspectResponse {
                agent_id: request.agent_id,
                authority: management.configuration_authority.into(),
                binding_count: management.binding_count,
                enabled_instance_count: bounded_count(
                    management
                        .plugins
                        .iter()
                        .flat_map(|plugin| &plugin.instances)
                        .filter(|instance| instance.selection == "enabled")
                        .count(),
                    65_536,
                ),
                plugins: management
                    .plugins
                    .into_iter()
                    .map(|plugin| contract::PluginInspection {
                        instances: plugin
                            .instances
                            .into_iter()
                            .map(|instance| contract::PluginInstanceInspection {
                                disableable: instance.disableable,
                                has_root_difference: instance.has_root_difference,
                                instance_key: instance.instance_key,
                                origin: instance.origin,
                                root_configuration_bytes: instance
                                    .root_configuration_toml
                                    .as_ref()
                                    .map_or(0, |value| bounded_count(value.len(), 262_144)),
                                root_configuration_present: instance
                                    .root_configuration_toml
                                    .is_some(),
                                selection: instance.selection,
                                source_digest: instance.source_digest,
                            })
                            .collect(),
                        package_id: plugin.package_id,
                        package_revision: plugin.package_revision,
                        source: if plugin.root_supplied {
                            "plugin-root".to_owned()
                        } else {
                            "host-build".to_owned()
                        },
                    })
                    .collect(),
                revision: management.revision,
            }))
        })
    }

    fn propose(
        &self,
        request: contract::ProposeRequest,
    ) -> lenso_kernel::NativeRequestFuture<contract::PluginManagementTargetPropose> {
        let target = self.configuration_target(
            &request.agent_id,
            contract::ProposeError::TargetNotFound,
            contract::ProposeError::Unsupported,
        );
        Box::pin(async move {
            let agent = match target {
                Ok(agent) => agent,
                Err(error) => return Ok(Err(error)),
            };
            let fence = match configuration_fence(
                &agent,
                &request.plugin_id,
                &request.instance,
                &request.expected_revision,
            )
            .await?
            {
                Ok(fence) => fence,
                Err(error) => return Ok(Err(map_propose_fence(error))),
            };
            let proposal: ProposalResponse = match send_json(
                &agent,
                Method::POST,
                &[
                    "control",
                    "plugins",
                    &request.plugin_id,
                    &request.instance,
                    "configuration",
                    "proposals",
                ],
                serde_json::json!({
                    "expectedRevision": request.expected_revision,
                    "expectedSourceDigest": fence.source_digest,
                    "expectedStreamId": fence.stream_id,
                    "toml": request.configuration_toml,
                }),
                "propose",
            )
            .await
            {
                Ok(value) => value,
                Err(TargetRequestError::Domain(status)) => {
                    return Ok(Err(map_propose_status(status)));
                }
                Err(TargetRequestError::Runtime(error)) => return Err(error),
            };
            validate_proposal(&proposal, &request, &fence)?;
            Ok(Ok(proposal.into_contract(request.agent_id)))
        })
    }

    fn propose_rollback(
        &self,
        request: contract::ProposeRollbackRequest,
    ) -> lenso_kernel::NativeRequestFuture<contract::PluginManagementTargetProposeRollback> {
        let target = self.configuration_target(
            &request.agent_id,
            contract::ProposeRollbackError::TargetNotFound,
            contract::ProposeRollbackError::Unsupported,
        );
        Box::pin(async move {
            let agent = match target {
                Ok(agent) => agent,
                Err(error) => return Ok(Err(error)),
            };
            let fence = match configuration_fence(
                &agent,
                &request.plugin_id,
                &request.instance,
                &request.expected_revision,
            )
            .await?
            {
                Ok(fence) => fence,
                Err(error) => return Ok(Err(map_propose_rollback_fence(error))),
            };
            if !fence.rollback_proposals {
                return Ok(Err(contract::ProposeRollbackError::Unsupported));
            }
            let rollback = match request_rollback_proposal(&agent, &request, &fence).await {
                Ok(value) => value,
                Err(TargetRequestError::Domain(status)) => {
                    return Ok(Err(map_propose_rollback_status(status)));
                }
                Err(TargetRequestError::Runtime(error)) => return Err(error),
            };
            validate_rollback_proposal(&rollback, &request, &fence)?;
            Ok(Ok(rollback.into_contract(request.agent_id)))
        })
    }

    fn publish(
        &self,
        request: contract::PublishRequest,
    ) -> lenso_kernel::NativeRequestFuture<contract::PluginManagementTargetPublish> {
        let target = self.configuration_target(
            &request.agent_id,
            contract::PublishError::TargetNotFound,
            contract::PublishError::Unsupported,
        );
        Box::pin(async move {
            let agent = match target {
                Ok(agent) => agent,
                Err(error) => return Ok(Err(error)),
            };
            let fence = match configuration_fence(
                &agent,
                &request.plugin_id,
                &request.instance,
                &request.expected_revision,
            )
            .await?
            {
                Ok(fence) => fence,
                Err(error) => return Ok(Err(map_publish_fence(error))),
            };
            let publication: PublicationResponse = match send_json(
                &agent,
                Method::PUT,
                &[
                    "control",
                    "plugins",
                    &request.plugin_id,
                    &request.instance,
                    "configuration",
                ],
                serde_json::json!({
                    "expectedRevision": request.expected_revision,
                    "expectedSourceDigest": fence.source_digest,
                    "expectedStreamId": fence.stream_id,
                    "proposalDigest": request.proposal_digest,
                    "toml": request.configuration_toml,
                }),
                "publish",
            )
            .await
            {
                Ok(value) => value,
                Err(TargetRequestError::Domain(status)) => {
                    return Ok(Err(map_publish_status(status)));
                }
                Err(TargetRequestError::Runtime(error)) => return Err(error),
            };
            validate_publication(&publication, &request, &fence)?;
            Ok(Ok(contract::PublishResponse {
                agent_id: request.agent_id,
                authority: publication.configuration_authority.into(),
                base_revision: publication.base_revision,
                base_source_digest: publication.base_source_digest,
                proposal_digest: publication.proposal_digest,
                revision: publication.revision,
                schema: publication.publication_schema,
            }))
        })
    }

    fn publish_rollback(
        &self,
        request: contract::PublishRollbackRequest,
    ) -> lenso_kernel::NativeRequestFuture<contract::PluginManagementTargetPublishRollback> {
        let target = self.configuration_target(
            &request.agent_id,
            contract::PublishRollbackError::TargetNotFound,
            contract::PublishRollbackError::Unsupported,
        );
        Box::pin(async move {
            let agent = match target {
                Ok(agent) => agent,
                Err(error) => return Ok(Err(error)),
            };
            let fence = match configuration_fence(
                &agent,
                &request.plugin_id,
                &request.instance,
                &request.expected_revision,
            )
            .await?
            {
                Ok(fence) => fence,
                Err(error) => return Ok(Err(map_publish_rollback_fence(error))),
            };
            if !fence.rollback_proposals {
                return Ok(Err(contract::PublishRollbackError::Unsupported));
            }
            let proposal_request = contract::ProposeRollbackRequest {
                agent_id: request.agent_id.clone(),
                expected_revision: request.expected_revision.clone(),
                instance: request.instance.clone(),
                plugin_id: request.plugin_id.clone(),
                publication_proposal_digest: request.publication_proposal_digest.clone(),
            };
            let rollback = match request_rollback_proposal(&agent, &proposal_request, &fence).await
            {
                Ok(value) => value,
                Err(TargetRequestError::Domain(status)) => {
                    return Ok(Err(map_publish_rollback_status(status)));
                }
                Err(TargetRequestError::Runtime(error)) => return Err(error),
            };
            validate_rollback_proposal(&rollback, &proposal_request, &fence)?;
            if rollback.proposal.proposal_digest != request.proposal_digest {
                return Ok(Err(contract::PublishRollbackError::ProposalMismatch));
            }
            if rollback.proposal.status != "ready" || rollback.proposal.application == "blocked" {
                return Ok(Err(contract::PublishRollbackError::ProposalNotReady));
            }
            let publication: PublicationResponse = match send_json(
                &agent,
                Method::PUT,
                &[
                    "control",
                    "plugins",
                    &request.plugin_id,
                    &request.instance,
                    "configuration",
                ],
                serde_json::json!({
                    "expectedRevision": request.expected_revision,
                    "expectedSourceDigest": fence.source_digest,
                    "expectedStreamId": fence.stream_id,
                    "proposalDigest": request.proposal_digest,
                    "rollbackOfProposalDigest": request.publication_proposal_digest,
                    "toml": rollback.configuration_toml,
                }),
                "publish rollback",
            )
            .await
            {
                Ok(value) => value,
                Err(TargetRequestError::Domain(status)) => {
                    return Ok(Err(map_publish_rollback_status(status)));
                }
                Err(TargetRequestError::Runtime(error)) => return Err(error),
            };
            validate_rollback_publication(&publication, &request, &fence)?;
            Ok(Ok(contract::PublishRollbackResponse {
                agent_id: request.agent_id,
                authority: publication.configuration_authority.into(),
                base_revision: publication.base_revision,
                base_source_digest: publication.base_source_digest,
                proposal_digest: publication.proposal_digest,
                revision: publication.revision,
                rollback_of_proposal_digest: request.publication_proposal_digest,
                schema: publication.publication_schema,
            }))
        })
    }

    fn set_enabled(
        &self,
        request: contract::SetEnabledRequest,
    ) -> lenso_kernel::NativeRequestFuture<contract::PluginManagementTargetSetEnabled> {
        let target = self.configuration_target(
            &request.agent_id,
            contract::SetEnabledError::TargetNotFound,
            contract::SetEnabledError::Unsupported,
        );
        Box::pin(async move {
            let agent = match target {
                Ok(agent) => agent,
                Err(error) => return Ok(Err(error)),
            };
            let management: ManagementResponse =
                get_json(&agent, &["control", "plugins"], "inspect before selection").await?;
            validate_management(&management)?;
            if management.revision != request.expected_revision {
                return Ok(Err(contract::SetEnabledError::Conflict));
            }
            let Some(instance) = management
                .plugins
                .iter()
                .find(|plugin| plugin.package_id == request.plugin_id)
                .and_then(|plugin| {
                    plugin
                        .instances
                        .iter()
                        .find(|instance| instance.instance_key == request.instance)
                })
            else {
                return Ok(Err(contract::SetEnabledError::PluginNotFound));
            };
            if !request.enabled && !instance.disableable {
                return Ok(Err(contract::SetEnabledError::NotDisableable));
            }
            if (instance.selection == "enabled") == request.enabled {
                return Ok(Err(contract::SetEnabledError::AlreadySelected));
            }
            let Some(authority) = management.selection_authority else {
                return Ok(Err(contract::SetEnabledError::Unsupported));
            };
            let inventory: InventoryResponse =
                get_json(&agent, &["plugins"], "read stream").await?;
            validate_inventory(&inventory)?;
            let receipt: MutationResponse = match send_json(
                &agent,
                Method::PUT,
                &[
                    "control",
                    "plugins",
                    &request.plugin_id,
                    &request.instance,
                    "enabled",
                ],
                serde_json::json!({
                    "enabled": request.enabled,
                    "expectedRevision": request.expected_revision,
                    "expectedStreamId": inventory.stream_id,
                }),
                "set enabled",
            )
            .await
            {
                Ok(value) => value,
                Err(TargetRequestError::Domain(status)) => {
                    return Ok(Err(map_selection_status(status)));
                }
                Err(TargetRequestError::Runtime(error)) => return Err(error),
            };
            if receipt.schema != "lenso.agent.plugin-operation.v1"
                || receipt.stream_id != inventory.stream_id
                || receipt.desired.plugin_root_revision.is_empty()
            {
                return Err(protocol_failure(
                    "Agent Host returned an invalid selection receipt",
                ));
            }
            Ok(Ok(contract::SetEnabledResponse {
                agent_id: request.agent_id,
                authority: authority.into(),
                base_revision: request.expected_revision,
                enabled: request.enabled,
                instance: request.instance,
                plugin_id: request.plugin_id,
                revision: receipt.desired.plugin_root_revision,
                schema: "lenso.plugin-selection-publication.v1".to_owned(),
            }))
        })
    }

    fn catalog(
        &self,
        request: contract::CatalogRequest,
    ) -> lenso_kernel::NativeRequestFuture<contract::PluginManagementTargetCatalog> {
        let target = self.lifecycle_target(
            &request.agent_id,
            contract::CatalogError::TargetNotFound,
            contract::CatalogError::Unsupported,
        );
        Box::pin(async move {
            let agent = match target {
                Ok(agent) => agent,
                Err(error) => return Ok(Err(error)),
            };
            let response: TrustedCatalogResponse = match get_json_domain(
                &agent,
                &["control", "plugins", "trusted-catalog"],
                "list trusted Plugins",
            )
            .await
            {
                Ok(value) => value,
                Err(TargetRequestError::Domain(status)) => {
                    return Ok(Err(map_lifecycle_status(status)));
                }
                Err(TargetRequestError::Runtime(error)) => return Err(error),
            };
            if response.schema != "lenso.agent.trusted-plugin-catalog.v1"
                || response.revision.len() != 71
            {
                return Err(protocol_failure(
                    "Agent Host returned an invalid trusted Plugin catalog",
                ));
            }
            let folded = request.query.to_lowercase();
            Ok(Ok(contract::CatalogResponse {
                agent_id: request.agent_id,
                authority: response.authority.into(),
                entries: response
                    .entries
                    .into_iter()
                    .filter(|entry| {
                        folded.is_empty()
                            || entry.catalog_entry_id.to_lowercase().contains(&folded)
                            || entry.package_id.to_lowercase().contains(&folded)
                    })
                    .map(|entry| contract::CatalogEntry {
                        catalog_entry_id: entry.catalog_entry_id,
                        package_id: entry.package_id,
                        package_revision: entry.package_revision,
                        source_digest: entry.source_digest,
                    })
                    .collect(),
                query: request.query,
                revision: response.revision,
            }))
        })
    }

    fn propose_install(
        &self,
        request: contract::ProposeInstallRequest,
    ) -> lenso_kernel::NativeRequestFuture<contract::PluginManagementTargetProposeInstall> {
        let target = self.lifecycle_target(
            &request.agent_id,
            contract::ProposeInstallError::TargetNotFound,
            contract::ProposeInstallError::Unsupported,
        );
        Box::pin(async move {
            let agent = match target {
                Ok(agent) => agent,
                Err(error) => return Ok(Err(error)),
            };
            let response: InstallProposalResponse = match send_json(&agent, Method::POST, &["control", "plugin-installations", "proposals"], serde_json::json!({"catalogEntryId": request.catalog_entry_id, "expectedRevision": request.expected_revision}), "propose install").await {
                Ok(value) => value,
                Err(TargetRequestError::Domain(status)) => return Ok(Err(map_propose_install_status(status))),
                Err(TargetRequestError::Runtime(error)) => return Err(error),
            };
            validate_install_proposal(&response, &request)?;
            Ok(Ok(response.into_contract(request.agent_id)))
        })
    }

    fn publish_install(
        &self,
        request: contract::PublishInstallRequest,
    ) -> lenso_kernel::NativeRequestFuture<contract::PluginManagementTargetPublishInstall> {
        let target = self.lifecycle_target(
            &request.agent_id,
            contract::PublishInstallError::TargetNotFound,
            contract::PublishInstallError::Unsupported,
        );
        Box::pin(async move {
            let agent = match target {
                Ok(agent) => agent,
                Err(error) => return Ok(Err(error)),
            };
            let checked: InstallProposalResponse = match send_json(&agent, Method::POST, &["control", "plugin-installations", "proposals"], serde_json::json!({"catalogEntryId": request.catalog_entry_id, "expectedRevision": request.expected_revision}), "recheck install").await {
                Ok(value) => value,
                Err(TargetRequestError::Domain(status)) => return Ok(Err(map_publish_install_status(status))),
                Err(TargetRequestError::Runtime(error)) => return Err(error),
            };
            if checked.proposal_digest != request.proposal_digest {
                return Ok(Err(contract::PublishInstallError::ProposalMismatch));
            }
            let response: InstallPublicationResponse = match send_json(&agent, Method::POST, &["control", "plugin-installations", "publications"], serde_json::json!({"catalogEntryId": request.catalog_entry_id, "expectedRevision": request.expected_revision, "proposalDigest": request.proposal_digest}), "publish install").await {
                Ok(value) => value,
                Err(TargetRequestError::Domain(status)) => return Ok(Err(map_publish_install_status(status))),
                Err(TargetRequestError::Runtime(error)) => return Err(error),
            };
            validate_install_publication(&response, &request)?;
            Ok(Ok(response.into_contract(request.agent_id)))
        })
    }

    fn propose_removal(
        &self,
        request: contract::ProposeRemovalRequest,
    ) -> lenso_kernel::NativeRequestFuture<contract::PluginManagementTargetProposeRemoval> {
        let target = self.lifecycle_target(
            &request.agent_id,
            contract::ProposeRemovalError::TargetNotFound,
            contract::ProposeRemovalError::Unsupported,
        );
        Box::pin(async move {
            let agent = match target {
                Ok(agent) => agent,
                Err(error) => return Ok(Err(error)),
            };
            let response: RemovalProposalResponse = match send_json(&agent, Method::POST, &["control", "plugin-removals", "proposals"], serde_json::json!({"expectedRevision": request.expected_revision, "pluginId": request.plugin_id}), "propose removal").await {
                Ok(value) => value,
                Err(TargetRequestError::Domain(status)) => return Ok(Err(map_propose_removal_status(status))),
                Err(TargetRequestError::Runtime(error)) => return Err(error),
            };
            validate_removal_proposal(&response, &request)?;
            Ok(Ok(response.into_contract(request.agent_id)))
        })
    }

    fn publish_removal(
        &self,
        request: contract::PublishRemovalRequest,
    ) -> lenso_kernel::NativeRequestFuture<contract::PluginManagementTargetPublishRemoval> {
        let target = self.lifecycle_target(
            &request.agent_id,
            contract::PublishRemovalError::TargetNotFound,
            contract::PublishRemovalError::Unsupported,
        );
        Box::pin(async move {
            let agent = match target {
                Ok(agent) => agent,
                Err(error) => return Ok(Err(error)),
            };
            let checked: RemovalProposalResponse = match send_json(&agent, Method::POST, &["control", "plugin-removals", "proposals"], serde_json::json!({"expectedRevision": request.expected_revision, "pluginId": request.plugin_id}), "recheck removal").await {
                Ok(value) => value,
                Err(TargetRequestError::Domain(status)) => return Ok(Err(map_publish_removal_status(status))),
                Err(TargetRequestError::Runtime(error)) => return Err(error),
            };
            if checked.proposal_digest != request.proposal_digest {
                return Ok(Err(contract::PublishRemovalError::ProposalMismatch));
            }
            let response: RemovalPublicationResponse = match send_json(&agent, Method::POST, &["control", "plugin-removals", "publications"], serde_json::json!({"expectedRevision": request.expected_revision, "pluginId": request.plugin_id, "proposalDigest": request.proposal_digest}), "publish removal").await {
                Ok(value) => value,
                Err(TargetRequestError::Domain(status)) => return Ok(Err(map_publish_removal_status(status))),
                Err(TargetRequestError::Runtime(error)) => return Err(error),
            };
            validate_removal_publication(&response, &request)?;
            Ok(Ok(response.into_contract(request.agent_id)))
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthorityResponse {
    kind: String,
    #[serde(default)]
    publication_history: Option<bool>,
    reference: String,
    #[serde(default)]
    rollback_proposals: Option<bool>,
}

impl From<AuthorityResponse> for contract::AuthoritySource {
    fn from(value: AuthorityResponse) -> Self {
        Self {
            kind: value.kind,
            reference: value.reference,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TrustedCatalogResponse {
    authority: AuthorityResponse,
    entries: Vec<TrustedCatalogEntry>,
    revision: String,
    schema: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TrustedCatalogEntry {
    catalog_entry_id: String,
    package_id: String,
    package_revision: String,
    source_digest: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallProposalResponse {
    authority: AuthorityResponse,
    base_revision: String,
    candidate_revision: String,
    catalog_entry_id: String,
    package_id: String,
    package_revision: String,
    proposal_digest: String,
    schema: String,
    source_digest: String,
}

impl InstallProposalResponse {
    fn into_contract(self, agent_id: String) -> contract::InstallProposalResponse {
        contract::InstallProposalResponse {
            agent_id,
            authority: self.authority.into(),
            base_revision: self.base_revision,
            catalog_entry_id: self.catalog_entry_id,
            candidate_revision: self.candidate_revision,
            package_id: self.package_id,
            package_revision: self.package_revision,
            proposal_digest: self.proposal_digest,
            source_digest: self.source_digest,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InstallPublicationResponse {
    authority: AuthorityResponse,
    base_revision: String,
    catalog_entry_id: String,
    package_id: String,
    package_revision: String,
    proposal_digest: String,
    revision: String,
    schema: String,
}

impl InstallPublicationResponse {
    fn into_contract(self, agent_id: String) -> contract::PublishInstallResponse {
        contract::PublishInstallResponse {
            agent_id,
            authority: self.authority.into(),
            base_revision: self.base_revision,
            catalog_entry_id: self.catalog_entry_id,
            package_id: self.package_id,
            package_revision: self.package_revision,
            proposal_digest: self.proposal_digest,
            revision: self.revision,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemovalProposalResponse {
    authority: AuthorityResponse,
    base_revision: String,
    candidate_revision: String,
    package_id: String,
    package_revision: String,
    proposal_digest: String,
    recoverable: bool,
    schema: String,
}

impl RemovalProposalResponse {
    fn into_contract(self, agent_id: String) -> contract::RemovalProposalResponse {
        contract::RemovalProposalResponse {
            agent_id,
            authority: self.authority.into(),
            base_revision: self.base_revision,
            candidate_revision: self.candidate_revision,
            package_id: self.package_id,
            package_revision: self.package_revision,
            proposal_digest: self.proposal_digest,
            recoverable: self.recoverable,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemovalPublicationResponse {
    authority: AuthorityResponse,
    base_revision: String,
    package_id: String,
    package_revision: String,
    proposal_digest: String,
    recoverable: bool,
    revision: String,
    schema: String,
}

impl RemovalPublicationResponse {
    fn into_contract(self, agent_id: String) -> contract::PublishRemovalResponse {
        contract::PublishRemovalResponse {
            agent_id,
            authority: self.authority.into(),
            base_revision: self.base_revision,
            package_id: self.package_id,
            package_revision: self.package_revision,
            proposal_digest: self.proposal_digest,
            recoverable: self.recoverable,
            revision: self.revision,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManagementResponse {
    binding_count: i64,
    configuration_authority: AuthorityResponse,
    plugins: Vec<ManagedPlugin>,
    revision: String,
    schema: String,
    selection_authority: Option<AuthorityResponse>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManagedPlugin {
    instances: Vec<ManagedPluginInstance>,
    package_id: String,
    package_revision: String,
    root_supplied: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ManagedPluginInstance {
    disableable: bool,
    has_root_difference: bool,
    instance_key: String,
    origin: String,
    root_configuration_toml: Option<String>,
    selection: String,
    source_digest: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InventoryResponse {
    schema: String,
    stream_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProposalResponse {
    application: String,
    base_revision: String,
    base_source_digest: String,
    candidate_revision: String,
    configuration_authority: AuthorityResponse,
    diagnostics: Vec<ProposalDiagnostic>,
    instance_key: String,
    plugin_id: String,
    proposal_digest: String,
    schema: String,
    status: String,
}

impl ProposalResponse {
    fn into_contract(self, agent_id: String) -> contract::ProposeResponse {
        contract::ProposeResponse {
            agent_id,
            application: self.application,
            authority: self.configuration_authority.into(),
            base_revision: self.base_revision,
            base_source_digest: self.base_source_digest,
            candidate_revision: self.candidate_revision,
            diagnostics: self
                .diagnostics
                .into_iter()
                .map(|diagnostic| contract::ProposalDiagnostic {
                    code: diagnostic.code,
                    detail: diagnostic.detail,
                })
                .collect(),
            instance: self.instance_key,
            plugin_id: self.plugin_id,
            proposal_digest: self.proposal_digest,
            schema: self.schema,
            status: self.status,
        }
    }
}

#[derive(Deserialize)]
struct ProposalDiagnostic {
    code: String,
    detail: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublicationResponse {
    base_revision: String,
    base_source_digest: String,
    configuration_authority: AuthorityResponse,
    proposal_digest: String,
    publication_schema: String,
    revision: String,
    schema: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HistoryResponse {
    configuration_authority: AuthorityResponse,
    instance_key: String,
    plugin_id: String,
    publications: Vec<PublicationRecordResponse>,
    schema: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublicationRecordResponse {
    base_revision: String,
    base_source_digest: Option<String>,
    configuration_toml: String,
    proposal_digest: String,
    published_at_unix_ms: i64,
    revision: String,
    rollback_of_proposal_digest: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RollbackProposalResponse {
    configuration_toml: String,
    proposal: ProposalResponse,
    rollback_of_proposal_digest: String,
    schema: String,
}

impl RollbackProposalResponse {
    fn into_contract(self, agent_id: String) -> contract::ProposeRollbackResponse {
        contract::ProposeRollbackResponse {
            agent_id,
            application: self.proposal.application,
            authority: self.proposal.configuration_authority.into(),
            base_revision: self.proposal.base_revision,
            base_source_digest: self.proposal.base_source_digest,
            candidate_revision: self.proposal.candidate_revision,
            diagnostics: self
                .proposal
                .diagnostics
                .into_iter()
                .map(|diagnostic| contract::ProposalDiagnostic {
                    code: diagnostic.code,
                    detail: diagnostic.detail,
                })
                .collect(),
            instance: self.proposal.instance_key,
            plugin_id: self.proposal.plugin_id,
            proposal_digest: self.proposal.proposal_digest,
            rollback_of_proposal_digest: self.rollback_of_proposal_digest,
            schema: self.proposal.schema,
            status: self.proposal.status,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MutationResponse {
    desired: DesiredSelection,
    schema: String,
    stream_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DesiredSelection {
    plugin_root_revision: String,
}

struct ConfigurationFence {
    rollback_proposals: bool,
    source_digest: String,
    stream_id: String,
}

#[derive(Clone, Copy)]
enum FenceError {
    Conflict,
    PluginNotFound,
}

async fn configuration_fence(
    agent: &AppAgentAdapter,
    plugin_id: &str,
    instance: &str,
    expected_revision: &str,
) -> Result<Result<ConfigurationFence, FenceError>, RuntimeFailure> {
    let management: ManagementResponse =
        get_json(agent, &["control", "plugins"], "inspect before change").await?;
    validate_management(&management)?;
    if management.revision != expected_revision {
        return Ok(Err(FenceError::Conflict));
    }
    let Some(source_digest) = management
        .plugins
        .iter()
        .find(|plugin| plugin.package_id == plugin_id)
        .and_then(|plugin| {
            plugin
                .instances
                .iter()
                .find(|candidate| candidate.instance_key == instance)
        })
        .map(|candidate| candidate.source_digest.clone())
    else {
        return Ok(Err(FenceError::PluginNotFound));
    };
    let inventory: InventoryResponse = get_json(agent, &["plugins"], "read stream").await?;
    validate_inventory(&inventory)?;
    Ok(Ok(ConfigurationFence {
        rollback_proposals: management.configuration_authority.rollback_proposals == Some(true),
        source_digest,
        stream_id: inventory.stream_id,
    }))
}

async fn request_rollback_proposal(
    agent: &AppAgentAdapter,
    request: &contract::ProposeRollbackRequest,
    fence: &ConfigurationFence,
) -> Result<RollbackProposalResponse, TargetRequestError> {
    send_json(
        agent,
        Method::POST,
        &[
            "control",
            "plugins",
            &request.plugin_id,
            &request.instance,
            "configuration",
            "rollback-proposals",
        ],
        serde_json::json!({
            "expectedRevision": request.expected_revision,
            "expectedSourceDigest": fence.source_digest,
            "expectedStreamId": fence.stream_id,
            "publicationProposalDigest": request.publication_proposal_digest,
        }),
        "propose rollback",
    )
    .await
}

fn has_managed_instance(management: &ManagementResponse, plugin_id: &str, instance: &str) -> bool {
    management.plugins.iter().any(|plugin| {
        plugin.package_id == plugin_id
            && plugin
                .instances
                .iter()
                .any(|candidate| candidate.instance_key == instance)
    })
}

fn validate_management(value: &ManagementResponse) -> Result<(), RuntimeFailure> {
    if value.schema != "lenso.agent.plugin-management.v1"
        || value.revision.is_empty()
        || value.configuration_authority.kind.is_empty()
        || value.configuration_authority.reference.is_empty()
    {
        return Err(protocol_failure(
            "Agent Host returned invalid Plugin management state",
        ));
    }
    Ok(())
}

fn validate_inventory(value: &InventoryResponse) -> Result<(), RuntimeFailure> {
    if value.schema != "lenso.agent.plugin-inventory.v2" || value.stream_id.is_empty() {
        return Err(protocol_failure(
            "Agent Host returned invalid Plugin inventory state",
        ));
    }
    Ok(())
}

fn validate_proposal(
    value: &ProposalResponse,
    request: &contract::ProposeRequest,
    fence: &ConfigurationFence,
) -> Result<(), RuntimeFailure> {
    if value.schema.is_empty()
        || value.plugin_id != request.plugin_id
        || value.instance_key != request.instance
        || value.base_revision != request.expected_revision
        || value.base_source_digest != fence.source_digest
        || value.proposal_digest.is_empty()
        || value.configuration_authority.kind.is_empty()
        || value.configuration_authority.reference.is_empty()
    {
        return Err(protocol_failure(
            "Agent Host returned an invalid Plugin proposal",
        ));
    }
    Ok(())
}

fn validate_publication(
    value: &PublicationResponse,
    request: &contract::PublishRequest,
    fence: &ConfigurationFence,
) -> Result<(), RuntimeFailure> {
    if value.schema != "lenso.agent.plugin-operation.v1"
        || value.publication_schema != "lenso.plugin-configuration-publication.v1"
        || value.base_revision != request.expected_revision
        || value.base_source_digest != fence.source_digest
        || value.proposal_digest != request.proposal_digest
        || value.revision.is_empty()
        || value.configuration_authority.kind.is_empty()
        || value.configuration_authority.reference.is_empty()
    {
        return Err(protocol_failure(
            "Agent Host returned an invalid Plugin publication",
        ));
    }
    Ok(())
}

fn validate_history(
    value: &HistoryResponse,
    request: &contract::HistoryRequest,
    management: &ManagementResponse,
) -> Result<(), RuntimeFailure> {
    if value.schema != "lenso.agent.plugin-configuration-history.v1"
        || value.plugin_id != request.plugin_id
        || value.instance_key != request.instance
        || value.configuration_authority.kind != management.configuration_authority.kind
        || value.configuration_authority.reference != management.configuration_authority.reference
        || value.publications.len() > 20
        || value.publications.iter().any(|publication| {
            publication.configuration_toml.len() > 262_144
                || publication.published_at_unix_ms < 0
                || publication.proposal_digest.len() != 71
                || publication.revision.len() != 71
                || publication.base_revision.len() != 71
                || publication
                    .base_source_digest
                    .as_ref()
                    .is_some_and(|digest| digest.len() != 71)
                || publication
                    .rollback_of_proposal_digest
                    .as_ref()
                    .is_some_and(|digest| digest.len() != 71)
        })
    {
        return Err(protocol_failure(
            "Agent Host returned invalid Plugin publication history",
        ));
    }
    Ok(())
}

fn history_contract(
    request: contract::HistoryRequest,
    management: ManagementResponse,
    history: HistoryResponse,
) -> Result<contract::HistoryResponse, RuntimeFailure> {
    validate_history(&history, &request, &management)?;
    let limit = usize::try_from(request.limit).unwrap_or(0).min(50);
    Ok(contract::HistoryResponse {
        agent_id: request.agent_id,
        authority: history.configuration_authority.into(),
        instance: history.instance_key,
        plugin_id: history.plugin_id,
        publications: history
            .publications
            .into_iter()
            .take(limit)
            .map(|publication| contract::PublicationRecord {
                base_revision: publication.base_revision,
                base_source_digest: publication.base_source_digest.into(),
                proposal_digest: publication.proposal_digest,
                published_at_unix_ms: publication.published_at_unix_ms,
                revision: publication.revision,
                rollback_of_proposal_digest: publication.rollback_of_proposal_digest.into(),
            })
            .collect(),
        revision: management.revision,
        schema: "lenso.agent.console-plugin-history.v1".to_owned(),
    })
}

fn validate_rollback_proposal(
    value: &RollbackProposalResponse,
    request: &contract::ProposeRollbackRequest,
    fence: &ConfigurationFence,
) -> Result<(), RuntimeFailure> {
    if value.schema != "lenso.agent.plugin-configuration-rollback-proposal.v1"
        || value.rollback_of_proposal_digest != request.publication_proposal_digest
        || value.configuration_toml.len() > 262_144
        || value.proposal.plugin_id != request.plugin_id
        || value.proposal.instance_key != request.instance
        || value.proposal.base_revision != request.expected_revision
        || value.proposal.base_source_digest != fence.source_digest
        || value.proposal.proposal_digest.len() != 71
        || value.proposal.configuration_authority.kind.is_empty()
        || value.proposal.configuration_authority.reference.is_empty()
    {
        return Err(protocol_failure(
            "Agent Host returned an invalid Plugin rollback proposal",
        ));
    }
    Ok(())
}

fn validate_rollback_publication(
    value: &PublicationResponse,
    request: &contract::PublishRollbackRequest,
    fence: &ConfigurationFence,
) -> Result<(), RuntimeFailure> {
    if value.schema != "lenso.agent.plugin-operation.v1"
        || value.publication_schema != "lenso.plugin-configuration-publication.v1"
        || value.base_revision != request.expected_revision
        || value.base_source_digest != fence.source_digest
        || value.proposal_digest != request.proposal_digest
        || value.revision.len() != 71
        || value.configuration_authority.kind.is_empty()
        || value.configuration_authority.reference.is_empty()
    {
        return Err(protocol_failure(
            "Agent Host returned an invalid Plugin rollback publication",
        ));
    }
    Ok(())
}

fn validate_install_proposal(
    value: &InstallProposalResponse,
    request: &contract::ProposeInstallRequest,
) -> Result<(), RuntimeFailure> {
    if value.schema != "lenso.agent.plugin-install-proposal.v1"
        || value.base_revision != request.expected_revision
        || value.catalog_entry_id != request.catalog_entry_id
        || value.candidate_revision.len() != 71
        || value.proposal_digest.len() != 71
        || value.source_digest.len() != 71
        || value.package_id.is_empty()
        || value.package_revision.is_empty()
    {
        return Err(protocol_failure(
            "Agent Host returned an invalid Plugin installation proposal",
        ));
    }
    Ok(())
}

fn validate_install_publication(
    value: &InstallPublicationResponse,
    request: &contract::PublishInstallRequest,
) -> Result<(), RuntimeFailure> {
    if value.schema != "lenso.agent.plugin-install-publication.v1"
        || value.base_revision != request.expected_revision
        || value.catalog_entry_id != request.catalog_entry_id
        || value.proposal_digest != request.proposal_digest
        || value.revision.len() != 71
        || value.package_id.is_empty()
        || value.package_revision.is_empty()
    {
        return Err(protocol_failure(
            "Agent Host returned an invalid Plugin installation publication",
        ));
    }
    Ok(())
}

fn validate_removal_proposal(
    value: &RemovalProposalResponse,
    request: &contract::ProposeRemovalRequest,
) -> Result<(), RuntimeFailure> {
    if value.schema != "lenso.agent.plugin-removal-proposal.v1"
        || value.base_revision != request.expected_revision
        || value.package_id != request.plugin_id
        || value.candidate_revision.len() != 71
        || value.proposal_digest.len() != 71
        || !value.recoverable
    {
        return Err(protocol_failure(
            "Agent Host returned an invalid recoverable Plugin removal proposal",
        ));
    }
    Ok(())
}

fn validate_removal_publication(
    value: &RemovalPublicationResponse,
    request: &contract::PublishRemovalRequest,
) -> Result<(), RuntimeFailure> {
    if value.schema != "lenso.agent.plugin-removal-publication.v1"
        || value.base_revision != request.expected_revision
        || value.package_id != request.plugin_id
        || value.proposal_digest != request.proposal_digest
        || value.revision.len() != 71
        || !value.recoverable
    {
        return Err(protocol_failure(
            "Agent Host returned an invalid recoverable Plugin removal publication",
        ));
    }
    Ok(())
}

async fn get_json<T: DeserializeOwned>(
    agent: &AppAgentAdapter,
    segments: &[&str],
    operation: &str,
) -> Result<T, RuntimeFailure> {
    let url = target_url(agent, segments)?;
    let response = tokio::time::timeout(REQUEST_TIMEOUT, agent.client.get(url).send())
        .await
        .map_err(|_| runtime_failure(format!("App Agent Plugin {operation} timed out")))?
        .map_err(|error| {
            runtime_failure(format!("App Agent Plugin {operation} failed: {error}"))
        })?;
    decode_success(response, operation).await
}

async fn get_json_domain<T: DeserializeOwned>(
    agent: &AppAgentAdapter,
    segments: &[&str],
    operation: &str,
) -> Result<T, TargetRequestError> {
    let url = target_url(agent, segments).map_err(TargetRequestError::Runtime)?;
    let response = tokio::time::timeout(REQUEST_TIMEOUT, agent.client.get(url).send())
        .await
        .map_err(|_| {
            TargetRequestError::Runtime(runtime_failure(format!(
                "App Agent Plugin {operation} timed out"
            )))
        })?
        .map_err(|error| {
            TargetRequestError::Runtime(runtime_failure(format!(
                "App Agent Plugin {operation} failed: {error}"
            )))
        })?;
    if !response.status().is_success() {
        let status = response.status();
        return if status.is_server_error() {
            Err(TargetRequestError::Runtime(runtime_failure(format!(
                "App Agent Plugin {operation} failed with {status}"
            ))))
        } else {
            Err(TargetRequestError::Domain(status))
        };
    }
    decode_success(response, operation)
        .await
        .map_err(TargetRequestError::Runtime)
}

async fn send_json<T: DeserializeOwned>(
    agent: &AppAgentAdapter,
    method: Method,
    segments: &[&str],
    body: serde_json::Value,
    operation: &str,
) -> Result<T, TargetRequestError> {
    let url = target_url(agent, segments).map_err(TargetRequestError::Runtime)?;
    let response = tokio::time::timeout(
        REQUEST_TIMEOUT,
        agent.client.request(method, url).json(&body).send(),
    )
    .await
    .map_err(|_| {
        TargetRequestError::Runtime(runtime_failure(format!(
            "App Agent Plugin {operation} timed out"
        )))
    })?
    .map_err(|error| {
        TargetRequestError::Runtime(runtime_failure(format!(
            "App Agent Plugin {operation} failed: {error}"
        )))
    })?;
    if !response.status().is_success() {
        let status = response.status();
        return if status.is_server_error() {
            Err(TargetRequestError::Runtime(runtime_failure(format!(
                "App Agent Plugin {operation} failed with {status}"
            ))))
        } else {
            Err(TargetRequestError::Domain(status))
        };
    }
    decode_success(response, operation)
        .await
        .map_err(TargetRequestError::Runtime)
}

async fn decode_success<T: DeserializeOwned>(
    mut response: Response,
    operation: &str,
) -> Result<T, RuntimeFailure> {
    if !response.status().is_success() {
        return Err(runtime_failure(format!(
            "App Agent Plugin {operation} failed with {}",
            response.status()
        )));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        runtime_failure(format!(
            "App Agent Plugin {operation} response failed: {error}"
        ))
    })? {
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(runtime_failure(format!(
                "App Agent Plugin {operation} response exceeded 2 MiB"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|error| {
        protocol_failure(format!(
            "App Agent Plugin {operation} response was invalid: {error}"
        ))
    })
}

fn target_url(agent: &AppAgentAdapter, segments: &[&str]) -> Result<Url, RuntimeFailure> {
    let mut url = agent.origin.clone();
    url.path_segments_mut()
        .map_err(|()| protocol_failure("App Agent origin cannot be a URL base"))?
        .pop_if_empty()
        .extend(["api", "console", "v1", "agent"])
        .extend(segments.iter().copied());
    Ok(url)
}

enum TargetRequestError {
    Domain(StatusCode),
    Runtime(RuntimeFailure),
}

fn map_lifecycle_status(status: StatusCode) -> contract::CatalogError {
    match status {
        StatusCode::NOT_FOUND => contract::CatalogError::PluginNotFound,
        StatusCode::FORBIDDEN => contract::CatalogError::Unsupported,
        StatusCode::CONFLICT => contract::CatalogError::Conflict,
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => {
            contract::CatalogError::InvalidRequest
        }
        _ => contract::CatalogError::Unknown(unknown_status(status)),
    }
}

macro_rules! lifecycle_status_mapper {
    ($name:ident, $error:ident) => {
        fn $name(status: StatusCode) -> contract::$error {
            match status {
                StatusCode::NOT_FOUND => contract::$error::PluginNotFound,
                StatusCode::FORBIDDEN => contract::$error::Unsupported,
                StatusCode::CONFLICT => contract::$error::Conflict,
                StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => {
                    contract::$error::InvalidRequest
                }
                _ => contract::$error::Unknown(unknown_status(status)),
            }
        }
    };
}

lifecycle_status_mapper!(map_propose_install_status, ProposeInstallError);
lifecycle_status_mapper!(map_publish_install_status, PublishInstallError);
lifecycle_status_mapper!(map_propose_removal_status, ProposeRemovalError);
lifecycle_status_mapper!(map_publish_removal_status, PublishRemovalError);

fn map_propose_fence(error: FenceError) -> contract::ProposeError {
    match error {
        FenceError::Conflict => contract::ProposeError::Conflict,
        FenceError::PluginNotFound => contract::ProposeError::PluginNotFound,
    }
}

fn map_history_status(status: StatusCode) -> contract::HistoryError {
    match status {
        StatusCode::NOT_FOUND => contract::HistoryError::PluginNotFound,
        StatusCode::CONFLICT | StatusCode::FORBIDDEN => contract::HistoryError::Unsupported,
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => {
            contract::HistoryError::InvalidRequest
        }
        _ => contract::HistoryError::Unknown(unknown_status(status)),
    }
}

fn map_propose_rollback_fence(error: FenceError) -> contract::ProposeRollbackError {
    match error {
        FenceError::Conflict => contract::ProposeRollbackError::Conflict,
        FenceError::PluginNotFound => contract::ProposeRollbackError::PluginNotFound,
    }
}

fn map_publish_rollback_fence(error: FenceError) -> contract::PublishRollbackError {
    match error {
        FenceError::Conflict => contract::PublishRollbackError::Conflict,
        FenceError::PluginNotFound => contract::PublishRollbackError::PluginNotFound,
    }
}

fn map_propose_rollback_status(status: StatusCode) -> contract::ProposeRollbackError {
    match status {
        StatusCode::NOT_FOUND => contract::ProposeRollbackError::PublicationNotFound,
        StatusCode::CONFLICT => contract::ProposeRollbackError::Conflict,
        StatusCode::FORBIDDEN => contract::ProposeRollbackError::Unsupported,
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => {
            contract::ProposeRollbackError::InvalidRequest
        }
        _ => contract::ProposeRollbackError::Unknown(unknown_status(status)),
    }
}

fn map_publish_rollback_status(status: StatusCode) -> contract::PublishRollbackError {
    match status {
        StatusCode::NOT_FOUND => contract::PublishRollbackError::PublicationNotFound,
        StatusCode::CONFLICT => contract::PublishRollbackError::Conflict,
        StatusCode::FORBIDDEN => contract::PublishRollbackError::Unsupported,
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => {
            contract::PublishRollbackError::InvalidRequest
        }
        _ => contract::PublishRollbackError::Unknown(unknown_status(status)),
    }
}

fn map_publish_fence(error: FenceError) -> contract::PublishError {
    match error {
        FenceError::Conflict => contract::PublishError::Conflict,
        FenceError::PluginNotFound => contract::PublishError::PluginNotFound,
    }
}

fn map_propose_status(status: StatusCode) -> contract::ProposeError {
    match status {
        StatusCode::NOT_FOUND => contract::ProposeError::PluginNotFound,
        StatusCode::CONFLICT => contract::ProposeError::Conflict,
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => {
            contract::ProposeError::InvalidRequest
        }
        _ => contract::ProposeError::Unknown(unknown_status(status)),
    }
}

fn map_publish_status(status: StatusCode) -> contract::PublishError {
    match status {
        StatusCode::NOT_FOUND => contract::PublishError::PluginNotFound,
        StatusCode::CONFLICT => contract::PublishError::Conflict,
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => {
            contract::PublishError::InvalidRequest
        }
        _ => contract::PublishError::Unknown(unknown_status(status)),
    }
}

fn map_selection_status(status: StatusCode) -> contract::SetEnabledError {
    match status {
        StatusCode::NOT_FOUND => contract::SetEnabledError::PluginNotFound,
        StatusCode::CONFLICT => contract::SetEnabledError::Conflict,
        StatusCode::FORBIDDEN => contract::SetEnabledError::Unsupported,
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => {
            contract::SetEnabledError::InvalidRequest
        }
        _ => contract::SetEnabledError::Unknown(unknown_status(status)),
    }
}

fn unknown_status(status: StatusCode) -> contract::UnknownDomainError {
    contract::UnknownDomainError {
        code: status.as_u16().to_string(),
        payload: None,
        extra: std::collections::BTreeMap::new(),
    }
}

fn bounded_count(value: usize, maximum: i64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX).min(maximum)
}

fn runtime_failure(detail: impl Into<String>) -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: detail.into(),
    }
}

fn protocol_failure(detail: impl Into<String>) -> RuntimeFailure {
    runtime_failure(format!(
        "Plugin management target protocol violation: {}",
        detail.into()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter(id: &str, plugin_configuration: bool, plugin_lifecycle: bool) -> AppAgentAdapter {
        let mut adapter = AppAgentAdapter::parse(&format!("{id}=http://127.0.0.1:3031")).unwrap();
        adapter.plugin_configuration = plugin_configuration;
        adapter.plugin_lifecycle = plugin_lifecycle;
        adapter
    }

    #[test]
    fn target_selection_is_exact_and_fails_closed() {
        let target = AppAgentPluginManagementTarget::new(vec![
            adapter("managed", true, false),
            adapter("lifecycle", false, true),
            adapter("observed", false, false),
        ]);

        assert!(matches!(
            target.configuration_target("missing", "missing", "unsupported"),
            Err("missing")
        ));
        assert!(matches!(
            target.configuration_target("observed", "missing", "unsupported"),
            Err("unsupported")
        ));
        assert!(matches!(
            target.lifecycle_target("managed", "missing", "unsupported"),
            Err("unsupported")
        ));
        assert_eq!(
            target
                .configuration_target("managed", "missing", "unsupported")
                .unwrap()
                .id,
            "managed"
        );
        assert_eq!(
            target
                .lifecycle_target("lifecycle", "missing", "unsupported")
                .unwrap()
                .id,
            "lifecycle"
        );
    }

    #[test]
    fn history_projection_never_exposes_retained_configuration() {
        let digest = |character: char| format!("sha256:{}", character.to_string().repeat(64));
        let management = ManagementResponse {
            binding_count: 1,
            configuration_authority: AuthorityResponse {
                kind: "remote_configuration_service".to_owned(),
                publication_history: Some(true),
                reference: "managed".to_owned(),
                rollback_proposals: Some(true),
            },
            plugins: vec![ManagedPlugin {
                instances: vec![ManagedPluginInstance {
                    disableable: true,
                    has_root_difference: true,
                    instance_key: "default".to_owned(),
                    origin: "plugin-root".to_owned(),
                    root_configuration_toml: Some("current = true\n".to_owned()),
                    selection: "enabled".to_owned(),
                    source_digest: digest('c'),
                }],
                package_id: "example.echo".to_owned(),
                package_revision: "1.0.0".to_owned(),
                root_supplied: true,
            }],
            revision: digest('b'),
            schema: "lenso.agent.plugin-management.v1".to_owned(),
            selection_authority: None,
        };
        let history = HistoryResponse {
            configuration_authority: AuthorityResponse {
                kind: "remote_configuration_service".to_owned(),
                publication_history: Some(true),
                reference: "managed".to_owned(),
                rollback_proposals: Some(true),
            },
            instance_key: "default".to_owned(),
            plugin_id: "example.echo".to_owned(),
            publications: vec![PublicationRecordResponse {
                base_revision: digest('a'),
                base_source_digest: Some(digest('c')),
                configuration_toml: "secret = \"do-not-leak\"\n".to_owned(),
                proposal_digest: digest('d'),
                published_at_unix_ms: 1_788_310_800_000,
                revision: digest('b'),
                rollback_of_proposal_digest: None,
            }],
            schema: "lenso.agent.plugin-configuration-history.v1".to_owned(),
        };

        let projected = history_contract(
            contract::HistoryRequest {
                agent_id: "managed".to_owned(),
                instance: "default".to_owned(),
                limit: 10,
                plugin_id: "example.echo".to_owned(),
            },
            management,
            history,
        )
        .unwrap();
        let json = serde_json::to_string(&projected).unwrap();

        assert!(!json.contains("do-not-leak"));
        assert!(!json.contains("configurationToml"));
        assert!(!json.contains("configuration_toml"));
        assert!(json.contains(&digest('d')));
    }
}
