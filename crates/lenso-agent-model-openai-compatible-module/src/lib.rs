//! OpenAI-compatible Chat Completions Model Module.

use std::{
    any::Any,
    cell::{Cell, RefCell},
    collections::{BTreeMap, VecDeque},
    fmt,
    rc::Rc,
};

use futures::{
    StreamExt,
    future::{LocalBoxFuture, ready},
    stream::LocalBoxStream,
};
use lenso::prelude::*;
use lenso_capability_agent_model::{
    self as model_contract, CAPABILITY_ID, CompleteError, CompleteMessage, CompleteMessageInput,
    CompleteMessageKind, CompleteMessageRole, CompleteOpen, ModelInvocationError, ModelProvider,
    ProviderFailurePayload,
};
use lenso_capability_secrets::{self as secrets_contract, ResolveRequest};
use lenso_kernel::{InvocationContext, NativeStreamItem, NativeStreamSession, RuntimeFailure};

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenAiConfig {
    base_url: String,
    model: String,
    api_key_ref: String,
}

impl OpenAiConfig {
    fn validate(self) -> Result<Self, RuntimeFailure> {
        if self.model.trim().is_empty() || self.api_key_ref.trim().is_empty() {
            return Err(invalid_plan(
                "OpenAI-compatible model and api_key_ref must not be empty",
            ));
        }
        let endpoint = self.endpoint()?;
        if !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(invalid_plan(
                "OpenAI-compatible base_url must not contain credentials, query, or fragment",
            ));
        }
        let secure = endpoint.scheme() == "https";
        let loopback = endpoint.scheme() == "http"
            && endpoint
                .host_str()
                .is_some_and(|host| matches!(host, "127.0.0.1" | "localhost" | "::1"));
        if !secure && !loopback {
            return Err(invalid_plan(
                "OpenAI-compatible base_url must use HTTPS or loopback HTTP",
            ));
        }
        Ok(self)
    }

    fn endpoint(&self) -> Result<reqwest::Url, RuntimeFailure> {
        reqwest::Url::parse(&format!(
            "{}/chat/completions",
            self.base_url.trim_end_matches('/')
        ))
        .map_err(|_| invalid_plan("OpenAI-compatible base_url is invalid"))
    }
}

fn validate_config(config: &OpenAiConfig) -> Result<(), RuntimeFailure> {
    config.clone().validate().map(|_| ())
}

#[lenso::module(configuration_schema = "config.schema.json", validate = validate_config)]
#[derive(Clone, Debug)]
struct OpenAiCompatibleModel {
    #[config]
    config: OpenAiConfig,
    client: reqwest::Client,
    secrets: Port<secrets_contract::SecretsClient>,
}

#[lenso::provides(model_contract::Model)]
impl ModelProvider for OpenAiCompatibleModel {
    fn complete(
        &self,
        context: InvocationContext,
        request: CompleteOpen,
    ) -> LocalBoxFuture<'static, Result<Box<dyn NativeStreamSession>, ModelInvocationError>> {
        if request.model != self.config.model {
            return Box::pin(ready(Err(ModelInvocationError::Domain(
                CompleteError::UnsupportedModel,
            ))));
        }
        let body = match chat_request(&request) {
            Ok(body) => body,
            Err(error) => return Box::pin(ready(Err(ModelInvocationError::Domain(error)))),
        };
        let secrets = self.secrets.clone();
        let client = self.client.clone();
        let config = self.config.clone();
        Box::pin(async move {
            let credential = secrets
                .resolve_with_context(
                    context,
                    ResolveRequest {
                        reference: config.api_key_ref.clone(),
                    },
                )
                .await
                .map_err(|_| {
                    provider_failure(
                        "authentication_required",
                        "configured model credential is unavailable",
                        false,
                    )
                })?;
            let response = client
                .post(config.endpoint().map_err(ModelInvocationError::Runtime)?)
                .bearer_auth(credential.value)
                .json(&body)
                .send()
                .await
                .map_err(|_| {
                    provider_failure("transport_error", "model provider request failed", true)
                })?;
            if !response.status().is_success() {
                return Err(map_status(response.status()));
            }
            let chunks = response.bytes_stream().boxed_local();
            Ok(Box::new(OpenAiStream::new(chunks)) as Box<dyn NativeStreamSession>)
        })
    }
}

fn chat_request(request: &CompleteOpen) -> Result<serde_json::Value, CompleteError> {
    if request.max_output_tokens <= 0 {
        return Err(CompleteError::InvalidRequest);
    }
    let messages = request
        .messages
        .iter()
        .map(chat_message)
        .collect::<Result<Vec<_>, _>>()?;
    let tools = request
        .tools
        .iter()
        .map(|tool| {
            let parameters =
                serde_json::from_str::<serde_json::Value>(tool.input_schema_json.as_str())
                    .map_err(|_| CompleteError::InvalidRequest)?;
            Ok(serde_json::json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": parameters
                }
            }))
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(serde_json::json!({
        "model": request.model,
        "messages": messages,
        "tools": tools,
        "parallel_tool_calls": false,
        "temperature": request.temperature,
        "max_tokens": request.max_output_tokens,
        "stream": true,
        "stream_options": { "include_usage": true }
    }))
}

fn chat_message(message: &CompleteMessageInput) -> Result<serde_json::Value, CompleteError> {
    match message.role {
        CompleteMessageRole::System | CompleteMessageRole::User => {
            if message.tool_call_id.is_some()
                || message.tool_name.is_some()
                || message.arguments_json.is_some()
            {
                return Err(CompleteError::InvalidRequest);
            }
            let role = if message.role == CompleteMessageRole::System {
                "system"
            } else {
                "user"
            };
            Ok(serde_json::json!({"role": role, "content": message.content}))
        }
        CompleteMessageRole::Assistant => match (
            message.tool_call_id.as_deref(),
            message.tool_name.as_deref(),
            message.arguments_json.as_deref(),
        ) {
            (None, None, None) => {
                Ok(serde_json::json!({"role": "assistant", "content": message.content}))
            }
            (Some(id), Some(name), Some(arguments))
                if !id.is_empty()
                    && !name.is_empty()
                    && serde_json::from_str::<serde_json::Value>(arguments).is_ok() =>
            {
                Ok(serde_json::json!({
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": id,
                        "type": "function",
                        "function": {"name": name, "arguments": arguments}
                    }]
                }))
            }
            _ => Err(CompleteError::InvalidRequest),
        },
        CompleteMessageRole::Tool => {
            let Some(id) = message.tool_call_id.as_deref().filter(|id| !id.is_empty()) else {
                return Err(CompleteError::InvalidRequest);
            };
            if message.tool_name.is_some() || message.arguments_json.is_some() {
                return Err(CompleteError::InvalidRequest);
            }
            Ok(serde_json::json!({
                "role": "tool",
                "tool_call_id": id,
                "content": message.content
            }))
        }
    }
}

fn map_status(status: reqwest::StatusCode) -> ModelInvocationError {
    match status {
        reqwest::StatusCode::BAD_REQUEST | reqwest::StatusCode::UNPROCESSABLE_ENTITY => {
            ModelInvocationError::Domain(CompleteError::InvalidRequest)
        }
        reqwest::StatusCode::NOT_FOUND => {
            ModelInvocationError::Domain(CompleteError::UnsupportedModel)
        }
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => provider_failure(
            "credential_rejected",
            "model provider rejected its configured credential",
            false,
        ),
        reqwest::StatusCode::TOO_MANY_REQUESTS => provider_failure(
            "rate_limited",
            "model provider rate limit was exceeded",
            true,
        ),
        _ => provider_failure(
            "provider_error",
            "model provider returned an unsuccessful status",
            status.is_server_error(),
        ),
    }
}

fn provider_failure(reason_code: &str, message: &str, retryable: bool) -> ModelInvocationError {
    ModelInvocationError::Domain(CompleteError::ProviderFailure {
        payload: ProviderFailurePayload {
            message: message.to_owned(),
            reason_code: reason_code.to_owned(),
            retryable,
        },
    })
}

fn invalid_plan(detail: impl Into<String>) -> RuntimeFailure {
    RuntimeFailure::InvalidResolvedPlan {
        detail: detail.into(),
    }
}

type ProviderChunks = LocalBoxStream<'static, Result<bytes::Bytes, reqwest::Error>>;

struct OpenAiStream {
    chunks: Rc<futures::lock::Mutex<ProviderChunks>>,
    decoder: Rc<RefCell<SseDecoder>>,
    events: Rc<RefCell<VecDeque<NativeStreamItem>>>,
    cancelled: Rc<Cell<bool>>,
    send_closed: Rc<Cell<bool>>,
}

impl fmt::Debug for OpenAiStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenAiStream")
            .field("cancelled", &self.cancelled.get())
            .field("send_closed", &self.send_closed.get())
            .finish_non_exhaustive()
    }
}

impl OpenAiStream {
    fn new(chunks: ProviderChunks) -> Self {
        Self {
            chunks: Rc::new(futures::lock::Mutex::new(chunks)),
            decoder: Rc::new(RefCell::new(SseDecoder::default())),
            events: Rc::new(RefCell::new(VecDeque::new())),
            cancelled: Rc::new(Cell::new(false)),
            send_closed: Rc::new(Cell::new(false)),
        }
    }
}

impl NativeStreamSession for OpenAiStream {
    fn send(&self, _message: Box<dyn Any>) -> LocalBoxFuture<'static, Result<(), RuntimeFailure>> {
        Box::pin(ready(Err(RuntimeFailure::ProtocolViolation {
            capability: CAPABILITY_ID,
        })))
    }

    fn receive(&self) -> LocalBoxFuture<'static, Result<NativeStreamItem, RuntimeFailure>> {
        let chunks = self.chunks.clone();
        let decoder = self.decoder.clone();
        let events = self.events.clone();
        let cancelled = self.cancelled.clone();
        Box::pin(async move {
            loop {
                if cancelled.get() {
                    return Err(RuntimeFailure::AdmissionClosed);
                }
                if let Some(event) = events.borrow_mut().pop_front() {
                    return Ok(event);
                }
                if decoder.borrow().terminal {
                    return Err(RuntimeFailure::ProtocolViolation {
                        capability: CAPABILITY_ID,
                    });
                }
                let chunk = chunks.lock().await.next().await;
                let output_events = match chunk {
                    Some(Ok(bytes)) => decoder.borrow_mut().push(&bytes)?,
                    Some(Err(_)) => {
                        return Err(RuntimeFailure::ModuleFailure {
                            detail: "model provider stream failed".to_owned(),
                        });
                    }
                    None => decoder.borrow_mut().finish()?,
                };
                events.borrow_mut().extend(output_events);
            }
        })
    }

    fn close_send(&self) -> LocalBoxFuture<'static, Result<(), RuntimeFailure>> {
        let result = if self.send_closed.replace(true) {
            Err(RuntimeFailure::ProtocolViolation {
                capability: CAPABILITY_ID,
            })
        } else {
            Ok(())
        };
        Box::pin(ready(result))
    }

    fn cancel(&self) {
        self.cancelled.set(true);
        self.events.borrow_mut().clear();
    }
}

#[derive(Debug, Default)]
struct SseDecoder {
    buffer: Vec<u8>,
    tool_calls: BTreeMap<u64, ToolCallAccumulator>,
    sequence: u64,
    terminal: bool,
}

impl SseDecoder {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<NativeStreamItem>, RuntimeFailure> {
        self.buffer.extend_from_slice(bytes);
        let mut output = Vec::new();
        while let Some((end, delimiter)) = frame_boundary(&self.buffer) {
            let frame = self.buffer.drain(..end).collect::<Vec<_>>();
            self.buffer.drain(..delimiter);
            self.decode_frame(&frame, &mut output)?;
        }
        Ok(output)
    }

    fn finish(&mut self) -> Result<Vec<NativeStreamItem>, RuntimeFailure> {
        let mut output = Vec::new();
        if !self.buffer.iter().all(u8::is_ascii_whitespace) {
            let frame = std::mem::take(&mut self.buffer);
            self.decode_frame(&frame, &mut output)?;
        }
        if !self.terminal {
            self.finish_success(&mut output)?;
        }
        Ok(output)
    }

    fn decode_frame(
        &mut self,
        frame: &[u8],
        output: &mut Vec<NativeStreamItem>,
    ) -> Result<(), RuntimeFailure> {
        let frame = std::str::from_utf8(frame).map_err(|_| provider_protocol_failure())?;
        let data = frame
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() {
            return Ok(());
        }
        if data == "[DONE]" {
            if !self.terminal {
                self.finish_success(output)?;
            }
            return Ok(());
        }
        let chunk =
            serde_json::from_str::<ChatChunk>(&data).map_err(|_| provider_protocol_failure())?;
        for choice in chunk.choices {
            if let Some(content) = choice.delta.content.filter(|value| !value.is_empty()) {
                output.push(self.message(
                    CompleteMessageKind::TextDelta,
                    content,
                    "",
                    "",
                    "{}",
                    0,
                    0,
                ));
            }
            for tool_call in choice.delta.tool_calls {
                let accumulator = self.tool_calls.entry(tool_call.index).or_default();
                if let Some(id) = tool_call.id {
                    accumulator.id.push_str(&id);
                }
                if let Some(function) = tool_call.function {
                    if let Some(name) = function.name {
                        accumulator.name.push_str(&name);
                    }
                    if let Some(arguments) = function.arguments {
                        accumulator.arguments.push_str(&arguments);
                    }
                }
            }
            if choice.finish_reason.as_deref() == Some("tool_calls") {
                self.flush_tool_calls(output)?;
            } else if choice.finish_reason.as_deref() == Some("content_filter") {
                self.tool_calls.clear();
                self.terminal = true;
                output.push(NativeStreamItem::PeerHalfClosed);
                output.push(NativeStreamItem::Terminal(Err(Box::new(
                    CompleteError::ContentRejected,
                ))));
                return Ok(());
            }
        }
        if let Some(usage) = chunk.usage {
            output.push(self.message(
                CompleteMessageKind::Usage,
                "",
                "",
                "",
                "{}",
                usage.prompt_tokens,
                usage.completion_tokens,
            ));
        }
        Ok(())
    }

    fn finish_success(&mut self, output: &mut Vec<NativeStreamItem>) -> Result<(), RuntimeFailure> {
        self.flush_tool_calls(output)?;
        self.terminal = true;
        output.push(NativeStreamItem::PeerHalfClosed);
        output.push(NativeStreamItem::Terminal(Ok(())));
        Ok(())
    }

    fn flush_tool_calls(
        &mut self,
        output: &mut Vec<NativeStreamItem>,
    ) -> Result<(), RuntimeFailure> {
        let calls = std::mem::take(&mut self.tool_calls);
        for call in calls.into_values() {
            if call.id.is_empty()
                || call.name.is_empty()
                || serde_json::from_str::<serde_json::Value>(&call.arguments).is_err()
            {
                return Err(provider_protocol_failure());
            }
            output.push(self.message(
                CompleteMessageKind::ToolCall,
                "",
                &call.id,
                &call.name,
                &call.arguments,
                0,
                0,
            ));
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn message(
        &mut self,
        kind: CompleteMessageKind,
        text: impl Into<String>,
        tool_call_id: &str,
        tool_name: &str,
        arguments_json: &str,
        input_tokens: u64,
        output_tokens: u64,
    ) -> NativeStreamItem {
        self.sequence = self.sequence.saturating_add(1);
        NativeStreamItem::Message(Box::new(CompleteMessage {
            sequence: self.sequence.to_string(),
            kind,
            text: text.into(),
            tool_call_id: tool_call_id.to_owned(),
            tool_name: tool_name.to_owned(),
            arguments_json: arguments_json
                .to_owned()
                .try_into()
                .expect("provider Tool arguments must be valid JSON"),
            input_tokens: input_tokens.to_string(),
            output_tokens: output_tokens.to_string(),
        }))
    }
}

fn frame_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|position| (position, 2))
        .or_else(|| {
            buffer
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|position| (position, 4))
        })
}

fn provider_protocol_failure() -> RuntimeFailure {
    RuntimeFailure::ModuleFailure {
        detail: "model provider returned an invalid event stream".to_owned(),
    }
}

#[derive(Debug, serde::Deserialize)]
struct ChatChunk {
    #[serde(default)]
    choices: Vec<ChatChoice>,
    usage: Option<ChatUsage>,
}

#[derive(Debug, serde::Deserialize)]
struct ChatChoice {
    #[serde(default)]
    delta: ChatDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
struct ChatDelta {
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolCallDelta>,
}

#[derive(Debug, serde::Deserialize)]
struct ToolCallDelta {
    index: u64,
    id: Option<String>,
    function: Option<FunctionDelta>,
}

#[derive(Debug, serde::Deserialize)]
struct FunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct ChatUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
}

#[derive(Debug, Default)]
struct ToolCallAccumulator {
    id: String,
    name: String,
    arguments: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use lenso_capability_agent_model::{CompleteMessageInput, CompleteMessageRole, CompleteTool};

    #[test]
    fn chat_request_preserves_assistant_tool_call() {
        let request = CompleteOpen {
            model: "test-model".to_owned(),
            messages: vec![
                CompleteMessageInput {
                    role: CompleteMessageRole::Assistant,
                    content: String::new(),
                    tool_call_id: Some("call-1".to_owned()),
                    tool_name: Some("read".to_owned()),
                    arguments_json: Some(r#"{"path":"README.md"}"#.to_owned().try_into().unwrap()),
                },
                CompleteMessageInput {
                    role: CompleteMessageRole::Tool,
                    content: "# Fixture".to_owned(),
                    tool_call_id: Some("call-1".to_owned()),
                    tool_name: None,
                    arguments_json: None,
                },
            ],
            tools: vec![CompleteTool {
                name: "read".to_owned(),
                description: "Read a file".to_owned(),
                input_schema_json: r#"{"type":"object"}"#.to_owned().try_into().unwrap(),
            }],
            temperature: 0.0,
            max_output_tokens: 128,
        };
        let body = chat_request(&request).unwrap();
        assert_eq!(body["messages"][0]["tool_calls"][0]["id"], "call-1");
        assert_eq!(
            body["messages"][0]["tool_calls"][0]["function"]["name"],
            "read"
        );
        assert_eq!(body["messages"][1]["tool_call_id"], "call-1");
        assert_eq!(body["parallel_tool_calls"], false);
    }

    #[test]
    fn decoder_assembles_fragmented_tool_call_and_usage() {
        let mut decoder = SseDecoder::default();
        let first = br#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-1","function":{"name":"read","arguments":"{\"path\":"}}]},"finish_reason":null}]}

"#;
        let second = br#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"README.md\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":12,"completion_tokens":4}}

data: [DONE]

"#;
        assert!(decoder.push(&first[..23]).unwrap().is_empty());
        assert!(decoder.push(&first[23..]).unwrap().is_empty());
        let events = decoder.push(second).unwrap();
        let messages = events
            .into_iter()
            .filter_map(|event| match event {
                NativeStreamItem::Message(value) => value.downcast::<CompleteMessage>().ok(),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].kind, CompleteMessageKind::ToolCall);
        assert_eq!(
            messages[0].arguments_json.as_str(),
            r#"{"path":"README.md"}"#
        );
        assert_eq!(messages[1].kind, CompleteMessageKind::Usage);
        assert_eq!(messages[1].input_tokens, "12");
    }

    #[test]
    fn insecure_remote_base_url_is_rejected() {
        let error = OpenAiConfig {
            base_url: "http://example.com/v1".to_owned(),
            model: "test-model".to_owned(),
            api_key_ref: "model/api-key".to_owned(),
        }
        .validate()
        .unwrap_err();
        assert!(matches!(error, RuntimeFailure::InvalidResolvedPlan { .. }));
    }

    #[test]
    fn credential_bearing_base_url_is_rejected() {
        let error = OpenAiConfig {
            base_url: "https://secret@example.com/v1".to_owned(),
            model: "test-model".to_owned(),
            api_key_ref: "model/api-key".to_owned(),
        }
        .validate()
        .unwrap_err();
        assert!(matches!(error, RuntimeFailure::InvalidResolvedPlan { .. }));
    }

    #[test]
    fn content_filter_is_the_last_stream_outcome() {
        let mut decoder = SseDecoder::default();
        let events = decoder
            .push(
                br#"data: {"choices":[{"delta":{},"finish_reason":"content_filter"}],"usage":{"prompt_tokens":1,"completion_tokens":0}}

"#,
            )
            .unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], NativeStreamItem::PeerHalfClosed));
        assert!(matches!(events[1], NativeStreamItem::Terminal(Err(_))));
    }

    #[test]
    fn cancellation_stops_provider_delivery() {
        let chunks =
            futures::stream::pending::<Result<bytes::Bytes, reqwest::Error>>().boxed_local();
        let stream = OpenAiStream::new(chunks);
        stream.cancel();
        let error = futures::executor::block_on(stream.receive()).unwrap_err();
        assert_eq!(error, RuntimeFailure::AdmissionClosed);
    }
}
