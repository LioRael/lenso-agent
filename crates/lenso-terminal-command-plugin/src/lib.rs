//! Aggregate terminal command runtime Plugin.

use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use lenso::prelude::*;
use lenso_capability_terminal_command as command_contract;
use lenso_capability_terminal_command::{
    CatalogRequest, CatalogResponse, CommandCatalog, CommandDefinition, CommandExecute,
    CommandExecuteInvocationError, CommandProvider, ExecuteError, ExecuteMessage, ExecuteOpen,
};
use lenso_capability_terminal_command_provider as provider_contract;
use lenso_kernel::{InvocationContext, NativeStreamSession, RuntimeFailure, StreamEvent};

const MAX_AGGREGATE_COMMANDS: usize = 256;

#[lenso::plugin(lifecycle)]
#[derive(Clone, Debug)]
struct TerminalCommandRuntime {
    providers: ManyPort<provider_contract::CommandProviderClient>,
    state: Rc<RefCell<Option<RuntimeState>>>,
    #[tasks]
    tasks: ManagedTasks,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    Text,
    Json,
}

#[derive(Clone, Debug)]
struct Route {
    provider_index: usize,
    formats: Vec<Format>,
}

#[derive(Debug)]
struct RuntimeState {
    catalog: Vec<CommandDefinition>,
    routes: BTreeMap<String, Route>,
}

#[lenso::provides(command_contract::Command)]
impl CommandProvider for TerminalCommandRuntime {
    fn catalog(
        &self,
        _context: InvocationContext,
        _request: CatalogRequest,
    ) -> lenso_kernel::NativeRequestFuture<CommandCatalog> {
        let result = self
            .state
            .borrow()
            .as_ref()
            .map(|state| CatalogResponse {
                commands: state.catalog.clone(),
            })
            .ok_or(RuntimeFailure::Unavailable {
                capability: command_contract::CAPABILITY_ID,
            });
        Box::pin(futures::future::ready(result.map(Ok)))
    }

    fn execute(
        &self,
        context: InvocationContext,
        request: ExecuteOpen,
    ) -> futures::future::LocalBoxFuture<
        'static,
        Result<Box<dyn NativeStreamSession>, CommandExecuteInvocationError>,
    > {
        let route = self
            .state
            .borrow()
            .as_ref()
            .and_then(|state| state.routes.get(&request.id).cloned());
        let Some(route) = route else {
            return Box::pin(futures::future::ready(Err(
                CommandExecuteInvocationError::Domain(ExecuteError::NotFound),
            )));
        };
        let format = aggregate_format(&request.output_format);
        if request.arguments_json.as_str().len() > 262_144
            || !route.formats.contains(&format)
            || serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(
                request.arguments_json.as_str(),
            )
            .is_err()
        {
            return Box::pin(futures::future::ready(Err(
                CommandExecuteInvocationError::Domain(ExecuteError::InvalidArguments),
            )));
        }

        let providers = self.providers.clone();
        let tasks = self.tasks.clone();
        Box::pin(async move {
            let provider_request = provider_contract::ExecuteOpen {
                id: request.id,
                arguments_json: request
                    .arguments_json
                    .as_str()
                    .to_owned()
                    .try_into()
                    .expect("validated arguments remain JSON"),
                output_format: provider_format(format),
            };
            let (stream, channel) = ProviderStream::<CommandExecute>::channel(&context, 16);
            tasks
                .spawn_local(async move {
                    produce_stream(
                        &providers[route.provider_index],
                        context,
                        provider_request,
                        channel,
                    )
                    .await;
                })
                .map_err(|error| {
                    CommandExecuteInvocationError::Runtime(RuntimeFailure::PluginFailure {
                        detail: format!("terminal command stream task failed to start: {error:?}"),
                    })
                })?;
            Ok(Box::new(stream) as Box<dyn NativeStreamSession>)
        })
    }
}

impl Lifecycle for TerminalCommandRuntime {
    async fn activate(&self, _context: ActivateContext) -> Result<(), RuntimeFailure> {
        let mut provider_catalogs = Vec::new();

        for (provider_index, provider) in self.providers.iter().enumerate() {
            let response = provider
                .catalog(provider_contract::CatalogRequest {})
                .await
                .map_err(|error| match error {
                    provider_contract::CommandProviderCatalogInvocationError::Domain(_) => {
                        RuntimeFailure::PluginFailure {
                            detail: format!(
                                "terminal command provider {provider_index} returned an invalid catalog"
                            ),
                        }
                    }
                    provider_contract::CommandProviderCatalogInvocationError::Runtime(error) => {
                        error
                    }
                })?;
            provider_contract::validate_catalog(&response.commands).map_err(|detail| {
                RuntimeFailure::InvalidResolvedPlan {
                    detail: format!("terminal command provider {provider_index}: {detail}"),
                }
            })?;
            provider_catalogs.push((provider_index, response.commands));
        }

        self.state
            .replace(Some(build_runtime_state(provider_catalogs)?));
        Ok(())
    }

    fn deactivate(
        &self,
        _context: DeactivateContext,
    ) -> impl std::future::Future<Output = Result<(), RuntimeFailure>> {
        self.state.replace(None);
        std::future::ready(Ok(()))
    }
}

fn build_runtime_state(
    provider_catalogs: impl IntoIterator<Item = (usize, Vec<provider_contract::CommandDefinition>)>,
) -> Result<RuntimeState, RuntimeFailure> {
    let mut catalog: Vec<CommandDefinition> = Vec::new();
    let mut routes = BTreeMap::new();
    let mut paths = BTreeMap::<String, String>::new();
    for (provider_index, commands) in provider_catalogs {
        for provider_command in commands {
            if catalog.len() == MAX_AGGREGATE_COMMANDS {
                return Err(RuntimeFailure::InvalidResolvedPlan {
                    detail: format!(
                        "aggregate terminal catalog exceeds {MAX_AGGREGATE_COMMANDS} commands"
                    ),
                });
            }
            let id = provider_command.id.clone();
            let path = provider_command.path.join(" ");
            if routes.contains_key(&id) {
                return Err(RuntimeFailure::InvalidResolvedPlan {
                    detail: format!("duplicate terminal command id `{id}`"),
                });
            }
            if let Some((existing_path, existing_id)) = paths
                .iter()
                .find(|(existing_path, _)| path_prefix_conflict(existing_path, &path))
            {
                return Err(RuntimeFailure::InvalidResolvedPlan {
                    detail: format!(
                        "terminal command path `{path}` conflicts with group `{existing_path}` owned by `{existing_id}`"
                    ),
                });
            }
            if let Some(existing) = paths.insert(path.clone(), id.clone()) {
                return Err(RuntimeFailure::InvalidResolvedPlan {
                    detail: format!(
                        "duplicate terminal command path `{path}` for `{existing}` and `{id}`"
                    ),
                });
            }
            let formats = provider_command
                .output_formats
                .iter()
                .map(provider_format_key)
                .collect();
            routes.insert(
                id,
                Route {
                    provider_index,
                    formats,
                },
            );
            catalog.push(convert_wire(provider_command)?);
        }
    }
    catalog.sort_by(|left, right| left.path.cmp(&right.path).then(left.id.cmp(&right.id)));
    Ok(RuntimeState { catalog, routes })
}

async fn produce_stream(
    provider: &provider_contract::CommandProviderClient,
    context: InvocationContext,
    request: provider_contract::ExecuteOpen,
    mut channel: ProviderStreamChannel<CommandExecute>,
) {
    let result = proxy_stream(provider, context, request, &mut channel).await;
    let _ = channel.complete(result).await;
}

async fn proxy_stream(
    provider: &provider_contract::CommandProviderClient,
    context: InvocationContext,
    request: provider_contract::ExecuteOpen,
    channel: &mut ProviderStreamChannel<CommandExecute>,
) -> PluginResult<(), ExecuteError> {
    let stream = provider
        .execute_with_context(context, request)
        .await
        .map_err(map_open_error)?;
    stream.close_send().await.map_err(PluginError::runtime)?;

    loop {
        match stream.receive().await.map_err(PluginError::runtime)? {
            StreamEvent::Message(message) => {
                channel
                    .send(validate_and_convert_message(message).map_err(PluginError::runtime)?)
                    .await
                    .map_err(PluginError::runtime)?;
            }
            StreamEvent::PeerHalfClosed => {}
            StreamEvent::Terminal(Ok(())) => return Ok(()),
            StreamEvent::Terminal(Err(error)) => {
                let error = validate_and_convert_error(error).map_err(PluginError::runtime)?;
                return Err(PluginError::domain(error));
            }
        }
    }
}

fn map_open_error(
    error: provider_contract::CommandProviderExecuteInvocationError,
) -> PluginError<ExecuteError> {
    match error {
        provider_contract::CommandProviderExecuteInvocationError::Domain(error) => {
            match validate_and_convert_error(error) {
                Ok(error) => PluginError::domain(error),
                Err(error) => PluginError::runtime(error),
            }
        }
        provider_contract::CommandProviderExecuteInvocationError::Runtime(error) => {
            PluginError::runtime(error)
        }
    }
}

fn validate_and_convert_message(
    message: provider_contract::ExecuteMessage,
) -> Result<ExecuteMessage, RuntimeFailure> {
    if message.content.chars().count() > 1_048_576
        || (matches!(message.content_type, provider_contract::ContentType::Json)
            && serde_json::from_str::<serde_json::Value>(&message.content).is_err())
    {
        return Err(RuntimeFailure::ProtocolViolation {
            capability: provider_contract::CAPABILITY_ID,
        });
    }
    Ok(ExecuteMessage {
        kind: match message.kind {
            provider_contract::OutputKind::Stdout => command_contract::OutputKind::Stdout,
            provider_contract::OutputKind::Stderr => command_contract::OutputKind::Stderr,
            provider_contract::OutputKind::Progress => command_contract::OutputKind::Progress,
            provider_contract::OutputKind::Result => command_contract::OutputKind::Result,
        },
        content_type: match message.content_type {
            provider_contract::ContentType::Text => command_contract::ContentType::Text,
            provider_contract::ContentType::Json => command_contract::ContentType::Json,
        },
        content: message.content,
    })
}

fn validate_and_convert_error(
    error: provider_contract::ExecuteError,
) -> Result<ExecuteError, RuntimeFailure> {
    let converted = match error {
        provider_contract::ExecuteError::InvalidArguments => ExecuteError::InvalidArguments,
        provider_contract::ExecuteError::PermissionDenied => ExecuteError::PermissionDenied,
        provider_contract::ExecuteError::NotFound => ExecuteError::NotFound,
        provider_contract::ExecuteError::OutputLimitExceeded => ExecuteError::OutputLimitExceeded,
        provider_contract::ExecuteError::ExecutionFailed { payload } => {
            if payload.reason_code.is_empty()
                || payload.reason_code.chars().count() > 128
                || payload.message.chars().count() > 4_096
                || payload.details_json.as_str().chars().count() > 65_536
                || serde_json::from_str::<serde_json::Value>(payload.details_json.as_str()).is_err()
            {
                return Err(RuntimeFailure::ProtocolViolation {
                    capability: provider_contract::CAPABILITY_ID,
                });
            }
            ExecuteError::ExecutionFailed {
                payload: command_contract::ExecutionFailedPayload {
                    reason_code: payload.reason_code,
                    message: payload.message,
                    details_json: payload.details_json,
                },
            }
        }
        provider_contract::ExecuteError::Unknown(unknown) => {
            if unknown.code.is_empty() || unknown.code.chars().count() > 128 {
                return Err(RuntimeFailure::ProtocolViolation {
                    capability: provider_contract::CAPABILITY_ID,
                });
            }
            let details_json = unknown
                .payload
                .unwrap_or_else(|| serde_json::json!({}))
                .to_string();
            if details_json.chars().count() > 65_536 {
                return Err(RuntimeFailure::ProtocolViolation {
                    capability: provider_contract::CAPABILITY_ID,
                });
            }
            ExecuteError::ExecutionFailed {
                payload: command_contract::ExecutionFailedPayload {
                    reason_code: unknown.code,
                    message: "terminal command provider returned an unknown domain error"
                        .to_owned(),
                    details_json: details_json
                        .try_into()
                        .expect("serialized unknown error payload is JSON"),
                },
            }
        }
    };
    Ok(converted)
}

fn aggregate_format(format: &command_contract::OutputFormat) -> Format {
    match format {
        command_contract::OutputFormat::Text => Format::Text,
        command_contract::OutputFormat::Json => Format::Json,
    }
}

fn provider_format(format: Format) -> provider_contract::OutputFormat {
    match format {
        Format::Text => provider_contract::OutputFormat::Text,
        Format::Json => provider_contract::OutputFormat::Json,
    }
}

fn provider_format_key(format: &provider_contract::OutputFormat) -> Format {
    match format {
        provider_contract::OutputFormat::Text => Format::Text,
        provider_contract::OutputFormat::Json => Format::Json,
    }
}

fn path_prefix_conflict(left: &str, right: &str) -> bool {
    left.strip_prefix(right)
        .is_some_and(|suffix| suffix.starts_with(' '))
        || right
            .strip_prefix(left)
            .is_some_and(|suffix| suffix.starts_with(' '))
}

fn convert_wire<T, U>(value: T) -> Result<U, RuntimeFailure>
where
    T: serde::Serialize,
    U: serde::de::DeserializeOwned,
{
    serde_json::to_value(value)
        .and_then(serde_json::from_value)
        .map_err(|_| RuntimeFailure::ProtocolViolation {
            capability: provider_contract::CAPABILITY_ID,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(id: &str, path: &[&str]) -> provider_contract::CommandDefinition {
        provider_contract::CommandDefinition {
            id: id.to_owned(),
            path: path.iter().map(|segment| (*segment).to_owned()).collect(),
            summary: id.to_owned(),
            description: String::new(),
            parameters: Vec::new(),
            output_formats: vec![provider_contract::OutputFormat::Text],
        }
    }

    #[test]
    fn provider_and_aggregate_command_schemas_stay_aligned() {
        for (provider, aggregate) in [
            (
                include_str!(
                    "../../lenso-capability-terminal-command-provider/schemas/catalog-request.schema.json"
                ),
                include_str!(
                    "../../lenso-capability-terminal-command/schemas/catalog-request.schema.json"
                ),
            ),
            (
                include_str!(
                    "../../lenso-capability-terminal-command-provider/schemas/catalog-response.schema.json"
                ),
                include_str!(
                    "../../lenso-capability-terminal-command/schemas/catalog-response.schema.json"
                ),
            ),
            (
                include_str!(
                    "../../lenso-capability-terminal-command-provider/schemas/catalog-error.schema.json"
                ),
                include_str!(
                    "../../lenso-capability-terminal-command/schemas/catalog-error.schema.json"
                ),
            ),
            (
                include_str!(
                    "../../lenso-capability-terminal-command-provider/schemas/execute-open.schema.json"
                ),
                include_str!(
                    "../../lenso-capability-terminal-command/schemas/execute-open.schema.json"
                ),
            ),
            (
                include_str!(
                    "../../lenso-capability-terminal-command-provider/schemas/execute-message.schema.json"
                ),
                include_str!(
                    "../../lenso-capability-terminal-command/schemas/execute-message.schema.json"
                ),
            ),
            (
                include_str!(
                    "../../lenso-capability-terminal-command-provider/schemas/execute-error.schema.json"
                ),
                include_str!(
                    "../../lenso-capability-terminal-command/schemas/execute-error.schema.json"
                ),
            ),
        ] {
            let provider: serde_json::Value = serde_json::from_str(provider).unwrap();
            let aggregate: serde_json::Value = serde_json::from_str(aggregate).unwrap();
            assert_eq!(provider, aggregate);
        }
    }

    #[test]
    fn aggregate_rejects_cross_provider_identity_and_path_conflicts() {
        for catalogs in [
            vec![
                (0, vec![command("project.show", &["project", "show"])]),
                (1, vec![command("project.show", &["project", "status"])]),
            ],
            vec![
                (0, vec![command("project.show", &["project", "show"])]),
                (1, vec![command("project.inspect", &["project", "show"])]),
            ],
            vec![
                (0, vec![command("project.root", &["project"])]),
                (1, vec![command("project.show", &["project", "show"])]),
            ],
        ] {
            assert!(matches!(
                build_runtime_state(catalogs),
                Err(RuntimeFailure::InvalidResolvedPlan { .. })
            ));
        }
    }

    #[test]
    fn removing_a_provider_removes_only_its_routes() {
        let base = (0, vec![command("project.show", &["project", "show"])]);
        let extra = (1, vec![command("session.list", &["sessions", "list"])]);
        let composed = build_runtime_state([base.clone(), extra]).unwrap();
        assert_eq!(composed.routes.len(), 2);

        let reduced = build_runtime_state([base]).unwrap();
        assert_eq!(reduced.routes.len(), 1);
        assert!(reduced.routes.contains_key("project.show"));
        assert!(!reduced.routes.contains_key("session.list"));
    }

    #[test]
    fn aggregate_rejects_invalid_json_output_from_a_native_provider() {
        let error = validate_and_convert_message(provider_contract::ExecuteMessage {
            kind: provider_contract::OutputKind::Result,
            content_type: provider_contract::ContentType::Json,
            content: "not-json".to_owned(),
        })
        .unwrap_err();
        assert_eq!(
            error,
            RuntimeFailure::ProtocolViolation {
                capability: provider_contract::CAPABILITY_ID
            }
        );
    }

    #[test]
    fn aggregate_rejects_unbounded_native_provider_errors() {
        let error = validate_and_convert_error(provider_contract::ExecuteError::ExecutionFailed {
            payload: provider_contract::ExecutionFailedPayload {
                reason_code: "x".repeat(129),
                message: String::new(),
                details_json: "{}".to_owned().try_into().expect("empty object is JSON"),
            },
        })
        .unwrap_err();
        assert!(matches!(error, RuntimeFailure::ProtocolViolation { .. }));
    }
}
