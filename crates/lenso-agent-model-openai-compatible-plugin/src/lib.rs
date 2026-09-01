//! OpenAI-compatible Chat Completions Model Plugin.

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
use lenso_capability_agent_model::{
    self as model_contract, CAPABILITY_ID, CatalogControl, CatalogControlStatus,
    CatalogInputModality, CatalogModel, CatalogModelLimits, CatalogRequest, CatalogResponse,
    CatalogWireProtocol, CompleteError, CompleteMessage, CompleteMessageInput, CompleteMessageKind,
    CompleteMessageRole, CompleteOpen, ModelCatalog, ModelCompleteInvocationError, ModelProvider,
    ProviderFailurePayload,
};
use lenso_capability_secrets::{self as secrets_contract, ResolveRequest};
use lenso_kernel::{InvocationContext, NativeStreamItem, NativeStreamSession, RuntimeFailure};

const MAX_EVENT_BYTES: usize = 1024 * 1024;
const MAX_TOOL_CALLS: usize = 128;
const MAX_TOOL_CALL_ID_BYTES: usize = 1024;
const MAX_TOOL_NAME_BYTES: usize = 256;
const MAX_TOOL_ARGUMENT_BYTES: usize = 1024 * 1024;
const MAX_TOOL_CALL_TOTAL_BYTES: usize = 4 * 1024 * 1024;
const MAX_PUSH_OUTPUT_ITEMS: usize = 1024;
const MAX_PUSH_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenAiConfig {
    base_url: String,
    model: String,
    #[serde(default)]
    allowed_models: Vec<String>,
    api_key_ref: String,
    #[serde(default)]
    http_referer: Option<String>,
    #[serde(default)]
    app_title: Option<String>,
}

impl OpenAiConfig {
    fn validate(self) -> Result<Self, RuntimeFailure> {
        if !valid_model_id(&self.model)
            || self.allowed_models.len() > 16
            || self
                .allowed_models
                .iter()
                .any(|model| !valid_model_id(model) || model == &self.model)
            || self.allowed_models.iter().collect::<BTreeSet<_>>().len()
                != self.allowed_models.len()
            || self.api_key_ref.trim().is_empty()
            || self
                .app_title
                .as_deref()
                .is_some_and(|title| title.trim().is_empty() || title.len() > 128)
        {
            return Err(invalid_plan(
                "OpenAI-compatible model allowlist and api_key_ref are invalid",
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
        if let Some(referer) = self.http_referer.as_deref() {
            let referer = reqwest::Url::parse(referer)
                .map_err(|_| invalid_plan("HTTP Referer must be an absolute HTTPS URL"))?;
            if referer.scheme() != "https" || referer.host_str().is_none() {
                return Err(invalid_plan("HTTP Referer must be an absolute HTTPS URL"));
            }
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

    fn admits_model(&self, model: &str) -> bool {
        model == self.model || self.allowed_models.iter().any(|allowed| allowed == model)
    }
}

fn valid_model_id(model: &str) -> bool {
    model.trim() == model && !model.is_empty() && model.len() <= 256
}

fn validate_config(config: &OpenAiConfig) -> Result<(), RuntimeFailure> {
    config.clone().validate().map(|_| ())
}

#[lenso::plugin(configuration_schema = "config.schema.json", validate = validate_config)]
#[derive(Clone, Debug)]
struct OpenAiCompatibleModel {
    #[config]
    config: OpenAiConfig,
    client: reqwest::Client,
    secrets: Port<secrets_contract::SecretsClient>,
}

#[lenso::provides(model_contract::Model)]
impl ModelProvider for OpenAiCompatibleModel {
    fn catalog(
        &self,
        _context: InvocationContext,
        _request: CatalogRequest,
    ) -> lenso_kernel::NativeRequestFuture<ModelCatalog> {
        let models = std::iter::once(self.config.model.as_str())
            .chain(self.config.allowed_models.iter().map(String::as_str))
            .map(|id| CatalogModel {
                id: id.to_owned(),
                display_name: id.to_owned(),
                description: "Configured OpenAI-compatible model".to_owned(),
                hidden: false,
                limits: CatalogModelLimits {
                    context_window_tokens: None,
                    max_input_tokens: None,
                    max_output_tokens: None,
                },
                input_modalities: vec![CatalogInputModality::Text],
                text_output: true,
                tool_calls: true,
                parallel_tool_calls: true,
                reasoning: unknown_control(),
                service_tiers: unknown_control(),
                wire_protocol: CatalogWireProtocol::OpenaiChatCompletions,
                compaction_compatibility: "generic-text-v1".to_owned(),
            })
            .collect();
        Box::pin(ready(Ok(Ok(CatalogResponse { models }))))
    }

    fn complete(
        &self,
        context: InvocationContext,
        request: CompleteOpen,
    ) -> LocalBoxFuture<'static, Result<Box<dyn NativeStreamSession>, ModelCompleteInvocationError>>
    {
        if !self.config.admits_model(&request.model) {
            return Box::pin(ready(Err(ModelCompleteInvocationError::Domain(
                CompleteError::UnsupportedModel,
            ))));
        }
        let body = match chat_request(&request) {
            Ok(body) => body,
            Err(error) => return Box::pin(ready(Err(ModelCompleteInvocationError::Domain(error)))),
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
            let mut request = client
                .post(
                    config
                        .endpoint()
                        .map_err(ModelCompleteInvocationError::Runtime)?,
                )
                .bearer_auth(credential.value);
            if let Some(referer) = config.http_referer.as_deref() {
                request = request.header("HTTP-Referer", referer);
            }
            if let Some(title) = config.app_title.as_deref() {
                request = request.header("X-Title", title);
            }
            let response = request.json(&body).send().await.map_err(|_| {
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

fn unknown_control() -> CatalogControl {
    CatalogControl {
        status: CatalogControlStatus::Unknown,
        mode: None,
        options: Vec::new(),
        default: None,
        budget_tokens: None,
    }
}

fn chat_request(request: &CompleteOpen) -> Result<serde_json::Value, CompleteError> {
    if request.max_output_tokens <= 0
        || request.reasoning_effort.is_some()
        || request.reasoning_enabled.is_some()
        || request.reasoning_budget_tokens.is_some()
        || request.service_tier.is_some()
    {
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
        "parallel_tool_calls": true,
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

fn map_status(status: reqwest::StatusCode) -> ModelCompleteInvocationError {
    match status {
        reqwest::StatusCode::BAD_REQUEST | reqwest::StatusCode::UNPROCESSABLE_ENTITY => {
            ModelCompleteInvocationError::Domain(CompleteError::InvalidRequest)
        }
        reqwest::StatusCode::NOT_FOUND => {
            ModelCompleteInvocationError::Domain(CompleteError::UnsupportedModel)
        }
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => provider_failure(
            "credential_rejected",
            "model provider rejected its configured credential",
            false,
        ),
        reqwest::StatusCode::TOO_MANY_REQUESTS => {
            ModelCompleteInvocationError::Domain(CompleteError::RateLimited)
        }
        reqwest::StatusCode::PAYLOAD_TOO_LARGE => {
            ModelCompleteInvocationError::Domain(CompleteError::ContextOverflow)
        }
        reqwest::StatusCode::SERVICE_UNAVAILABLE => {
            ModelCompleteInvocationError::Domain(CompleteError::Overloaded)
        }
        _ => provider_failure(
            "provider_error",
            "model provider returned an unsuccessful status",
            status.is_server_error(),
        ),
    }
}

fn provider_failure(
    reason_code: &str,
    message: &str,
    retryable: bool,
) -> ModelCompleteInvocationError {
    ModelCompleteInvocationError::Domain(CompleteError::ProviderFailure {
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
                        return Err(RuntimeFailure::PluginFailure {
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
    separator: Vec<u8>,
    tool_calls: BTreeMap<u64, ToolCallAccumulator>,
    tool_call_count: usize,
    tool_call_bytes: usize,
    tool_calls_finished: bool,
    sequence: u64,
    terminal: bool,
}

#[derive(Debug, Default)]
struct PushOutputBudget {
    items: usize,
    bytes: usize,
}

impl PushOutputBudget {
    fn reserve(&mut self, bytes: usize) -> Result<(), RuntimeFailure> {
        self.items = self
            .items
            .checked_add(1)
            .filter(|items| *items <= MAX_PUSH_OUTPUT_ITEMS)
            .ok_or_else(provider_protocol_failure)?;
        self.bytes = self
            .bytes
            .checked_add(bytes)
            .filter(|total| *total <= MAX_PUSH_OUTPUT_BYTES)
            .ok_or_else(provider_protocol_failure)?;
        Ok(())
    }
}

impl SseDecoder {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<NativeStreamItem>, RuntimeFailure> {
        let mut output = Vec::new();
        let mut budget = PushOutputBudget::default();
        for byte in bytes {
            self.push_byte(*byte, &mut output, &mut budget)?;
        }
        Ok(output)
    }

    fn finish(&mut self) -> Result<Vec<NativeStreamItem>, RuntimeFailure> {
        let mut output = Vec::new();
        let mut budget = PushOutputBudget::default();
        while !self.separator.is_empty() {
            let byte = self.separator.remove(0);
            self.push_content_byte(byte)?;
        }
        if !self.buffer.iter().all(u8::is_ascii_whitespace) {
            let frame = std::mem::take(&mut self.buffer);
            self.decode_frame(&frame, &mut output, &mut budget)?;
        }
        if !self.terminal {
            self.finish_success(&mut output, &mut budget)?;
        }
        Ok(output)
    }

    fn push_byte(
        &mut self,
        byte: u8,
        output: &mut Vec<NativeStreamItem>,
        budget: &mut PushOutputBudget,
    ) -> Result<(), RuntimeFailure> {
        self.separator.push(byte);
        loop {
            if matches!(self.separator.as_slice(), b"\n\n" | b"\r\n\r\n") {
                self.separator.clear();
                let frame = std::mem::take(&mut self.buffer);
                self.decode_frame(&frame, output, budget)?;
                return Ok(());
            }
            if [b"\n\n".as_slice(), b"\r\n\r\n".as_slice()]
                .iter()
                .any(|delimiter| delimiter.starts_with(&self.separator))
            {
                return Ok(());
            }
            let content = self.separator.remove(0);
            self.push_content_byte(content)?;
        }
    }

    fn push_content_byte(&mut self, byte: u8) -> Result<(), RuntimeFailure> {
        if self.buffer.len() >= MAX_EVENT_BYTES {
            return Err(provider_protocol_failure());
        }
        self.buffer.push(byte);
        Ok(())
    }

    fn decode_frame(
        &mut self,
        frame: &[u8],
        output: &mut Vec<NativeStreamItem>,
        budget: &mut PushOutputBudget,
    ) -> Result<(), RuntimeFailure> {
        if frame.len() > MAX_EVENT_BYTES {
            return Err(provider_protocol_failure());
        }
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
        if self.terminal {
            return Err(provider_protocol_failure());
        }
        if data == "[DONE]" {
            self.finish_success(output, budget)?;
            return Ok(());
        }
        let chunk =
            serde_json::from_str::<ChatChunk>(&data).map_err(|_| provider_protocol_failure())?;
        for choice in chunk.choices {
            self.decode_choice(choice, output, budget)?;
            if self.terminal {
                return Ok(());
            }
        }
        if let Some(usage) = chunk.usage {
            budget.reserve(
                usage.prompt_tokens.to_string().len() + usage.completion_tokens.to_string().len(),
            )?;
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

    fn decode_choice(
        &mut self,
        choice: ChatChoice,
        output: &mut Vec<NativeStreamItem>,
        budget: &mut PushOutputBudget,
    ) -> Result<(), RuntimeFailure> {
        if let Some(reasoning) = choice
            .delta
            .reasoning_content
            .filter(|value| !value.is_empty())
        {
            budget.reserve(reasoning.len())?;
            output.push(self.message(
                CompleteMessageKind::ReasoningSummaryDelta,
                reasoning,
                "",
                "",
                "{}",
                0,
                0,
            ));
        }
        if let Some(content) = choice.delta.content.filter(|value| !value.is_empty()) {
            budget.reserve(content.len())?;
            output.push(self.message(CompleteMessageKind::TextDelta, content, "", "", "{}", 0, 0));
        }
        for tool_call in choice.delta.tool_calls {
            self.accumulate_tool_call(tool_call)?;
        }
        match choice.finish_reason.as_deref() {
            Some("tool_calls") => {
                self.flush_tool_calls(output, budget)?;
                self.tool_calls_finished = true;
            }
            Some("content_filter") => {
                self.tool_calls.clear();
                self.terminal = true;
                budget.reserve(0)?;
                output.push(NativeStreamItem::PeerHalfClosed);
                budget.reserve(0)?;
                output.push(NativeStreamItem::Terminal(Err(Box::new(
                    CompleteError::ContentRejected,
                ))));
            }
            _ => {}
        }
        Ok(())
    }

    fn accumulate_tool_call(&mut self, tool_call: ToolCallDelta) -> Result<(), RuntimeFailure> {
        let ToolCallDelta {
            index,
            id,
            function,
        } = tool_call;
        if self.tool_calls_finished {
            return Err(provider_protocol_failure());
        }
        if !self.tool_calls.contains_key(&index) {
            self.tool_call_count = self
                .tool_call_count
                .checked_add(1)
                .filter(|count| *count <= MAX_TOOL_CALLS)
                .ok_or_else(provider_protocol_failure)?;
        }
        let added_bytes = id.as_ref().map_or(0, String::len)
            + function
                .as_ref()
                .and_then(|value| value.name.as_ref())
                .map_or(0, String::len)
            + function
                .as_ref()
                .and_then(|value| value.arguments.as_ref())
                .map_or(0, String::len);
        self.tool_call_bytes = self
            .tool_call_bytes
            .checked_add(added_bytes)
            .filter(|total| *total <= MAX_TOOL_CALL_TOTAL_BYTES)
            .ok_or_else(provider_protocol_failure)?;
        let accumulator = self.tool_calls.entry(index).or_default();
        if let Some(id) = id {
            append_bounded(&mut accumulator.id, &id, MAX_TOOL_CALL_ID_BYTES)?;
        }
        if let Some(function) = function {
            if let Some(name) = function.name {
                append_bounded(&mut accumulator.name, &name, MAX_TOOL_NAME_BYTES)?;
            }
            if let Some(arguments) = function.arguments {
                append_bounded(
                    &mut accumulator.arguments,
                    &arguments,
                    MAX_TOOL_ARGUMENT_BYTES,
                )?;
            }
        }
        Ok(())
    }

    fn finish_success(
        &mut self,
        output: &mut Vec<NativeStreamItem>,
        budget: &mut PushOutputBudget,
    ) -> Result<(), RuntimeFailure> {
        self.flush_tool_calls(output, budget)?;
        self.terminal = true;
        budget.reserve(0)?;
        output.push(NativeStreamItem::PeerHalfClosed);
        budget.reserve(0)?;
        output.push(NativeStreamItem::Terminal(Ok(())));
        Ok(())
    }

    fn flush_tool_calls(
        &mut self,
        output: &mut Vec<NativeStreamItem>,
        budget: &mut PushOutputBudget,
    ) -> Result<(), RuntimeFailure> {
        let calls = std::mem::take(&mut self.tool_calls);
        for call in calls.into_values() {
            if call.id.is_empty()
                || call.name.is_empty()
                || serde_json::from_str::<serde_json::Value>(&call.arguments).is_err()
            {
                return Err(provider_protocol_failure());
            }
            budget.reserve(
                call.id
                    .len()
                    .checked_add(call.name.len())
                    .and_then(|bytes| bytes.checked_add(call.arguments.len()))
                    .ok_or_else(provider_protocol_failure)?,
            )?;
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

fn append_bounded(target: &mut String, value: &str, limit: usize) -> Result<(), RuntimeFailure> {
    if target
        .len()
        .checked_add(value.len())
        .is_none_or(|length| length > limit)
    {
        return Err(provider_protocol_failure());
    }
    target.push_str(value);
    Ok(())
}

fn provider_protocol_failure() -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
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
    reasoning_content: Option<String>,
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
            reasoning_effort: None,
            reasoning_enabled: None,
            reasoning_budget_tokens: None,
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
        assert_eq!(body["parallel_tool_calls"], true);
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
    fn decoder_rejects_a_huge_chunk_without_allocating_past_the_event_bound() {
        let mut decoder = SseDecoder::default();
        let error = decoder.push(&vec![b'x'; MAX_EVENT_BYTES * 4]).unwrap_err();
        assert!(matches!(error, RuntimeFailure::PluginFailure { .. }));
        assert_eq!(decoder.buffer.len(), MAX_EVENT_BYTES);
        assert!(decoder.buffer.capacity() <= MAX_EVENT_BYTES);
    }

    #[test]
    fn decoder_rechecks_an_oversized_tail_after_a_complete_frame() {
        let mut bytes = br#"data: {"choices":[{"delta":{"content":"ok"},"finish_reason":null}]}

"#
        .to_vec();
        bytes.extend(std::iter::repeat_n(b'x', MAX_EVENT_BYTES + 1));
        let mut decoder = SseDecoder::default();

        let error = decoder.push(&bytes).unwrap_err();

        assert!(matches!(error, RuntimeFailure::PluginFailure { .. }));
        assert_eq!(decoder.buffer.len(), MAX_EVENT_BYTES);
    }

    #[test]
    fn decoder_rejects_too_many_accumulated_tool_calls() {
        let calls = (0..=MAX_TOOL_CALLS)
            .map(|index| {
                serde_json::json!({
                    "index": index,
                    "id": format!("call-{index}"),
                    "function": {"name": "read", "arguments": "{}"},
                })
            })
            .collect::<Vec<_>>();
        let frame = format!(
            "data: {}\n\n",
            serde_json::json!({
                "choices": [{"delta": {"tool_calls": calls}, "finish_reason": null}]
            })
        );
        let error = SseDecoder::default().push(frame.as_bytes()).unwrap_err();
        assert!(matches!(error, RuntimeFailure::PluginFailure { .. }));
    }

    #[test]
    fn decoder_rejects_cumulative_tool_arguments_over_the_bound() {
        let fragment = "x".repeat(MAX_TOOL_ARGUMENT_BYTES / 2 + 1);
        let frame = |include_identity: bool| {
            let mut call = serde_json::json!({
                "index": 0,
                "function": {"arguments": fragment},
            });
            if include_identity {
                call["id"] = "call-1".into();
                call["function"]["name"] = "read".into();
            }
            format!(
                "data: {}\n\n",
                serde_json::json!({
                    "choices": [{"delta": {"tool_calls": [call]}, "finish_reason": null}]
                })
            )
        };
        let mut decoder = SseDecoder::default();
        decoder.push(frame(true).as_bytes()).unwrap();
        let error = decoder.push(frame(false).as_bytes()).unwrap_err();
        assert!(matches!(error, RuntimeFailure::PluginFailure { .. }));
    }

    #[test]
    fn decoder_bounds_total_tool_bytes_across_multiple_calls() {
        let fragment = "x".repeat(128 * 1024);
        let mut decoder = SseDecoder::default();
        let mut rejected = false;
        for index in 0_u64..MAX_TOOL_CALLS as u64 {
            let frame = format!(
                "data: {}\n\n",
                serde_json::json!({
                    "choices": [{
                        "delta": {"tool_calls": [{
                            "index": index,
                            "id": format!("call-{index}"),
                            "function": {"name": "read", "arguments": fragment},
                        }]},
                        "finish_reason": null,
                    }]
                })
            );
            if decoder.push(frame.as_bytes()).is_err() {
                rejected = true;
                assert!(index > 1, "the total bound must span multiple Tool calls");
                break;
            }
        }
        assert!(rejected);
    }

    #[test]
    fn decoder_rejects_a_second_tool_batch_after_the_first_finish() {
        let batch = |index: u64, id: &str| {
            format!(
                "data: {}\n\n",
                serde_json::json!({
                    "choices": [{
                        "delta": {"tool_calls": [{
                            "index": index,
                            "id": id,
                            "function": {"name": "read", "arguments": "{}"},
                        }]},
                        "finish_reason": "tool_calls",
                    }]
                })
            )
        };
        let mut decoder = SseDecoder::default();

        let first = decoder.push(batch(0, "call-1").as_bytes()).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(decoder.tool_call_count, 1);
        assert!(decoder.tool_call_bytes > 0);

        let error = decoder.push(batch(0, "call-2").as_bytes()).unwrap_err();
        assert!(matches!(error, RuntimeFailure::PluginFailure { .. }));
        assert_eq!(decoder.tool_call_count, 1);
    }

    #[test]
    fn decoder_accepts_usage_and_done_after_tool_calls_finish() {
        let mut decoder = SseDecoder::default();
        let tool = br#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-1","function":{"name":"read","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}

"#;
        decoder.push(tool).unwrap();

        let usage = decoder
            .push(
                br#"data: {"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":4}}

"#,
            )
            .unwrap();
        assert_eq!(usage.len(), 1);
        let done = decoder.push(b"data: [DONE]\n\n").unwrap();
        assert_eq!(done.len(), 2);
        assert!(decoder.terminal);
    }

    #[test]
    fn decoder_rejects_a_frame_after_done_in_the_same_push() {
        let mut decoder = SseDecoder::default();
        let error = decoder
            .push(
                concat!(
                    "data: [DONE]\n\n",
                    "data: {\"choices\":[{\"delta\":{\"content\":\"late\"},",
                    "\"finish_reason\":null}]}\n\n"
                )
                .as_bytes(),
            )
            .unwrap_err();

        assert!(matches!(error, RuntimeFailure::PluginFailure { .. }));
        assert!(decoder.terminal);
        assert_eq!(decoder.sequence, 0);
    }

    #[test]
    fn decoder_rejects_a_frame_after_done_in_a_later_push() {
        let mut decoder = SseDecoder::default();
        let done = decoder.push(b"data: [DONE]\n\n").unwrap();
        assert_eq!(done.len(), 2);

        let error = decoder
            .push(
                concat!(
                    "data: {\"choices\":[{\"delta\":{\"content\":\"late\"},",
                    "\"finish_reason\":null}]}\n\n"
                )
                .as_bytes(),
            )
            .unwrap_err();

        assert!(matches!(error, RuntimeFailure::PluginFailure { .. }));
        assert_eq!(decoder.sequence, 0);
    }

    #[test]
    fn decoder_bounds_output_items_from_many_valid_frames_in_one_push() {
        let frame = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"x\"},",
            "\"finish_reason\":null}]}\n\n"
        );
        let chunk = frame.repeat(MAX_PUSH_OUTPUT_ITEMS + 1);
        let mut decoder = SseDecoder::default();

        let error = decoder.push(chunk.as_bytes()).unwrap_err();

        assert!(matches!(error, RuntimeFailure::PluginFailure { .. }));
        assert_eq!(decoder.sequence, MAX_PUSH_OUTPUT_ITEMS as u64);
    }

    #[test]
    fn decoder_bounds_output_bytes_from_valid_frames_in_one_push() {
        let content = "x".repeat(MAX_EVENT_BYTES / 2);
        let frame = format!(
            "data: {}\n\n",
            serde_json::json!({
                "choices": [{"delta": {"content": content}, "finish_reason": null}]
            })
        );
        let frame_count = MAX_PUSH_OUTPUT_BYTES / (MAX_EVENT_BYTES / 2) + 1;
        let chunk = frame.repeat(frame_count);
        let mut decoder = SseDecoder::default();

        let error = decoder.push(chunk.as_bytes()).unwrap_err();

        assert!(matches!(error, RuntimeFailure::PluginFailure { .. }));
        assert!(decoder.sequence < frame_count as u64);
    }

    #[test]
    fn bounded_accumulator_accepts_the_limit_and_rejects_one_more_byte() {
        for limit in [
            MAX_TOOL_CALL_ID_BYTES,
            MAX_TOOL_NAME_BYTES,
            MAX_TOOL_ARGUMENT_BYTES,
        ] {
            let mut value = String::new();
            append_bounded(&mut value, &"x".repeat(limit), limit).unwrap();
            assert!(append_bounded(&mut value, "x", limit).is_err());
        }
    }

    #[test]
    fn decoder_preserves_provider_reasoning_content_before_text() {
        let mut decoder = SseDecoder::default();
        let events = decoder
            .push(
                br#"data: {"choices":[{"delta":{"reasoning_content":"Checking.","content":"Done."},"finish_reason":null}]}

"#,
            )
            .unwrap();
        let messages = events
            .into_iter()
            .filter_map(|event| match event {
                NativeStreamItem::Message(value) => value.downcast::<CompleteMessage>().ok(),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].kind, CompleteMessageKind::ReasoningSummaryDelta);
        assert_eq!(messages[0].text, "Checking.");
        assert_eq!(messages[1].kind, CompleteMessageKind::TextDelta);
        assert_eq!(messages[1].text, "Done.");
    }

    #[test]
    fn insecure_remote_base_url_is_rejected() {
        let error = OpenAiConfig {
            base_url: "http://example.com/v1".to_owned(),
            model: "test-model".to_owned(),
            allowed_models: Vec::new(),
            api_key_ref: "model/api-key".to_owned(),
            http_referer: None,
            app_title: None,
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
            allowed_models: Vec::new(),
            api_key_ref: "model/api-key".to_owned(),
            http_referer: None,
            app_title: None,
        }
        .validate()
        .unwrap_err();
        assert!(matches!(error, RuntimeFailure::InvalidResolvedPlan { .. }));
    }

    #[test]
    fn configured_auxiliary_model_is_admitted_without_changing_the_primary() {
        let config = OpenAiConfig {
            base_url: "https://api.openai.com/v1".to_owned(),
            model: "main-model".to_owned(),
            allowed_models: vec!["presentation-model".to_owned()],
            api_key_ref: "model/api-key".to_owned(),
            http_referer: None,
            app_title: None,
        }
        .validate()
        .unwrap();
        assert!(config.admits_model("main-model"));
        assert!(config.admits_model("presentation-model"));
        assert!(!config.admits_model("unreviewed-model"));
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
