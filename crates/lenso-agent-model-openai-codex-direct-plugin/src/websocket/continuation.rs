//! Disposable socket-local optimization; caller messages remain authoritative.
use serde_json::{Value, json};

use super::{MAX_REQUEST_BYTES, ResponsesRequest};

pub(super) struct Checkpoint {
    pub scope: String,
    parameters: Value,
    input: Vec<Value>,
    text: String,
    calls: Vec<Value>,
    response_id: Option<String>,
    bytes: usize,
}

fn parameters(request: &ResponsesRequest) -> Value {
    request.body.as_object().map_or(Value::Null, |body| {
        Value::Object(
            body.iter()
                .filter(|(key, _)| key.as_str() != "input")
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        )
    })
}

impl Checkpoint {
    pub fn start(request: &ResponsesRequest) -> Option<Self> {
        let scope = request.continuation_scope.as_ref()?;
        if scope.is_empty() || scope.len() > 128 {
            return None;
        }
        let bytes = request.body.to_string().len();
        if bytes > MAX_REQUEST_BYTES {
            return None;
        }
        Some(Self {
            scope: scope.clone(),
            parameters: parameters(request),
            input: request.body.get("input")?.as_array()?.clone(),
            text: String::new(),
            calls: Vec::new(),
            response_id: None,
            bytes,
        })
    }

    pub fn observe(&mut self, event: &Value) -> bool {
        self.bytes = self.bytes.saturating_add(event.to_string().len());
        if self.bytes > MAX_REQUEST_BYTES {
            return false;
        }
        match event.get("type").and_then(Value::as_str) {
            Some("response.output_text.delta") => {
                if let Some(text) = event.get("delta").and_then(Value::as_str) {
                    self.text.push_str(text);
                }
            }
            Some("response.output_item.done") => {
                let Some(item) = event.get("item") else {
                    return false;
                };
                if item.get("type").and_then(Value::as_str) == Some("function_call") {
                    self.calls.push(json!({"type":"function_call", "call_id":item["call_id"], "name":item["name"], "arguments":item["arguments"]}));
                }
            }
            Some("response.completed" | "response.done") => {
                self.response_id = event
                    .get("response")
                    .and_then(|response| response.get("id"))
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty() && id.len() <= 256)
                    .map(str::to_owned);
            }
            _ => {}
        }
        true
    }

    pub fn incremental_body(&self, request: &ResponsesRequest) -> Option<Value> {
        if request.continuation_scope.as_deref() != Some(self.scope.as_str())
            || parameters(request) != self.parameters
        {
            return None;
        }
        let id = self.response_id.as_ref()?;
        let input = request.body.get("input")?.as_array()?;
        if !input.starts_with(&self.input) {
            return None;
        }
        let mut tail = input[self.input.len()..].iter().peekable();
        // The Agent aggregates text before its ordered Tool call/result pairs.
        if !self.text.is_empty() {
            let expected = json!({"type":"message", "role":"assistant", "content":[{"type":"output_text", "text":self.text}]});
            if tail.next() != Some(&expected) {
                return None;
            }
        }
        let mut delta = Vec::new();
        for call in &self.calls {
            if tail.next() != Some(call) {
                return None;
            }
            let result = tail.next()?;
            if result.get("type").and_then(Value::as_str) != Some("function_call_output")
                || result.get("call_id") != call.get("call_id")
            {
                return None;
            }
            delta.push(result.clone());
        }
        // Anything except newly appended user input invalidates the hint. In
        // particular, another branch's assistant output is never silently dropped.
        for item in tail {
            if item.get("role").and_then(Value::as_str) != Some("user") {
                return None;
            }
            delta.push(item.clone());
        }
        if delta.is_empty() {
            return None;
        }
        let mut body = request.body.clone();
        body["previous_response_id"] = json!(id);
        body["input"] = json!(delta);
        Some(body)
    }
}
