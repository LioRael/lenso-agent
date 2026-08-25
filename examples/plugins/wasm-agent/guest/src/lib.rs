use std::{cell::RefCell, collections::BTreeMap};

use lenso_capability_agent::{
    CAPABILITY_ID as AGENT_CAPABILITY_ID, RunTurnError, RunTurnRequest, RunTurnResponse,
};
use lenso_capability_agent_model::{
    CompleteRequest, CompleteRequestMessagesItem, CompleteRequestMessagesItemRole,
    CompleteRequestToolsItem, CompleteResponse, CompleteResponseKind, ModelGuestClient,
};
use lenso_capability_agent_prompt::{AssembleRequest, PromptGuestClient};
use lenso_capability_agent_session::{OpenRequest, SessionGuestClient};
use lenso_capability_agent_tools::{CatalogRequest, ToolsGuestClient};
use lenso_guest_sdk::{GuestContext, GuestStream, GuestStreamEvent};

wit_bindgen::generate!({
    path: "wit",
    world: "plugin",
});

lenso_guest_sdk::wasm_host! {
    struct WasmHost {
        bindings: host_bindings,
        invoke: host_invoke,
        stream_open: host_stream_open,
        stream_send: host_stream_send,
        stream_receive: host_stream_receive,
        stream_close_send: host_stream_close_send,
        stream_cancel: host_stream_cancel,
    }
}

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
    model: GuestStream<WasmHost, CompleteResponse, lenso_capability_agent_model::CompleteError>,
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
        r#"{"abi":"lenso.json-host-imports@1","capabilities":[{"capability_id":"lenso.agent@1","descriptor_version":"1.1.0","request_operations":[],"stream_operations":["run_turn"]}],"required_capabilities":[{"capability_id":"lenso.agent.model@1","descriptor_version":"1.1.0","cardinality":"one"},{"capability_id":"lenso.agent.prompt@1","descriptor_version":"1.0.0","cardinality":"one"},{"capability_id":"lenso.agent.session@1","descriptor_version":"1.1.0","cardinality":"one"},{"capability_id":"lenso.agent.tools@1","descriptor_version":"1.0.0","cardinality":"one"}]}"#.to_owned()
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
            .open(&OpenRequest {
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
            .complete(&CompleteRequest {
                model: "fixture/readme-summary-v1".to_owned(),
                messages: vec![
                    CompleteRequestMessagesItem {
                        role: CompleteRequestMessagesItemRole::System,
                        content: prompt.content,
                        arguments_json: None,
                        tool_call_id: None,
                        tool_name: None,
                    },
                    CompleteRequestMessagesItem {
                        role: CompleteRequestMessagesItemRole::User,
                        content: format!("Answer directly: {}", request.input),
                        arguments_json: None,
                        tool_call_id: None,
                        tool_name: None,
                    },
                ],
                tools: tools
                    .tools
                    .into_iter()
                    .map(|tool| CompleteRequestToolsItem {
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
                        if message.kind == CompleteResponseKind::TextDelta
                            && !message.text.is_empty() =>
                    {
                        let response = RunTurnResponse {
                            sequence: session.sequence.to_string(),
                            text: message.text,
                            session_id: Some(session.session_id.clone()),
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
