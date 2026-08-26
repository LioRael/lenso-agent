use std::{cell::RefCell, collections::BTreeMap};

use lenso_capability_agent::{
    self as agent_capability, CAPABILITY_ID as AGENT_CAPABILITY_ID, RunTurnError,
    RunTurnRequest, RunTurnResponse,
};
use lenso_capability_agent_model::{
    self as model_capability, CompleteMessage, CompleteMessageInput, CompleteMessageKind,
    CompleteMessageRole, CompleteOpen, CompleteTool, ModelGuestClient,
};
use lenso_capability_agent_prompt::{self as prompt_capability, AssembleRequest, PromptGuestClient};
use lenso_capability_agent_session::{
    self as session_capability, OpenSessionRequest, SessionGuestClient,
};
use lenso_capability_agent_tools::{self as tools_capability, CatalogRequest, ToolsGuestClient};
use lenso_guest_sdk::{GuestContext, GuestStream, GuestStreamEvent};

wit_bindgen::generate!({
    path: "wit",
    world: "plugin",
});

lenso_guest_sdk::wasm_host!(struct WasmHost);

struct AgentContext<'a> {
    model: ModelGuestClient<'a, WasmHost>,
    prompt: PromptGuestClient<'a, WasmHost>,
    session: SessionGuestClient<'a, WasmHost>,
    tools: ToolsGuestClient<'a, WasmHost>,
}

impl<'a> AgentContext<'a> {
    fn load(context: &'a GuestContext<WasmHost>) -> Result<Self, String> {
        Ok(Self {
            model: ModelGuestClient::from_context(context)
                .map_err(|error| format!("Model binding: {error:?}"))?,
            prompt: PromptGuestClient::from_context(context)
                .map_err(|error| format!("Prompt binding: {error:?}"))?,
            session: SessionGuestClient::from_context(context)
                .map_err(|error| format!("Session binding: {error:?}"))?,
            tools: ToolsGuestClient::from_context(context)
                .map_err(|error| format!("Tools binding: {error:?}"))?,
        })
    }
}

struct TurnSession {
    model: GuestStream<WasmHost, CompleteMessage, lenso_capability_agent_model::CompleteError>,
    session_id: String,
    sequence: u64,
}

struct State {
    next_id: u64,
    sessions: BTreeMap<u64, TurnSession>,
}

thread_local! {
    static STATE: RefCell<State> = const { RefCell::new(State {
        next_id: 1,
        sessions: BTreeMap::new(),
    }) };
}

struct WasmAgent;

impl Guest for WasmAgent {
    fn describe() -> String {
        lenso_guest_sdk::guest_descriptor! {
            provides: [agent_capability {
                requests: [],
                streams: [agent_capability::RUN_TURN_OPERATION],
            }],
            requires: [
                model_capability,
                prompt_capability,
                session_capability,
                tools_capability,
            ],
        }
    }

    fn invoke(_: String, _: String, _: String) -> Result<String, String> {
        Err("the Agent Capability exposes only the run_turn Stream Operation".to_owned())
    }

    fn stream_open(
        capability: String,
        operation: String,
        request_json: String,
    ) -> Result<u64, String> {
        if capability != AGENT_CAPABILITY_ID || operation != "run_turn" {
            return Err("unsupported Capability or Operation".to_owned());
        }
        let request: RunTurnRequest = serde_json::from_str(&request_json)
            .map_err(|error| format!("invalid run_turn request: {error}"))?;
        let context =
            GuestContext::load(WasmHost).map_err(|error| format!("Host bindings: {error:?}"))?;
        let agent = AgentContext::load(&context)?;
        let opened = agent
            .session
            .open(&OpenSessionRequest {
                session_id: request.session_id,
            })
            .map_err(|error| format!("session.open: {error:?}"))?;
        let prompt = agent
            .prompt
            .assemble(&AssembleRequest {})
            .map_err(|error| format!("prompt.assemble: {error:?}"))?;
        let tools = agent
            .tools
            .catalog(&CatalogRequest {})
            .map_err(|error| format!("tools.catalog: {error:?}"))?;
        let model = agent
            .model
            .complete(&CompleteOpen {
                model: "fixture/readme-summary-v1".to_owned(),
                messages: vec![
                    CompleteMessageInput {
                        role: CompleteMessageRole::System,
                        content: prompt.content,
                        arguments_json: None,
                        tool_call_id: None,
                        tool_name: None,
                    },
                    CompleteMessageInput {
                        role: CompleteMessageRole::User,
                        content: format!("Answer directly: {}", request.input),
                        arguments_json: None,
                        tool_call_id: None,
                        tool_name: None,
                    },
                ],
                tools: tools
                    .tools
                    .into_iter()
                    .map(|tool| CompleteTool {
                        name: tool.name,
                        description: tool.description,
                        input_schema_json: tool.input_schema_json,
                    })
                    .collect(),
                temperature: 0.0,
                max_output_tokens: 128,
            })
            .map_err(|error| format!("model.complete: {error:?}"))?;

        STATE.with_borrow_mut(|state| {
            let id = state.next_id;
            state.next_id += 1;
            state.sessions.insert(
                id,
                TurnSession {
                    model,
                    session_id: opened.session_id,
                    sequence: 0,
                },
            );
            Ok(id)
        })
    }

    fn stream_send(stream_id: u64, _: String) -> Result<(), String> {
        STATE.with_borrow(|state| {
            state
                .sessions
                .contains_key(&stream_id)
                .then_some(())
                .ok_or_else(|| "unknown stream".to_owned())
        })
    }

    fn stream_receive(stream_id: u64) -> Result<String, String> {
        STATE.with_borrow_mut(|state| {
            loop {
                let session = state
                    .sessions
                    .get_mut(&stream_id)
                    .ok_or_else(|| "unknown stream".to_owned())?;
                match session
                    .model
                    .receive()
                    .map_err(|error| format!("model.receive: {error:?}"))?
                {
                    GuestStreamEvent::Message(message)
                        if message.kind == CompleteMessageKind::TextDelta
                            && !message.text.is_empty() =>
                    {
                        let response = RunTurnResponse {
                            arguments_json: None,
                            content: None,
                            duration_ms: None,
                            error: None,
                            kind: Some(lenso_capability_agent::RunTurnResponseKind::TextDelta),
                            metadata_json: None,
                            progress_channel: None,
                            reasoning_id: None,
                            sequence: session.sequence.to_string(),
                            text: message.text,
                            session_id: Some(session.session_id.clone()),
                            tool_call_id: None,
                            tool_name: None,
                        };
                        session.sequence += 1;
                        return Ok(serde_json::json!({
                            "kind": "message",
                            "value": response,
                        })
                        .to_string());
                    }
                    GuestStreamEvent::Message(_) | GuestStreamEvent::PeerHalfClosed => {}
                    GuestStreamEvent::Terminal(Ok(())) => {
                        state.sessions.remove(&stream_id);
                        return Ok(r#"{"kind":"terminal-success"}"#.to_owned());
                    }
                    GuestStreamEvent::Terminal(Err(_)) => {
                        state.sessions.remove(&stream_id);
                        return Ok(serde_json::json!({
                            "kind": "terminal-error",
                            "value": RunTurnError::ContextLimitExceeded,
                        })
                        .to_string());
                    }
                }
            }
        })
    }

    fn stream_close_send(stream_id: u64) -> Result<(), String> {
        Self::stream_send(stream_id, String::new())
    }

    fn stream_cancel(stream_id: u64) {
        STATE.with_borrow_mut(|state| {
            state.sessions.remove(&stream_id);
        });
    }
}

export!(WasmAgent);
