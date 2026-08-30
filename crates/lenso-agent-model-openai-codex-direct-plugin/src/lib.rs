//! Experimental direct `ChatGPT` subscription Model Plugin.

use std::{
    any::Any,
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt,
    rc::Rc,
};

use futures::{
    StreamExt,
    future::{LocalBoxFuture, ready},
    stream::LocalBoxStream,
};
use lenso::prelude::*;
use lenso_capability_agent_auth_openai_codex::{
    self as auth_contract, AccessRequest, OpenaiCodexInvocationError,
};
use lenso_capability_agent_model::{
    self as model_contract, CAPABILITY_ID, CompleteError, CompleteMessage, CompleteMessageInput,
    CompleteMessageKind, CompleteMessageRole, CompleteOpen, ModelInvocationError, ModelProvider,
    ProviderFailurePayload,
};
use lenso_kernel::{InvocationContext, NativeStreamItem, NativeStreamSession, RuntimeFailure};

const DEFAULT_BASE_URL: &str = "https://chatgpt.com/backend-api";
const MAX_EVENT_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectModelConfig {
    base_url: String,
    model: String,
    #[serde(default)]
    allowed_models: Vec<String>,
    reasoning_effort: String,
    max_event_bytes: usize,
}

impl DirectModelConfig {
    fn validate(self) -> Result<Self, RuntimeFailure> {
        if !valid_model_id(&self.model)
            || self.allowed_models.len() > 16
            || self
                .allowed_models
                .iter()
                .any(|model| !valid_model_id(model) || model == &self.model)
            || self.allowed_models.iter().collect::<BTreeSet<_>>().len()
                != self.allowed_models.len()
            || !matches!(
                self.reasoning_effort.as_str(),
                "low" | "medium" | "high" | "xhigh" | "max" | "ultra"
            )
            || self.max_event_bytes == 0
            || self.max_event_bytes > MAX_EVENT_BYTES
        {
            return Err(invalid_plan(
                "direct Codex model and max_event_bytes are invalid",
            ));
        }
        let endpoint = self.endpoint()?;
        let official = self.base_url.trim_end_matches('/') == DEFAULT_BASE_URL;
        let loopback = endpoint.scheme() == "http"
            && endpoint
                .host_str()
                .is_some_and(|host| matches!(host, "127.0.0.1" | "localhost" | "::1"));
        if (!official && !loopback)
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(invalid_plan(
                "direct Codex base_url must be chatgpt.com/backend-api or loopback HTTP",
            ));
        }
        Ok(self)
    }

    fn endpoint(&self) -> Result<reqwest::Url, RuntimeFailure> {
        reqwest::Url::parse(&format!(
            "{}/codex/responses",
            self.base_url.trim_end_matches('/')
        ))
        .map_err(|_| invalid_plan("direct Codex base_url is invalid"))
    }

    fn admits_model(&self, model: &str) -> bool {
        model == self.model || self.allowed_models.iter().any(|allowed| allowed == model)
    }
}

fn valid_model_id(model: &str) -> bool {
    model.trim() == model && !model.is_empty() && model.len() <= 256
}

fn validate_config(config: &DirectModelConfig) -> Result<(), RuntimeFailure> {
    config.clone().validate().map(|_| ())
}

#[lenso::plugin(configuration_schema = "config.schema.json", validate = validate_config)]
#[derive(Clone, Debug)]
struct DirectModel {
    #[config]
    config: DirectModelConfig,
    client: reqwest::Client,
    auth: Port<auth_contract::OpenaiCodexClient>,
}

#[lenso::provides(model_contract::Model)]
impl ModelProvider for DirectModel {
    fn complete(
        &self,
        context: InvocationContext,
        request: CompleteOpen,
    ) -> LocalBoxFuture<'static, Result<Box<dyn NativeStreamSession>, ModelInvocationError>> {
        if !self.config.admits_model(&request.model) {
            return Box::pin(ready(Err(ModelInvocationError::Domain(
                CompleteError::UnsupportedModel,
            ))));
        }
        let reasoning_effort = request
            .reasoning_effort
            .as_deref()
            .unwrap_or(&self.config.reasoning_effort);
        let wire_request = match responses_request(&request, reasoning_effort) {
            Ok(body) => body,
            Err(error) => return Box::pin(ready(Err(ModelInvocationError::Domain(error)))),
        };
        let auth = self.auth.clone();
        let config = self.config.clone();
        let client = self.client.clone();
        Box::pin(async move {
            let credential = auth
                .access_with_context(context, AccessRequest {})
                .await
                .map_err(map_auth_error)?;
            let response = client
                .post(config.endpoint().map_err(ModelInvocationError::Runtime)?)
                .bearer_auth(credential.access_token)
                .header("chatgpt-account-id", credential.account_id)
                .header("originator", "lenso")
                .header("User-Agent", "lenso-agent/0.1.0")
                .header("OpenAI-Beta", "responses=experimental")
                .header("Accept", "text/event-stream")
                .json(&wire_request.body)
                .send()
                .await
                .map_err(|_| {
                    provider_failure("transport_error", "direct Codex request failed", true)
                })?;
            if !response.status().is_success() {
                return Err(map_status(response.status()));
            }
            let chunks = response.bytes_stream().boxed_local();
            Ok(Box::new(DirectCodexStream::new(
                chunks,
                config.max_event_bytes,
                wire_request.provider_to_lenso_tool_names,
            )) as Box<dyn NativeStreamSession>)
        })
    }
}

struct ResponsesRequest {
    body: serde_json::Value,
    provider_to_lenso_tool_names: BTreeMap<String, String>,
}

fn responses_request(
    request: &CompleteOpen,
    reasoning_effort: &str,
) -> Result<ResponsesRequest, CompleteError> {
    if request.max_output_tokens <= 0 || !request.temperature.is_finite() {
        return Err(CompleteError::InvalidRequest);
    }
    let mut lenso_to_provider_tool_names = BTreeMap::new();
    let mut provider_to_lenso_tool_names = BTreeMap::new();
    for tool in &request.tools {
        let provider_name = provider_tool_name(&tool.name)?;
        if provider_to_lenso_tool_names
            .insert(provider_name.clone(), tool.name.clone())
            .is_some()
        {
            return Err(CompleteError::InvalidRequest);
        }
        lenso_to_provider_tool_names.insert(tool.name.clone(), provider_name);
    }
    let instructions = request
        .messages
        .iter()
        .filter(|message| message.role == CompleteMessageRole::System)
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let input = request
        .messages
        .iter()
        .filter(|message| message.role != CompleteMessageRole::System)
        .map(|message| responses_message(message, &lenso_to_provider_tool_names))
        .collect::<Result<Vec<_>, _>>()?;
    let tools = request
        .tools
        .iter()
        .map(|tool| {
            let parameters = serde_json::from_str::<serde_json::Value>(tool.input_schema_json.as_str())
                .map_err(|_| CompleteError::InvalidRequest)?;
            Ok(serde_json::json!({
                "type": "function",
                "name": lenso_to_provider_tool_names.get(&tool.name).ok_or(CompleteError::InvalidRequest)?,
                "description": tool.description,
                "parameters": parameters,
                "strict": false
            }))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut body = serde_json::json!({
        "model": request.model,
        "store": false,
        "stream": true,
        "instructions": if instructions.is_empty() { "You are a helpful assistant." } else { &instructions },
        "input": input,
        "tools": tools,
        "tool_choice": "auto",
        "parallel_tool_calls": true,
        "include": ["reasoning.encrypted_content"],
        "reasoning": { "effort": reasoning_effort, "summary": "auto" },
        "text": { "verbosity": "low" },
        "max_output_tokens": request.max_output_tokens,
    });
    if request.temperature != 0.0 {
        body["temperature"] = serde_json::json!(request.temperature);
    }
    if let Some(service_tier) = request.service_tier.as_deref() {
        if service_tier != "fast" {
            return Err(CompleteError::InvalidRequest);
        }
        body["service_tier"] = serde_json::json!(service_tier);
    }
    Ok(ResponsesRequest {
        body,
        provider_to_lenso_tool_names,
    })
}

fn provider_tool_name(name: &str) -> Result<String, CompleteError> {
    if name.is_empty() {
        return Err(CompleteError::InvalidRequest);
    }
    let name = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .take(128)
        .collect::<String>();
    (!name.is_empty())
        .then_some(name)
        .ok_or(CompleteError::InvalidRequest)
}

fn responses_message(
    message: &CompleteMessageInput,
    lenso_to_provider_tool_names: &BTreeMap<String, String>,
) -> Result<serde_json::Value, CompleteError> {
    match message.role {
        CompleteMessageRole::User => {
            require_no_tool_fields(message)?;
            Ok(serde_json::json!({
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": message.content }]
            }))
        }
        CompleteMessageRole::Assistant => match (
            message.tool_call_id.as_deref(),
            message.tool_name.as_deref(),
            message.arguments_json.as_deref(),
        ) {
            (None, None, None) => Ok(serde_json::json!({
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": message.content }]
            })),
            (Some(call_id), Some(name), Some(arguments))
                if !call_id.is_empty()
                    && !name.is_empty()
                    && serde_json::from_str::<serde_json::Value>(arguments).is_ok() =>
            {
                let name = lenso_to_provider_tool_names
                    .get(name)
                    .ok_or(CompleteError::InvalidRequest)?;
                Ok(serde_json::json!({
                    "type": "function_call",
                    "call_id": call_id,
                    "name": name,
                    "arguments": arguments
                }))
            }
            _ => Err(CompleteError::InvalidRequest),
        },
        CompleteMessageRole::Tool => {
            let Some(call_id) = message.tool_call_id.as_deref().filter(|id| !id.is_empty()) else {
                return Err(CompleteError::InvalidRequest);
            };
            if message.tool_name.is_some() || message.arguments_json.is_some() {
                return Err(CompleteError::InvalidRequest);
            }
            Ok(serde_json::json!({
                "type": "function_call_output",
                "call_id": call_id,
                "output": message.content
            }))
        }
        CompleteMessageRole::System => Err(CompleteError::InvalidRequest),
    }
}

fn require_no_tool_fields(message: &CompleteMessageInput) -> Result<(), CompleteError> {
    if message.tool_call_id.is_some()
        || message.tool_name.is_some()
        || message.arguments_json.is_some()
    {
        Err(CompleteError::InvalidRequest)
    } else {
        Ok(())
    }
}

fn map_auth_error(_error: OpenaiCodexInvocationError) -> ModelInvocationError {
    provider_failure(
        "authentication_required",
        "direct Codex authentication failed; run direct login",
        false,
    )
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
            "direct Codex credential was rejected",
            false,
        ),
        reqwest::StatusCode::TOO_MANY_REQUESTS => {
            ModelInvocationError::Domain(CompleteError::RateLimited)
        }
        reqwest::StatusCode::PAYLOAD_TOO_LARGE => {
            ModelInvocationError::Domain(CompleteError::ContextOverflow)
        }
        reqwest::StatusCode::SERVICE_UNAVAILABLE => {
            ModelInvocationError::Domain(CompleteError::Overloaded)
        }
        _ => provider_failure(
            "provider_error",
            "direct Codex provider returned an unsuccessful status",
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

struct DirectCodexStream {
    chunks: Rc<futures::lock::Mutex<ProviderChunks>>,
    decoder: Rc<RefCell<ResponsesDecoder>>,
    events: Rc<RefCell<VecDeque<NativeStreamItem>>>,
    cancelled: Rc<Cell<bool>>,
    send_closed: Rc<Cell<bool>>,
}

impl fmt::Debug for DirectCodexStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DirectCodexStream")
            .field("cancelled", &self.cancelled.get())
            .field("send_closed", &self.send_closed.get())
            .finish_non_exhaustive()
    }
}

impl DirectCodexStream {
    fn new(
        chunks: ProviderChunks,
        max_event_bytes: usize,
        provider_to_lenso_tool_names: BTreeMap<String, String>,
    ) -> Self {
        Self {
            chunks: Rc::new(futures::lock::Mutex::new(chunks)),
            decoder: Rc::new(RefCell::new(ResponsesDecoder::new(
                max_event_bytes,
                provider_to_lenso_tool_names,
            ))),
            events: Rc::new(RefCell::new(VecDeque::new())),
            cancelled: Rc::new(Cell::new(false)),
            send_closed: Rc::new(Cell::new(false)),
        }
    }
}

impl NativeStreamSession for DirectCodexStream {
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
                let output = match chunk {
                    Some(Ok(bytes)) => decoder.borrow_mut().push(&bytes)?,
                    Some(Err(_)) => return Err(protocol_failure("direct Codex stream failed")),
                    None => decoder.borrow_mut().finish()?,
                };
                events.borrow_mut().extend(output);
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

#[derive(Debug)]
struct ResponsesDecoder {
    buffer: Vec<u8>,
    sequence: u64,
    terminal: bool,
    max_event_bytes: usize,
    provider_to_lenso_tool_names: BTreeMap<String, String>,
}

impl ResponsesDecoder {
    fn new(max_event_bytes: usize, provider_to_lenso_tool_names: BTreeMap<String, String>) -> Self {
        Self {
            buffer: Vec::new(),
            sequence: 0,
            terminal: false,
            max_event_bytes,
            provider_to_lenso_tool_names,
        }
    }

    fn push(&mut self, bytes: &[u8]) -> Result<Vec<NativeStreamItem>, RuntimeFailure> {
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() > self.max_event_bytes && frame_boundary(&self.buffer).is_none() {
            return Err(protocol_failure("direct Codex event exceeded its bound"));
        }
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
            return Err(protocol_failure(
                "direct Codex stream ended without response.completed",
            ));
        }
        Ok(output)
    }

    fn decode_frame(
        &mut self,
        frame: &[u8],
        output: &mut Vec<NativeStreamItem>,
    ) -> Result<(), RuntimeFailure> {
        if frame.len() > self.max_event_bytes {
            return Err(protocol_failure("direct Codex event exceeded its bound"));
        }
        let frame = std::str::from_utf8(frame)
            .map_err(|_| protocol_failure("direct Codex emitted non-UTF-8 SSE"))?;
        let data = frame
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() || data == "[DONE]" {
            return Ok(());
        }
        let event = serde_json::from_str::<serde_json::Value>(&data)
            .map_err(|_| protocol_failure("direct Codex emitted invalid SSE JSON"))?;
        match event.get("type").and_then(serde_json::Value::as_str) {
            Some("response.reasoning_summary_text.delta") => {
                self.emit_text_delta(CompleteMessageKind::ReasoningSummaryDelta, &event, output);
            }
            Some("response.output_text.delta") => {
                self.emit_text_delta(CompleteMessageKind::TextDelta, &event, output);
            }
            Some("response.output_item.done") => {
                let item = event
                    .get("item")
                    .ok_or_else(|| protocol_failure("direct Codex omitted output item"))?;
                if item.get("type").and_then(serde_json::Value::as_str) == Some("function_call") {
                    let call_id = string_field(item, "call_id")?;
                    let provider_name = string_field(item, "name")?;
                    let name = self
                        .provider_to_lenso_tool_names
                        .get(provider_name)
                        .ok_or_else(|| protocol_failure("direct Codex returned an unknown Tool"))?
                        .clone();
                    let arguments = string_field(item, "arguments")?;
                    serde_json::from_str::<serde_json::Value>(arguments).map_err(|_| {
                        protocol_failure("direct Codex emitted invalid Tool arguments")
                    })?;
                    output.push(self.message(
                        CompleteMessageKind::ToolCall,
                        "",
                        call_id,
                        &name,
                        arguments,
                        0,
                        0,
                    ));
                }
            }
            Some("response.completed" | "response.done") => {
                let usage = event
                    .get("response")
                    .and_then(|response| response.get("usage"));
                if let Some(usage) = usage {
                    output.push(
                        self.message(
                            CompleteMessageKind::Usage,
                            "",
                            "",
                            "",
                            "{}",
                            usage
                                .get("input_tokens")
                                .and_then(serde_json::Value::as_u64)
                                .unwrap_or(0),
                            usage
                                .get("output_tokens")
                                .and_then(serde_json::Value::as_u64)
                                .unwrap_or(0),
                        ),
                    );
                }
                self.terminal = true;
                output.push(NativeStreamItem::PeerHalfClosed);
                output.push(NativeStreamItem::Terminal(Ok(())));
            }
            Some("error" | "response.failed" | "response.incomplete") => {
                return Err(protocol_failure("direct Codex response failed"));
            }
            _ => {}
        }
        Ok(())
    }

    fn emit_text_delta(
        &mut self,
        kind: CompleteMessageKind,
        event: &serde_json::Value,
        output: &mut Vec<NativeStreamItem>,
    ) {
        if let Some(delta) = event.get("delta").and_then(serde_json::Value::as_str)
            && !delta.is_empty()
        {
            output.push(self.message(kind, delta, "", "", "{}", 0, 0));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn message(
        &mut self,
        kind: CompleteMessageKind,
        text: &str,
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
            text: text.to_owned(),
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

fn string_field<'a>(value: &'a serde_json::Value, field: &str) -> Result<&'a str, RuntimeFailure> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| protocol_failure("direct Codex output item is incomplete"))
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

fn protocol_failure(detail: &str) -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: detail.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_auxiliary_model_is_admitted_without_changing_the_primary() {
        let config = DirectModelConfig {
            base_url: DEFAULT_BASE_URL.to_owned(),
            model: "main-model".to_owned(),
            allowed_models: vec!["presentation-model".to_owned()],
            reasoning_effort: "medium".to_owned(),
            max_event_bytes: MAX_EVENT_BYTES,
        }
        .validate()
        .unwrap();
        assert!(config.admits_model("main-model"));
        assert!(config.admits_model("presentation-model"));
        assert!(!config.admits_model("unreviewed-model"));
    }
    use lenso_capability_agent_model::CompleteTool;

    #[test]
    fn request_preserves_tool_call_and_result() {
        let request = CompleteOpen {
            model: "gpt-test".to_owned(),
            reasoning_effort: None,
            service_tier: None,
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
                    content: "fixture".to_owned(),
                    tool_call_id: Some("call-1".to_owned()),
                    tool_name: None,
                    arguments_json: None,
                },
            ],
            tools: vec![CompleteTool {
                name: "read".to_owned(),
                description: "Read text".to_owned(),
                input_schema_json: r#"{"type":"object"}"#.to_owned().try_into().unwrap(),
            }],
            temperature: 0.0,
            max_output_tokens: 128,
        };
        let wire_request = responses_request(&request, "medium").unwrap();
        let body = wire_request.body;
        assert_eq!(body["input"][0]["type"], "function_call");
        assert_eq!(body["input"][0]["name"], "read");
        assert_eq!(body["input"][1]["type"], "function_call_output");
        assert_eq!(body["tools"][0]["name"], "read");
        assert_eq!(body["parallel_tool_calls"], true);
        assert_eq!(body["reasoning"]["effort"], "medium");
        assert!(body.get("temperature").is_none());
        assert_eq!(body["max_output_tokens"], 128);
        assert_eq!(
            wire_request.provider_to_lenso_tool_names.get("read"),
            Some(&"read".to_owned())
        );
    }

    #[test]
    fn request_rejects_provider_tool_name_collisions() {
        let request = CompleteOpen {
            model: "gpt-test".to_owned(),
            reasoning_effort: None,
            service_tier: None,
            messages: Vec::new(),
            tools: vec![
                CompleteTool {
                    name: "workspace.read".to_owned(),
                    description: "Read text".to_owned(),
                    input_schema_json: r#"{"type":"object"}"#.to_owned().try_into().unwrap(),
                },
                CompleteTool {
                    name: "workspace_read".to_owned(),
                    description: "Read other text".to_owned(),
                    input_schema_json: r#"{"type":"object"}"#.to_owned().try_into().unwrap(),
                },
            ],
            temperature: 0.0,
            max_output_tokens: 128,
        };
        assert!(matches!(
            responses_request(&request, "medium"),
            Err(CompleteError::InvalidRequest)
        ));
    }

    #[test]
    fn decoder_streams_reasoning_text_tool_call_and_usage() {
        let mut decoder = ResponsesDecoder::new(
            4096,
            BTreeMap::from([("read".to_owned(), "read".to_owned())]),
        );
        let frames = concat!(
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"Checking the workspace.\"}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"call_id\":\"call-1\",\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\"}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":3}}}\n\n"
        );
        let events = decoder.push(frames.as_bytes()).unwrap();
        assert_eq!(events.len(), 6);
        let first = match &events[0] {
            NativeStreamItem::Message(message) => message
                .downcast_ref::<CompleteMessage>()
                .expect("reasoning message must keep its generated type"),
            other => panic!("expected reasoning message, got {other:?}"),
        };
        assert_eq!(first.kind, CompleteMessageKind::ReasoningSummaryDelta);
        assert_eq!(first.text, "Checking the workspace.");
        assert!(decoder.terminal);
    }
}
