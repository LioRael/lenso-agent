//! Aggregate Tool Runtime Plugin.

use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use lenso::prelude::*;
use lenso_capability_agent_tool_hook as hook_contract;
use lenso_capability_agent_tool_progress as progress_contract;
use lenso_capability_agent_tool_provider as provider_contract;
use lenso_capability_agent_tools::{
    self as tools_contract, CatalogRequest, CatalogResponse, CatalogResponseToolsItem,
    CatalogResponseToolsItemExecution, ExecuteError, ExecuteErrorToolErrorPayload, ExecuteRequest,
    ExecuteResponse, ExecuteResponseContentType, ExecuteStreamError,
    ExecuteStreamErrorToolErrorPayload, ExecuteStreamRequest, ExecuteStreamResponse,
    ExecuteStreamResponseContentType, ExecuteStreamResponseKind, ToolsCatalog, ToolsExecute,
    ToolsExecuteStreamInvocationError, ToolsProvider,
};
use lenso_kernel::{InvocationContext, NativeStreamSession, RuntimeFailure, StreamEvent};

#[lenso::plugin(lifecycle)]
#[derive(Clone, Debug)]
struct ToolsPlugin {
    hooks: ManyPort<hook_contract::ToolHookClient>,
    providers: ManyPort<provider_contract::ToolProviderClient>,
    progress_providers: ManyPort<progress_contract::ToolProgressClient>,
    state: Rc<RefCell<Option<ToolRuntimeState>>>,
    #[tasks]
    tasks: ManagedTasks,
}

#[derive(Debug)]
struct ToolRuntimeState {
    catalog: Vec<CatalogResponseToolsItem>,
    routes: BTreeMap<String, usize>,
    progress_routes: BTreeMap<String, usize>,
}

#[lenso::provides(tools_contract::Tools)]
impl ToolsProvider for ToolsPlugin {
    fn catalog(
        &self,
        _context: InvocationContext,
        _request: CatalogRequest,
    ) -> lenso_kernel::NativeRequestFuture<ToolsCatalog> {
        let result = self
            .state
            .borrow()
            .as_ref()
            .map(|state| CatalogResponse {
                tools: state.catalog.clone(),
            })
            .ok_or(RuntimeFailure::Unavailable {
                capability: lenso_capability_agent_tools::CAPABILITY_ID,
            });
        Box::pin(futures::future::ready(result.map(Ok)))
    }

    fn execute(
        &self,
        context: InvocationContext,
        request: ExecuteRequest,
    ) -> lenso_kernel::NativeRequestFuture<ToolsExecute> {
        let route = self
            .state
            .borrow()
            .as_ref()
            .and_then(|state| state.routes.get(&request.name).copied());
        let Some(index) = route else {
            return Box::pin(futures::future::ready(Ok(Err(ExecuteError::UnknownTool))));
        };
        let Ok(arguments_json) =
            hook_contract::normalize_arguments(request.arguments_json.as_str())
        else {
            return Box::pin(futures::future::ready(Ok(Err(
                ExecuteError::InvalidArguments,
            ))));
        };
        let providers = self.providers.clone();
        let hooks = self.hooks.clone();
        Box::pin(async move {
            let execution = hook_contract::start_hooks(
                &hooks,
                &context,
                request.name.clone(),
                arguments_json.clone(),
            )
            .await?;
            if let Some(block) = &execution.block {
                hook_contract::finish_hooks(
                    &hooks,
                    &context,
                    &execution,
                    hook_contract::HookTerminal::DomainError,
                    "",
                    "{}",
                    block.provider_code,
                )
                .await?;
                return Ok(Err(tool_error(
                    block.provider_code,
                    &block.message,
                    &block.details_json,
                )));
            }
            let result = providers[index]
                .execute_with_context(
                    context.clone(),
                    provider_contract::ExecuteRequest {
                        name: request.name,
                        arguments_json: arguments_json
                            .try_into()
                            .expect("normalized arguments must remain JSON"),
                    },
                )
                .await;
            match result {
                Ok(response) => {
                    hook_contract::finish_hooks(
                        &hooks,
                        &context,
                        &execution,
                        hook_contract::HookTerminal::Success,
                        &response.content,
                        response.metadata_json.as_str(),
                        "",
                    )
                    .await?;
                    Ok(Ok(convert_execute_response(response)))
                }
                Err(provider_contract::ToolProviderExecuteInvocationError::Domain(error)) => {
                    let error = convert_execute_error(error);
                    hook_contract::finish_hooks(
                        &hooks,
                        &context,
                        &execution,
                        hook_contract::HookTerminal::DomainError,
                        "",
                        "{}",
                        execute_error_code(&error),
                    )
                    .await?;
                    Ok(Err(error))
                }
                Err(provider_contract::ToolProviderExecuteInvocationError::Runtime(error)) => {
                    hook_contract::finish_hooks(
                        &hooks,
                        &context,
                        &execution,
                        hook_contract::HookTerminal::RuntimeFailure,
                        "",
                        "{}",
                        "runtime_failure",
                    )
                    .await?;
                    Err(error)
                }
            }
        })
    }

    fn execute_stream(
        &self,
        context: InvocationContext,
        request: ExecuteStreamRequest,
    ) -> futures::future::LocalBoxFuture<
        'static,
        Result<Box<dyn NativeStreamSession>, ToolsExecuteStreamInvocationError>,
    > {
        let route = self
            .state
            .borrow()
            .as_ref()
            .and_then(|state| state.routes.get(&request.name).copied());
        let Some(provider_index) = route else {
            return Box::pin(futures::future::ready(Err(
                ToolsExecuteStreamInvocationError::Domain(ExecuteStreamError::UnknownTool),
            )));
        };
        let Ok(arguments_json) =
            hook_contract::normalize_arguments(request.arguments_json.as_str())
        else {
            return Box::pin(futures::future::ready(Err(
                ToolsExecuteStreamInvocationError::Domain(ExecuteStreamError::InvalidArguments),
            )));
        };
        let progress_index = self
            .state
            .borrow()
            .as_ref()
            .and_then(|state| state.progress_routes.get(&request.name).copied());
        let providers = self.providers.clone();
        let progress_providers = self.progress_providers.clone();
        let hooks = self.hooks.clone();
        let tasks = self.tasks.clone();
        Box::pin(async move {
            let execution = hook_contract::start_hooks(
                &hooks,
                &context,
                request.name.clone(),
                arguments_json.clone(),
            )
            .await
            .map_err(ToolsExecuteStreamInvocationError::Runtime)?;
            if let Some(block) = &execution.block {
                hook_contract::finish_hooks(
                    &hooks,
                    &context,
                    &execution,
                    hook_contract::HookTerminal::DomainError,
                    "",
                    "{}",
                    block.provider_code,
                )
                .await
                .map_err(ToolsExecuteStreamInvocationError::Runtime)?;
                return Err(ToolsExecuteStreamInvocationError::Domain(
                    stream_tool_error(block.provider_code, &block.message, &block.details_json),
                ));
            }
            let request = ExecuteStreamRequest {
                name: request.name,
                arguments_json: arguments_json
                    .try_into()
                    .expect("normalized arguments must remain JSON"),
            };
            let (stream, channel) =
                ProviderStream::<tools_contract::ToolsExecuteStream>::channel(&context, 8);
            tasks
                .spawn_local(async move {
                    produce_execute_stream(
                        providers,
                        progress_providers,
                        hooks,
                        execution,
                        provider_index,
                        progress_index,
                        context,
                        request,
                        channel,
                    )
                    .await;
                })
                .map_err(|error| {
                    ToolsExecuteStreamInvocationError::Runtime(RuntimeFailure::PluginFailure {
                        detail: format!("Tool execution stream task failed to start: {error:?}"),
                    })
                })?;
            Ok(Box::new(stream) as Box<dyn NativeStreamSession>)
        })
    }
}

fn execute_error_code(error: &ExecuteError) -> &str {
    match error {
        ExecuteError::InvalidArguments => "invalid_arguments",
        ExecuteError::UnknownTool => "unknown_tool",
        ExecuteError::ToolError { payload } => &payload.provider_code,
        ExecuteError::Unknown(unknown) => &unknown.code,
    }
}

impl Lifecycle for ToolsPlugin {
    async fn activate(&self, _context: ActivateContext) -> Result<(), RuntimeFailure> {
        let mut catalog = Vec::new();
        let mut routes = BTreeMap::new();
        let mut progress_routes = BTreeMap::new();
        for (index, provider) in self.providers.iter().enumerate() {
            let response = provider
                .catalog(provider_contract::CatalogRequest {})
                .await
                .map_err(|error| match error {
                    provider_contract::ToolProviderCatalogInvocationError::Domain(_) => {
                        RuntimeFailure::PluginFailure {
                            detail: format!("Tool Provider {index} returned an invalid catalog"),
                        }
                    }
                    provider_contract::ToolProviderCatalogInvocationError::Runtime(error) => error,
                })?;
            for tool in response.tools {
                if !valid_model_tool_name(&tool.name) {
                    return Err(RuntimeFailure::InvalidResolvedPlan {
                        detail: format!(
                            "invalid Tool name `{}`; expected lowercase snake_case with at most 64 ASCII characters",
                            tool.name
                        ),
                    });
                }
                if routes.insert(tool.name.clone(), index).is_some() {
                    return Err(RuntimeFailure::InvalidResolvedPlan {
                        detail: format!("duplicate Tool name `{}`", tool.name),
                    });
                }
                catalog.push(CatalogResponseToolsItem {
                    name: tool.name,
                    description: tool.description,
                    input_schema_json: tool.input_schema_json,
                    execution: match tool.execution {
                        provider_contract::ToolExecutionClass::ParallelSafe => {
                            CatalogResponseToolsItemExecution::ParallelSafe
                        }
                        provider_contract::ToolExecutionClass::Exclusive => {
                            CatalogResponseToolsItemExecution::Exclusive
                        }
                    },
                });
            }
        }
        for (index, provider) in self.progress_providers.iter().enumerate() {
            let response = provider
                .progress_catalog(progress_contract::CatalogRequest {})
                .await
                .map_err(|error| RuntimeFailure::PluginFailure {
                    detail: format!("Tool Progress Provider {index} catalog failed: {error:?}"),
                })?;
            for tool in response.tools {
                if !routes.contains_key(&tool.name) {
                    return Err(RuntimeFailure::InvalidResolvedPlan {
                        detail: format!(
                            "Tool Progress Provider advertises unknown Tool `{}`",
                            tool.name
                        ),
                    });
                }
                if progress_routes.insert(tool.name.clone(), index).is_some() {
                    return Err(RuntimeFailure::InvalidResolvedPlan {
                        detail: format!("duplicate Tool progress route `{}`", tool.name),
                    });
                }
            }
        }
        catalog.sort_by(|left, right| left.name.cmp(&right.name));
        self.state.replace(Some(ToolRuntimeState {
            catalog,
            routes,
            progress_routes,
        }));
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
async fn produce_execute_stream(
    providers: ManyPort<provider_contract::ToolProviderClient>,
    progress_providers: ManyPort<progress_contract::ToolProgressClient>,
    hooks: ManyPort<hook_contract::ToolHookClient>,
    hook_execution: hook_contract::HookExecution,
    provider_index: usize,
    progress_index: Option<usize>,
    context: InvocationContext,
    request: ExecuteStreamRequest,
    mut channel: ProviderStreamChannel<tools_contract::ToolsExecuteStream>,
) {
    let result = if let Some(progress_index) = progress_index {
        proxy_progress_provider(
            &progress_providers[progress_index],
            context.clone(),
            request,
            &mut channel,
        )
        .await
    } else {
        execute_legacy_provider(
            &providers[provider_index],
            context.clone(),
            request,
            &mut channel,
        )
        .await
    };
    let hook_result = match &result {
        Ok(terminal) => {
            hook_contract::finish_hooks(
                &hooks,
                &context,
                &hook_execution,
                hook_contract::HookTerminal::Success,
                &terminal.content,
                &terminal.metadata_json,
                "",
            )
            .await
        }
        Err(PluginError::Domain(error)) => {
            hook_contract::finish_hooks(
                &hooks,
                &context,
                &hook_execution,
                hook_contract::HookTerminal::DomainError,
                "",
                "{}",
                execute_stream_error_code(error),
            )
            .await
        }
        Err(PluginError::Runtime(_)) => {
            hook_contract::finish_hooks(
                &hooks,
                &context,
                &hook_execution,
                hook_contract::HookTerminal::RuntimeFailure,
                "",
                "{}",
                "runtime_failure",
            )
            .await
        }
    };
    let terminal = match (result, hook_result) {
        (_, Err(error)) => Err(PluginError::runtime(error)),
        (Ok(_), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
    };
    let _ = channel.complete(terminal).await;
}

#[derive(Debug)]
struct ExecuteTerminal {
    content: String,
    metadata_json: String,
}

async fn proxy_progress_provider(
    provider: &progress_contract::ToolProgressClient,
    context: InvocationContext,
    request: ExecuteStreamRequest,
    channel: &mut ProviderStreamChannel<tools_contract::ToolsExecuteStream>,
) -> PluginResult<ExecuteTerminal, ExecuteStreamError> {
    let stream = provider
        .execute_progress_with_context(
            context,
            progress_contract::ExecuteOpen {
                name: request.name,
                arguments_json: request.arguments_json,
            },
        )
        .await
        .map_err(map_progress_open_error)?;
    stream.close_send().await.map_err(PluginError::runtime)?;
    let mut completed = None;
    loop {
        match stream.receive().await.map_err(PluginError::runtime)? {
            StreamEvent::Message(_) if completed.is_some() => {
                return Err(PluginError::runtime(RuntimeFailure::ProtocolViolation {
                    capability: progress_contract::CAPABILITY_ID,
                }));
            }
            StreamEvent::Message(message) => {
                let kind = match message.kind {
                    progress_contract::ExecuteProgressKind::Stdout => {
                        ExecuteStreamResponseKind::Stdout
                    }
                    progress_contract::ExecuteProgressKind::Stderr => {
                        ExecuteStreamResponseKind::Stderr
                    }
                    progress_contract::ExecuteProgressKind::Completed => {
                        completed = Some(ExecuteTerminal {
                            content: message.content.clone(),
                            metadata_json: message.metadata_json.as_str().to_owned(),
                        });
                        ExecuteStreamResponseKind::Completed
                    }
                };
                channel
                    .send(ExecuteStreamResponse {
                        kind,
                        content_type: ExecuteStreamResponseContentType::Text,
                        content: message.content,
                        metadata_json: message.metadata_json,
                    })
                    .await
                    .map_err(PluginError::runtime)?;
            }
            StreamEvent::PeerHalfClosed => {}
            StreamEvent::Terminal(Ok(())) if completed.is_some() => {
                return Ok(completed.expect("completed terminal was checked"));
            }
            StreamEvent::Terminal(Ok(())) => {
                return Err(PluginError::runtime(RuntimeFailure::ProtocolViolation {
                    capability: progress_contract::CAPABILITY_ID,
                }));
            }
            StreamEvent::Terminal(Err(error)) => {
                return Err(PluginError::domain(convert_progress_error(error)));
            }
        }
    }
}

async fn execute_legacy_provider(
    provider: &provider_contract::ToolProviderClient,
    context: InvocationContext,
    request: ExecuteStreamRequest,
    channel: &mut ProviderStreamChannel<tools_contract::ToolsExecuteStream>,
) -> PluginResult<ExecuteTerminal, ExecuteStreamError> {
    let response = provider
        .execute_with_context(
            context,
            provider_contract::ExecuteRequest {
                name: request.name,
                arguments_json: request.arguments_json,
            },
        )
        .await
        .map_err(|error| match error {
            provider_contract::ToolProviderExecuteInvocationError::Domain(error) => {
                PluginError::domain(convert_provider_stream_error(error))
            }
            provider_contract::ToolProviderExecuteInvocationError::Runtime(error) => {
                PluginError::runtime(error)
            }
        })?;
    let terminal = ExecuteTerminal {
        content: response.content.clone(),
        metadata_json: response.metadata_json.as_str().to_owned(),
    };
    channel
        .send(ExecuteStreamResponse {
            kind: ExecuteStreamResponseKind::Completed,
            content_type: ExecuteStreamResponseContentType::Text,
            content: response.content,
            metadata_json: response.metadata_json,
        })
        .await
        .map_err(PluginError::runtime)?;
    Ok(terminal)
}

fn execute_stream_error_code(error: &ExecuteStreamError) -> &str {
    match error {
        ExecuteStreamError::InvalidArguments => "invalid_arguments",
        ExecuteStreamError::UnknownTool => "unknown_tool",
        ExecuteStreamError::ToolError { payload } => &payload.provider_code,
        ExecuteStreamError::Unknown(unknown) => &unknown.code,
    }
}

fn map_progress_open_error(
    error: progress_contract::ToolProgressExecuteProgressInvocationError,
) -> PluginError<ExecuteStreamError> {
    match error {
        progress_contract::ToolProgressExecuteProgressInvocationError::Domain(error) => {
            PluginError::domain(convert_progress_error(error))
        }
        progress_contract::ToolProgressExecuteProgressInvocationError::Runtime(error) => {
            PluginError::runtime(error)
        }
    }
}

fn convert_progress_error(error: progress_contract::ExecuteProgressError) -> ExecuteStreamError {
    use progress_contract::ExecuteProgressError as ProgressError;
    match error {
        ProgressError::InvalidArguments => ExecuteStreamError::InvalidArguments,
        ProgressError::NotFound => {
            stream_tool_error("not_found", "Tool resource was not found", "{}")
        }
        ProgressError::OutputLimitExceeded => {
            stream_tool_error("output_limit_exceeded", "Tool output limit exceeded", "{}")
        }
        ProgressError::PermissionDenied => {
            stream_tool_error("permission_denied", "Tool permission denied", "{}")
        }
        ProgressError::ExecutionFailed { payload } => stream_tool_error(
            &payload.reason_code,
            &payload.message,
            payload.details_json.as_str(),
        ),
        ProgressError::Unknown(unknown) => stream_tool_error(
            &unknown.code,
            "Tool Progress Provider returned an unknown Domain Error",
            &unknown
                .payload
                .map_or_else(|| "{}".to_owned(), |value| value.to_string()),
        ),
    }
}

fn convert_provider_stream_error(error: provider_contract::ExecuteError) -> ExecuteStreamError {
    use provider_contract::ExecuteError as ProviderError;
    match error {
        ProviderError::InvalidArguments => ExecuteStreamError::InvalidArguments,
        ProviderError::NotFound => {
            stream_tool_error("not_found", "Tool resource was not found", "{}")
        }
        ProviderError::OutputLimitExceeded => {
            stream_tool_error("output_limit_exceeded", "Tool output limit exceeded", "{}")
        }
        ProviderError::PermissionDenied => {
            stream_tool_error("permission_denied", "Tool permission denied", "{}")
        }
        ProviderError::ExecutionFailed { payload } => stream_tool_error(
            &payload.reason_code,
            &payload.message,
            payload.details_json.as_str(),
        ),
        ProviderError::Unknown(unknown) => stream_tool_error(
            &unknown.code,
            "Tool Provider returned an unknown Domain Error",
            &unknown
                .payload
                .map_or_else(|| "{}".to_owned(), |value| value.to_string()),
        ),
    }
}

fn stream_tool_error(code: &str, message: &str, details_json: &str) -> ExecuteStreamError {
    ExecuteStreamError::ToolError {
        payload: ExecuteStreamErrorToolErrorPayload {
            provider_code: code.to_owned(),
            message: message.to_owned(),
            details_json: details_json
                .to_owned()
                .try_into()
                .expect("Tool error details must be valid JSON"),
        },
    }
}

fn valid_model_tool_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    matches!(bytes.first(), Some(b'a'..=b'z'))
        && bytes.len() <= 64
        && bytes[1..]
            .iter()
            .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_'))
}

fn convert_execute_response(response: provider_contract::ExecuteResponse) -> ExecuteResponse {
    ExecuteResponse {
        content: response.content,
        content_type: match response.content_type {
            provider_contract::ContentType::Text => ExecuteResponseContentType::Text,
        },
        metadata_json: response.metadata_json,
    }
}

fn convert_execute_error(error: provider_contract::ExecuteError) -> ExecuteError {
    use provider_contract::ExecuteError as ProviderError;
    match error {
        ProviderError::InvalidArguments => ExecuteError::InvalidArguments,
        ProviderError::NotFound => tool_error("not_found", "Tool resource was not found", "{}"),
        ProviderError::OutputLimitExceeded => {
            tool_error("output_limit_exceeded", "Tool output limit exceeded", "{}")
        }
        ProviderError::PermissionDenied => {
            tool_error("permission_denied", "Tool permission denied", "{}")
        }
        ProviderError::ExecutionFailed { payload } => tool_error(
            &payload.reason_code,
            &payload.message,
            payload.details_json.as_str(),
        ),
        ProviderError::Unknown(unknown) => tool_error(
            &unknown.code,
            "Tool Provider returned an unknown Domain Error",
            &unknown
                .payload
                .map_or_else(|| "{}".to_owned(), |value| value.to_string()),
        ),
    }
}

fn tool_error(code: &str, message: &str, details_json: &str) -> ExecuteError {
    ExecuteError::ToolError {
        payload: ExecuteErrorToolErrorPayload {
            provider_code: code.to_owned(),
            message: message.to_owned(),
            details_json: details_json
                .to_owned()
                .try_into()
                .expect("Tool error details must be valid JSON"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::valid_model_tool_name;

    #[test]
    fn model_tool_names_use_bounded_lowercase_snake_case() {
        for name in ["read", "create_file", "run_process", "skill_resource"] {
            assert!(valid_model_tool_name(name), "expected `{name}` to be valid");
        }
        for name in [
            "",
            "Read",
            "workspace.read",
            "read-file",
            "_read",
            "réad",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            assert!(
                !valid_model_tool_name(name),
                "expected `{name}` to be invalid"
            );
        }
    }
}
