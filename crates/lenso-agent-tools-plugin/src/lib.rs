//! Aggregate Tool Runtime Plugin.

use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

use lenso::prelude::*;
use lenso_agent_native_support::ToolTaskOwner;
use lenso_capability_agent_dynamic_authority as authority_contract;
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
use lenso_capability_agent_turn_processing as processing_contract;
use lenso_kernel::{InvocationContext, NativeStreamSession, RuntimeFailure, StreamEvent};
use sha2::{Digest as _, Sha256};

#[lenso::plugin(lifecycle)]
#[derive(Clone, Debug)]
struct ToolsPlugin {
    /// Final runtime authorization for a Host-issued dynamic resource snapshot.
    /// A missing sealed snapshot preserves static Plan behavior; a present one
    /// fails closed unless exactly one selected policy Provider revalidates it.
    authorities: ManyPort<authority_contract::DynamicAuthorityClient>,
    hooks: ManyPort<hook_contract::ToolHookClient>,
    providers: ManyPort<provider_contract::ToolProviderClient>,
    progress_providers: ManyPort<progress_contract::ToolProgressClient>,
    /// Ordered, Plan-bound argument processors run before final Tool Hooks.
    ///
    /// This is intentionally a narrow extension point: processors can only
    /// replace the canonical argument JSON. They never choose a Tool, bypass
    /// schema validation, or observe a Hook approval for different arguments.
    processors: ManyPort<processing_contract::TurnProcessingClient>,
    state: Rc<RefCell<Option<ToolRuntimeState>>>,
    #[tasks]
    tasks: ManagedTasks,
}

/// Explicitly retains this optional aggregate Plugin in a statically linked Host.
///
/// Hosts choose the call site; the package never self-installs a Tool runtime.
pub fn link() {
    __lenso_link_tools_plugin();
}

#[derive(Debug)]
struct ToolRuntimeState {
    catalog: Vec<CatalogResponseToolsItem>,
    routes: BTreeMap<String, usize>,
    progress_routes: BTreeMap<String, usize>,
    resource_identities: BTreeMap<String, authority_contract::ResourceIdentity>,
    schemas: BTreeMap<String, serde_json::Value>,
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

    #[allow(
        clippy::too_many_lines,
        reason = "ordered transformation, authorization, invocation and settlement form one execution boundary"
    )]
    fn execute(
        &self,
        context: InvocationContext,
        request: ExecuteRequest,
    ) -> lenso_kernel::NativeRequestFuture<ToolsExecute> {
        let route = self.state.borrow().as_ref().and_then(|state| {
            Some((
                state.routes.get(&request.name).copied()?,
                state.schemas.get(&request.name).cloned()?,
                state.resource_identities.get(&request.name).cloned()?,
            ))
        });
        let Some((index, schema, resource_identity)) = route else {
            return Box::pin(futures::future::ready(Ok(Err(ExecuteError::UnknownTool))));
        };
        let providers = self.providers.clone();
        let authorities = self.authorities.clone();
        let hooks = self.hooks.clone();
        let processors = self.processors.clone();
        Box::pin(async move {
            let Ok(arguments_json) =
                processing_contract::canonical_json(request.arguments_json.as_str())
            else {
                return Ok(Err(ExecuteError::InvalidArguments));
            };
            // A processor is allowed to repair an otherwise schema-invalid
            // call, so only JSON validity is checked here. The final value is
            // schema-validated below, immediately before authorization.
            let (turn_id, tool_call_id) = processing_call_identity(&context)?;
            let transformed = processing_contract::apply_tool_call_processors(
                &processors,
                &context,
                processing_contract::TransformToolCallRequest {
                    turn_id,
                    tool_call_id,
                    tool_name: request.name.clone(),
                    arguments_json: arguments_json
                        .try_into()
                        .expect("canonical Tool arguments must remain JSON"),
                },
            )
            .await?;
            let arguments_json = transformed.value.arguments_json.as_str().to_owned();
            if !arguments_match_schema(&schema, &arguments_json)? {
                return Ok(Err(ExecuteError::InvalidArguments));
            }
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
            let authority = revalidate_after_hooks(
                &authorities,
                &context,
                &hooks,
                &execution,
                resource_identity,
            )
            .await?;
            if let authority_contract::ToolAuthorityState::Denied {
                reason_code,
                message,
            } = authority
            {
                return Ok(Err(dynamic_authority_error(&reason_code, &message)));
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
                Ok(mut response) => {
                    response.metadata_json =
                        attach_processing_trace(response.metadata_json, &transformed.trace)?;
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

    #[allow(
        clippy::too_many_lines,
        reason = "stream admission preserves the same ordered execution boundary as request admission"
    )]
    fn execute_stream(
        &self,
        context: InvocationContext,
        request: ExecuteStreamRequest,
    ) -> futures::future::LocalBoxFuture<
        'static,
        Result<Box<dyn NativeStreamSession>, ToolsExecuteStreamInvocationError>,
    > {
        let route = self.state.borrow().as_ref().and_then(|state| {
            Some((
                state.routes.get(&request.name).copied()?,
                state.schemas.get(&request.name).cloned()?,
                state.resource_identities.get(&request.name).cloned()?,
            ))
        });
        let Some((provider_index, schema, resource_identity)) = route else {
            return Box::pin(futures::future::ready(Err(
                ToolsExecuteStreamInvocationError::Domain(ExecuteStreamError::UnknownTool),
            )));
        };
        let progress_index = self
            .state
            .borrow()
            .as_ref()
            .and_then(|state| state.progress_routes.get(&request.name).copied());
        let providers = self.providers.clone();
        let progress_providers = self.progress_providers.clone();
        let authorities = self.authorities.clone();
        let hooks = self.hooks.clone();
        let processors = self.processors.clone();
        let tasks = self.tasks.clone();
        Box::pin(async move {
            let arguments_json = processing_contract::canonical_json(
                request.arguments_json.as_str(),
            )
            .map_err(|_| {
                ToolsExecuteStreamInvocationError::Domain(ExecuteStreamError::InvalidArguments)
            })?;
            // As in the request/reply operation, validate the provider schema
            // only after all ordered argument transformations have completed.
            let (turn_id, tool_call_id) = processing_call_identity(&context)
                .map_err(ToolsExecuteStreamInvocationError::Runtime)?;
            let transformed = processing_contract::apply_tool_call_processors(
                &processors,
                &context,
                processing_contract::TransformToolCallRequest {
                    turn_id,
                    tool_call_id,
                    tool_name: request.name.clone(),
                    arguments_json: arguments_json
                        .try_into()
                        .expect("canonical Tool arguments must remain JSON"),
                },
            )
            .await
            .map_err(ToolsExecuteStreamInvocationError::Runtime)?;
            let arguments_json = transformed.value.arguments_json.as_str().to_owned();
            if !arguments_match_schema(&schema, &arguments_json)
                .map_err(ToolsExecuteStreamInvocationError::Runtime)?
            {
                return Err(ToolsExecuteStreamInvocationError::Domain(
                    ExecuteStreamError::InvalidArguments,
                ));
            }
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
            let authority = revalidate_after_hooks(
                &authorities,
                &context,
                &hooks,
                &execution,
                resource_identity,
            )
            .await
            .map_err(ToolsExecuteStreamInvocationError::Runtime)?;
            if let authority_contract::ToolAuthorityState::Denied {
                reason_code,
                message,
            } = authority
            {
                return Err(ToolsExecuteStreamInvocationError::Domain(
                    dynamic_authority_stream_error(&reason_code, &message),
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
                        transformed.trace,
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
        let mut resource_identities = BTreeMap::new();
        let mut schemas = BTreeMap::new();
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
                let schema = parse_tool_schema(&tool.name, tool.input_schema_json.as_str())?;
                let schema_digest = format!(
                    "sha256:{:x}",
                    Sha256::digest(tool.input_schema_json.as_str())
                );
                resource_identities.insert(
                    tool.name.clone(),
                    authority_contract::ResourceIdentity {
                        kind: "tool".to_owned(),
                        name: tool.name.clone(),
                        provider_instance: provider.provider_instance().to_owned(),
                        revision: schema_digest,
                    },
                );
                schemas.insert(tool.name.clone(), schema);
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
            resource_identities,
            schemas,
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
    processing_trace: Vec<processing_contract::TransformationTrace>,
    mut channel: ProviderStreamChannel<tools_contract::ToolsExecuteStream>,
) {
    // `execute_stream` has all request data in its open payload. Its input
    // direction is therefore only a readiness barrier: retaining it until the
    // caller half-closes prevents a fast provider from racing the consumer's
    // close and makes the stream contract identical for built-in and external
    // Agent strategies.
    let result = match channel.receive().await {
        Ok(StreamInput::PeerHalfClosed) => {
            if let Some(progress_index) = progress_index {
                proxy_progress_provider(
                    &progress_providers[progress_index],
                    context.clone(),
                    request,
                    &processing_trace,
                    &mut channel,
                )
                .await
            } else {
                execute_legacy_provider(
                    &providers[provider_index],
                    context.clone(),
                    request,
                    &processing_trace,
                    &mut channel,
                )
                .await
            }
        }
        Ok(StreamInput::Message(_)) => {
            Err(PluginError::runtime(RuntimeFailure::ProtocolViolation {
                capability: tools_contract::CAPABILITY_ID,
            }))
        }
        Err(error) => Err(PluginError::runtime(error)),
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
    processing_trace: &[processing_contract::TransformationTrace],
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
                let mut metadata_json = message.metadata_json;
                let kind = match message.kind {
                    progress_contract::ExecuteProgressKind::Stdout => {
                        ExecuteStreamResponseKind::Stdout
                    }
                    progress_contract::ExecuteProgressKind::Stderr => {
                        ExecuteStreamResponseKind::Stderr
                    }
                    progress_contract::ExecuteProgressKind::Completed => {
                        metadata_json = attach_processing_trace(metadata_json, processing_trace)
                            .map_err(PluginError::runtime)?;
                        completed = Some(ExecuteTerminal {
                            content: message.content.clone(),
                            metadata_json: metadata_json.as_str().to_owned(),
                        });
                        ExecuteStreamResponseKind::Completed
                    }
                };
                channel
                    .send(ExecuteStreamResponse {
                        kind,
                        content_type: ExecuteStreamResponseContentType::Text,
                        content: message.content,
                        content_blocks: parse_content_blocks(metadata_json.as_str()),
                        metadata_json,
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
    processing_trace: &[processing_contract::TransformationTrace],
    channel: &mut ProviderStreamChannel<tools_contract::ToolsExecuteStream>,
) -> PluginResult<ExecuteTerminal, ExecuteStreamError> {
    let mut response = provider
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
    response.metadata_json = attach_processing_trace(response.metadata_json, processing_trace)
        .map_err(PluginError::runtime)?;
    let content_blocks = response
        .content_blocks
        .clone()
        .flatten()
        .map(convert_content_blocks);
    let terminal = ExecuteTerminal {
        content: response.content.clone(),
        metadata_json: response.metadata_json.as_str().to_owned(),
    };
    channel
        .send(ExecuteStreamResponse {
            kind: ExecuteStreamResponseKind::Completed,
            content_type: ExecuteStreamResponseContentType::Text,
            content: response.content,
            content_blocks,
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

/// Maps a final dynamic selection rejection to the existing public Tool error
/// shape. The structured reason is evidence, not executable authority.
// Approval can suspend execution. Revalidate after it returns, so revocation
// during that wait cannot become an approved side effect. Settle started hooks
// even when the policy rejects or cannot be reached.
async fn revalidate_after_hooks(
    authorities: &ManyPort<authority_contract::DynamicAuthorityClient>,
    context: &InvocationContext,
    hooks: &ManyPort<hook_contract::ToolHookClient>,
    execution: &hook_contract::HookExecution,
    resource: authority_contract::ResourceIdentity,
) -> Result<authority_contract::ToolAuthorityState, RuntimeFailure> {
    let result =
        authority_contract::revalidate_tool_authority(authorities, context, resource).await;
    let terminal = match &result {
        Ok(authority_contract::ToolAuthorityState::Denied { .. }) => Some((
            hook_contract::HookTerminal::DomainError,
            "dynamic_authorization_denied",
        )),
        Err(_) => Some((
            hook_contract::HookTerminal::RuntimeFailure,
            "runtime_failure",
        )),
        _ => None,
    };
    if let Some((terminal, code)) = terminal {
        hook_contract::finish_hooks(hooks, context, execution, terminal, "", "{}", code).await?;
    }
    result
}

fn dynamic_authority_error(reason_code: &str, message: &str) -> ExecuteError {
    tool_error(
        "dynamic_authorization_denied",
        message,
        &serde_json::json!({
            "schema": "lenso.agent.dynamic-authority.denial@1",
            "reason_code": reason_code,
        })
        .to_string(),
    )
}

fn dynamic_authority_stream_error(reason_code: &str, message: &str) -> ExecuteStreamError {
    stream_tool_error(
        "dynamic_authorization_denied",
        message,
        &serde_json::json!({
            "schema": "lenso.agent.dynamic-authority.denial@1",
            "reason_code": reason_code,
        })
        .to_string(),
    )
}

fn valid_model_tool_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    matches!(bytes.first(), Some(b'a'..=b'z'))
        && bytes.len() <= 64
        && bytes[1..]
            .iter()
            .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_'))
}

const TURN_PROCESSING_METADATA_KEY: &str = "lenso.agent.turn-processing@1";

/// Uses the Loop's durable Tool-call identity when it is present, while
/// retaining a stable correlation identity for direct Tools consumers.
fn processing_call_identity(
    context: &InvocationContext,
) -> Result<(String, String), RuntimeFailure> {
    match context
        .typed_extension::<ToolTaskOwner>()
        .map_err(|error| RuntimeFailure::PluginFailure {
            detail: format!("Tool task ownership extension was malformed: {error}"),
        })? {
        Some(owner) => Ok((owner.turn_id, owner.tool_call_id)),
        None => Ok((
            format!("request-{}", context.request_id()),
            format!("tool-{}", context.request_id()),
        )),
    }
}

/// Parses and preflights one provider-advertised schema during activation.
///
/// Tool schemas are Plan input. Rejecting external references here prevents a
/// later Tool invocation from turning schema validation into an ambient file
/// or network lookup.
fn parse_tool_schema(name: &str, schema_json: &str) -> Result<serde_json::Value, RuntimeFailure> {
    let schema = serde_json::from_str::<serde_json::Value>(schema_json).map_err(|error| {
        RuntimeFailure::InvalidResolvedPlan {
            detail: format!("Tool `{name}` returned invalid input schema JSON: {error}"),
        }
    })?;
    if schema_has_external_reference(&schema) {
        return Err(RuntimeFailure::InvalidResolvedPlan {
            detail: format!(
                "Tool `{name}` input schema contains an external $ref; only document-local references are supported"
            ),
        });
    }
    jsonschema::validator_for(&schema).map_err(|error| RuntimeFailure::InvalidResolvedPlan {
        detail: format!("Tool `{name}` returned an invalid input schema: {error}"),
    })?;
    Ok(schema)
}

fn schema_has_external_reference(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Array(values) => values.iter().any(schema_has_external_reference),
        serde_json::Value::Object(object) => object.iter().any(|(key, value)| {
            (key == "$ref"
                && value
                    .as_str()
                    .is_some_and(|reference| !reference.starts_with('#')))
                || schema_has_external_reference(value)
        }),
        _ => false,
    }
}

/// Validates the exact canonical argument value at the authority boundary.
/// A processor may transform a valid value into an invalid one, so this check
/// intentionally runs both before and after the ordered processor chain.
fn arguments_match_schema(
    schema: &serde_json::Value,
    arguments_json: &str,
) -> Result<bool, RuntimeFailure> {
    let arguments = serde_json::from_str::<serde_json::Value>(arguments_json).map_err(|error| {
        RuntimeFailure::PluginFailure {
            detail: format!("canonical Tool arguments became invalid JSON: {error}"),
        }
    })?;
    let validator =
        jsonschema::validator_for(schema).map_err(|error| RuntimeFailure::Internal {
            detail: format!("prevalidated Tool schema no longer compiles: {error}"),
        })?;
    Ok(validator.is_valid(&arguments))
}

/// Adds processor provenance without overwriting factual provider metadata.
///
/// The aggregate owns this response envelope. A provider that uses a
/// non-object metadata value cannot supply a safe namespace for Plan evidence,
/// so the invocation fails closed when a processor was selected.
fn attach_processing_trace(
    metadata_json: processing_contract::RawJson,
    trace: &[processing_contract::TransformationTrace],
) -> Result<processing_contract::RawJson, RuntimeFailure> {
    if trace.is_empty() {
        return Ok(metadata_json);
    }
    let mut metadata =
        serde_json::from_str::<serde_json::Value>(metadata_json.as_str()).map_err(|error| {
            RuntimeFailure::PluginFailure {
                detail: format!("Tool Provider returned invalid metadata JSON: {error}"),
            }
        })?;
    let object = metadata
        .as_object_mut()
        .ok_or_else(|| RuntimeFailure::PluginFailure {
            detail: "Tool Provider metadata must be a JSON object when Turn Processing is selected"
                .to_owned(),
        })?;
    if object.contains_key(TURN_PROCESSING_METADATA_KEY) {
        return Err(RuntimeFailure::PluginFailure {
            detail: format!(
                "Tool Provider metadata reserves `{TURN_PROCESSING_METADATA_KEY}` for aggregate evidence"
            ),
        });
    }
    object.insert(
        TURN_PROCESSING_METADATA_KEY.to_owned(),
        serde_json::to_value(trace).map_err(|error| RuntimeFailure::Internal {
            detail: format!("failed to encode Tool processing evidence: {error}"),
        })?,
    );
    serde_json::to_string(&metadata)
        .map_err(|error| RuntimeFailure::Internal {
            detail: format!("failed to serialize Tool processing metadata: {error}"),
        })?
        .try_into()
        .map_err(|_| RuntimeFailure::Internal {
            detail: "aggregate-generated Tool metadata was not valid JSON".to_owned(),
        })
}

fn convert_execute_response(response: provider_contract::ExecuteResponse) -> ExecuteResponse {
    let content_blocks = response
        .content_blocks
        .flatten()
        .map(convert_content_blocks);
    ExecuteResponse {
        content: response.content,
        content_blocks,
        content_type: match response.content_type {
            provider_contract::ContentType::Text => ExecuteResponseContentType::Text,
        },
        metadata_json: response.metadata_json,
    }
}

fn convert_content_blocks<T, U>(blocks: Vec<T>) -> Vec<U>
where
    T: serde::Serialize,
    U: serde::de::DeserializeOwned,
{
    serde_json::from_value(
        serde_json::to_value(blocks).expect("Tool Provider content blocks are serializable"),
    )
    .expect("Tool Provider and aggregate Tool content block schemas are aligned")
}

fn parse_content_blocks<T>(metadata_json: &str) -> Option<Vec<T>>
where
    T: serde::de::DeserializeOwned,
{
    let metadata = serde_json::from_str::<serde_json::Value>(metadata_json).ok()?;
    let blocks = metadata.get("content_blocks")?.clone();
    serde_json::from_value::<Vec<T>>(blocks)
        .ok()
        .filter(|blocks| !blocks.is_empty())
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
