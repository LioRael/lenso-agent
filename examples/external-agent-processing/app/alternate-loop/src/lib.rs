//! A compact third-party Agent strategy that consumes only public contracts.

use futures::future::LocalBoxFuture;
use lenso::prelude::*;
use lenso_capability_agent::{
    self as agent, AgentInvocationError, AgentProvider, ModelFailurePayload, RunTurnError,
    RunTurnRequest, RunTurnResponse, RunTurnResponseKind,
};
use lenso_capability_agent_model::{
    self as model, CompleteError, CompleteMessageInput, CompleteMessageRole, CompleteOpen,
    ModelCompleteEvent,
};
use lenso_capability_agent_tools::{
    self as tools, ExecuteStreamRequest, ExecuteStreamResponseKind,
    ToolsExecuteStreamInvocationError,
};
use lenso_capability_agent_turn_processing as processing;
use lenso_kernel::{InvocationContext, NativeStreamSession, RuntimeFailure, StreamEvent};

const TURN_ID: &str = "external-v04-turn";
const TOOL_CALL_ID: &str = "external-v04-call";

#[lenso::plugin]
#[derive(Clone, Debug)]
struct AlternateLoop {
    model: Port<model::ModelClient>,
    tools: Port<tools::ToolsClient>,
    processors: ManyPort<processing::TurnProcessingClient>,
    #[tasks]
    tasks: ManagedTasks,
}

pub fn link() {
    link_plugin();
}

#[lenso::provides(agent::Agent)]
impl AgentProvider for AlternateLoop {
    fn run_turn(
        &self,
        context: InvocationContext,
        request: RunTurnRequest,
    ) -> LocalBoxFuture<'static, Result<Box<dyn NativeStreamSession>, AgentInvocationError>> {
        let plugin = self.clone();
        Box::pin(async move {
            if context.is_cancelled() {
                return Err(AgentInvocationError::Runtime(RuntimeFailure::Cancelled {
                    request_id: context.request_id(),
                }));
            }
            if request.input.trim().is_empty() {
                return Err(AgentInvocationError::Domain(
                    RunTurnError::ContextLimitExceeded,
                ));
            }
            let (stream, channel) = ProviderStream::<agent::Agent>::channel(&context, 8);
            let task_plugin = plugin.clone();
            plugin
                .tasks
                .spawn_local(async move {
                    task_plugin.produce(context, request, channel).await;
                })
                .map_err(|error| {
                    AgentInvocationError::Runtime(RuntimeFailure::PluginFailure {
                        detail: format!("external Agent turn task failed to start: {error:?}"),
                    })
                })?;
            Ok(Box::new(stream) as Box<dyn NativeStreamSession>)
        })
    }
}

impl AlternateLoop {
    async fn produce(
        &self,
        context: InvocationContext,
        request: RunTurnRequest,
        mut channel: ProviderStreamChannel<agent::Agent>,
    ) {
        // `run_turn` has no post-open client messages. Holding this direction
        // until half-close makes the source stream safe for a fast external
        // strategy and preserves the standard Agent stream handshake.
        let result = match channel.receive().await {
            Ok(StreamInput::PeerHalfClosed) => self.run(&context, request, &mut channel).await,
            Ok(StreamInput::Message(_)) => Err(AgentInvocationError::Runtime(
                RuntimeFailure::ProtocolViolation {
                    capability: agent::CAPABILITY_ID,
                },
            )),
            Err(error) => Err(AgentInvocationError::Runtime(error)),
        };
        let terminal = match result {
            Ok(()) => Ok(()),
            Err(AgentInvocationError::Domain(error)) => Err(PluginError::domain(error)),
            Err(AgentInvocationError::Runtime(error)) => Err(PluginError::runtime(error)),
        };
        let _ = channel.complete(terminal).await;
    }

    async fn run(
        &self,
        context: &InvocationContext,
        request: RunTurnRequest,
        channel: &mut ProviderStreamChannel<agent::Agent>,
    ) -> Result<(), AgentInvocationError> {
        // The aggregate runtime owns pre-authorization argument transformation,
        // schema revalidation, and final Tool Hook approval. This Loop receives
        // the immutable execution result only after those authority checks.
        let (tool_content, tool_metadata) = self.execute_tool(context).await?;
        let projected_tool = processing::apply_tool_result_processors(
            &self.processors,
            context,
            processing::ProjectToolResultRequest {
                turn_id: TURN_ID.to_owned(),
                fact: processing::ToolResultFact {
                    tool_call_id: TOOL_CALL_ID.to_owned(),
                    tool_name: "read_secret".to_owned(),
                    arguments_json:
                        r#"{"id":"raw"}"#.try_into().expect("fixture arguments are valid JSON"),
                    outcome: processing::ToolResultOutcome::Success,
                    content: tool_content,
                    metadata_json: tool_metadata,
                    provider_code: String::new(),
                },
                presentation: "Tool returned a SECRET_RESULT.".to_owned(),
            },
        )
        .await
        .map_err(AgentInvocationError::Runtime)?;

        let mut model_request = CompleteOpen {
            continuation_scope: Some(TURN_ID.to_owned()),
            model: "example/inspect-v1".to_owned(),
            reasoning_effort: None,
            reasoning_enabled: None,
            reasoning_budget_tokens: None,
            service_tier: None,
            messages: vec![
                CompleteMessageInput {
                    images: None,
                    role: CompleteMessageRole::User,
                    content: request.input,
                    tool_call_id: None,
                    tool_name: None,
                    arguments_json: None,
                },
                CompleteMessageInput {
                    images: None,
                    role: CompleteMessageRole::Tool,
                    content: projected_tool.value.presentation,
                    tool_call_id: Some(TOOL_CALL_ID.to_owned()),
                    tool_name: Some("read_secret".to_owned()),
                    arguments_json: None,
                },
            ],
            tools: Vec::new(),
            temperature: 0.0,
            max_output_tokens: 128,
        };
        let projected_request = processing::apply_model_request_processors(
            &self.processors,
            context,
            processing::model_request_from_model(
                TURN_ID,
                &model_request.messages,
                &model_request.tools,
            ),
        )
        .await
        .map_err(AgentInvocationError::Runtime)?;
        let (messages, tools) = processing::model_request_to_model(&projected_request.value)
            .map_err(AgentInvocationError::Runtime)?;
        model_request.messages = messages;
        model_request.tools = tools;

        let text = self.complete_model(context, model_request).await?;
        let metadata_json = serde_json::json!({
            "schema": "example.external-agent-processing.v1",
            "model_request": projected_request.trace,
            "tool_result": projected_tool.trace,
        })
        .to_string()
        .try_into()
        .expect("fixture Agent metadata is valid JSON");
        channel
            .send(RunTurnResponse {
                arguments_json: None,
                content: None,
                duration_ms: None,
                error: None,
                kind: Some(RunTurnResponseKind::TextDelta),
                metadata_json: Some(metadata_json),
                progress_channel: None,
                reasoning_id: None,
                sequence: "1".to_owned(),
                session_id: None,
                text,
                tool_call_id: None,
                tool_name: None,
            })
            .await
            .map_err(AgentInvocationError::Runtime)?;
        Ok(())
    }

    async fn execute_tool(
        &self,
        context: &InvocationContext,
    ) -> Result<(String, processing::RawJson), AgentInvocationError> {
        let stream = self
            .tools
            .execute_stream_with_context(
                context.clone(),
                ExecuteStreamRequest {
                    name: "read_secret".to_owned(),
                    arguments_json:
                        r#"{"id":"raw"}"#.try_into().expect("fixture arguments are valid JSON"),
                },
            )
            .await
            .map_err(map_tool_open)?;
        stream
            .close_send()
            .await
            .map_err(AgentInvocationError::Runtime)?;
        let mut completed = None;
        loop {
            match stream
                .receive()
                .await
                .map_err(AgentInvocationError::Runtime)?
            {
                StreamEvent::Message(message)
                    if message.kind == ExecuteStreamResponseKind::Completed =>
                {
                    if completed
                        .replace((message.content, message.metadata_json))
                        .is_some()
                    {
                        return Err(strategy_failure("Tool stream completed more than once"));
                    }
                }
                StreamEvent::Message(_) | StreamEvent::PeerHalfClosed => {}
                StreamEvent::Terminal(Ok(())) => {
                    return completed
                        .ok_or_else(|| strategy_failure("Tool stream ended without a result"));
                }
                StreamEvent::Terminal(Err(error)) => {
                    return Err(strategy_failure(&format!(
                        "Tool rejected the call: {error:?}"
                    )));
                }
            }
        }
    }

    async fn complete_model(
        &self,
        context: &InvocationContext,
        request: CompleteOpen,
    ) -> Result<String, AgentInvocationError> {
        let stream = self
            .model
            .complete_with_context(context.clone(), request)
            .await
            .map_err(map_model_open)?;
        stream
            .close_send()
            .await
            .map_err(AgentInvocationError::Runtime)?;
        let mut text = String::new();
        loop {
            match stream
                .receive()
                .await
                .map_err(AgentInvocationError::Runtime)?
            {
                ModelCompleteEvent::Message(message) => match message.kind {
                    model::CompleteMessageKind::TextDelta => text.push_str(&message.text),
                    model::CompleteMessageKind::ReasoningSummaryDelta
                    | model::CompleteMessageKind::Usage => {}
                    model::CompleteMessageKind::ToolCall => {
                        return Err(strategy_failure(
                            "inspection model unexpectedly requested a Tool",
                        ));
                    }
                },
                StreamEvent::PeerHalfClosed => {}
                StreamEvent::Terminal(Ok(())) => return Ok(text),
                StreamEvent::Terminal(Err(error)) => {
                    return Err(strategy_failure(&format!(
                        "Model rejected the request: {error:?}"
                    )));
                }
            }
        }
    }
}

fn map_tool_open(error: ToolsExecuteStreamInvocationError) -> AgentInvocationError {
    match error {
        ToolsExecuteStreamInvocationError::Runtime(error) => AgentInvocationError::Runtime(error),
        ToolsExecuteStreamInvocationError::Domain(error) => {
            strategy_failure(&format!("Tool call was rejected: {error:?}"))
        }
    }
}

fn map_model_open(error: model::ModelCompleteInvocationError) -> AgentInvocationError {
    match error {
        model::ModelCompleteInvocationError::Runtime(error) => AgentInvocationError::Runtime(error),
        model::ModelCompleteInvocationError::Domain(error) => {
            strategy_failure(&format!("Model request was rejected: {error:?}"))
        }
    }
}

fn strategy_failure(message: &str) -> AgentInvocationError {
    AgentInvocationError::Domain(RunTurnError::ModelFailure {
        payload: ModelFailurePayload {
            reason_code: "external_strategy_failed".to_owned(),
            message: message.to_owned(),
        },
    })
}

#[allow(dead_code)]
fn _model_error_is_public(_: CompleteError) {}
