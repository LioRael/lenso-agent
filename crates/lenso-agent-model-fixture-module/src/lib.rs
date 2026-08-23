//! Deterministic Model Module for the headless read-only proof.

use std::rc::Rc;

use futures::future::{LocalBoxFuture, ready};
use lenso_agent_native_support::FiniteOutputStream;
use lenso_capability_agent_model::{
    CAPABILITY_ID, CompleteError, CompleteRequest, CompleteRequestMessagesItemRole,
    CompleteResponse, CompleteResponseKind, ModelEndpoint, ModelInvocationError, ModelProvider,
};
use lenso_kernel::{InvocationContext, NativeStreamEndpoint, NativeStreamSession, RuntimeFailure};
use lenso_native_adapter::{NativeModuleFactory, NativeModuleFactoryContext, NativeModuleInstance};

/// Runtime package identity selected by App Composition.
pub const PACKAGE_ID: &str = "lenso.agent.model.fixture";
/// Exact linked package version.
pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Only model identifier supported by the deterministic fixture.
pub const MODEL_ID: &str = "fixture/readme-summary-v1";

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureConfig {
    model: String,
}

/// Native factory for the deterministic Model fixture.
#[derive(Clone, Debug, Default)]
pub struct FixtureModelFactory;

impl NativeModuleFactory for FixtureModelFactory {
    fn package_id(&self) -> &'static str {
        PACKAGE_ID
    }

    fn package_version(&self) -> &'static str {
        PACKAGE_VERSION
    }

    fn instantiate(
        &self,
        context: NativeModuleFactoryContext<'_>,
    ) -> Result<NativeModuleInstance, RuntimeFailure> {
        if context.entrypoint() != "default" {
            return Err(RuntimeFailure::InvalidResolvedPlan {
                detail: format!(
                    "unsupported fixture Model entrypoint `{}`",
                    context.entrypoint()
                ),
            });
        }
        let config =
            serde_json::from_str::<FixtureConfig>(context.configuration()).map_err(|error| {
                RuntimeFailure::InvalidResolvedPlan {
                    detail: format!("invalid fixture Model configuration: {error}"),
                }
            })?;
        if config.model != MODEL_ID {
            return Err(RuntimeFailure::InvalidResolvedPlan {
                detail: format!("fixture Model must be `{MODEL_ID}`"),
            });
        }
        let endpoint =
            Rc::new(ModelEndpoint::new(FixtureModel { config })) as Rc<dyn NativeStreamEndpoint>;
        Ok(NativeModuleInstance::with_stream_endpoints(
            vec![endpoint],
            lenso_kernel::NoopModuleLifecycle,
        ))
    }
}

#[derive(Clone, Debug)]
struct FixtureModel {
    config: FixtureConfig,
}

impl ModelProvider for FixtureModel {
    fn complete(
        &self,
        _context: InvocationContext,
        request: CompleteRequest,
    ) -> LocalBoxFuture<'static, Result<Box<dyn NativeStreamSession>, ModelInvocationError>> {
        let result = self.complete_now(&request).map(|messages| {
            Box::new(FiniteOutputStream::successful(CAPABILITY_ID, messages))
                as Box<dyn NativeStreamSession>
        });
        Box::pin(ready(result))
    }
}

impl FixtureModel {
    fn complete_now(
        &self,
        request: &CompleteRequest,
    ) -> Result<Vec<CompleteResponse>, ModelInvocationError> {
        if request.model != self.config.model || request.max_output_tokens <= 0 {
            return Err(ModelInvocationError::Domain(
                CompleteError::UnsupportedModel,
            ));
        }
        let current_user_index = request
            .messages
            .iter()
            .rposition(|message| message.role == CompleteRequestMessagesItemRole::User)
            .ok_or(ModelInvocationError::Domain(CompleteError::InvalidRequest))?;
        let current_user = &request.messages[current_user_index].content;
        if current_user.starts_with("Answer directly:") {
            let plugin_prefix = request.messages.iter().any(|message| {
                message.role == CompleteRequestMessagesItemRole::System
                    && message
                        .content
                        .contains("Prefix direct answers with `Plugin: `.")
            });
            let filesystem_prefix = request.messages.iter().any(|message| {
                message.role == CompleteRequestMessagesItemRole::System
                    && message
                        .content
                        .contains("Prefix direct answers with `Filesystem: `.")
            });
            return Ok(direct_response(plugin_prefix, filesystem_prefix));
        }
        if current_user == "What did you summarize?" {
            let previous = request.messages[..current_user_index]
                .iter()
                .rev()
                .find(|message| message.role == CompleteRequestMessagesItemRole::Assistant)
                .map_or("Nothing yet.", |message| message.content.as_str());
            return Ok(previous_response(previous));
        }
        let tool_results = request.messages[current_user_index + 1..]
            .iter()
            .filter(|message| message.role == CompleteRequestMessagesItemRole::Tool)
            .collect::<Vec<_>>();
        if current_user == "Read README.md twice." && tool_results.len() < 2 {
            return Ok(tool_request(tool_results.len() + 1));
        }
        let tool_result = tool_results.last().copied();
        if let Some(tool_result) = tool_result {
            let first_line = tool_result
                .content
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("The README is empty.")
                .trim();
            return Ok(summary_response(first_line));
        }
        let has_workspace_tool = request
            .tools
            .iter()
            .any(|tool| tool.name == "workspace.read_text");
        if !has_workspace_tool {
            return Err(ModelInvocationError::Domain(CompleteError::InvalidRequest));
        }
        Ok(tool_request(1))
    }
}

fn direct_response(plugin_prefix: bool, filesystem_prefix: bool) -> Vec<CompleteResponse> {
    let prefix = match (filesystem_prefix, plugin_prefix) {
        (true, true) => "Filesystem: Plugin: ",
        (true, false) => "Filesystem: ",
        (false, true) => "Plugin: ",
        (false, false) => "",
    };
    vec![
        response(
            "1",
            CompleteResponseKind::TextDelta,
            format!("{prefix}Direct "),
            "",
            "",
            "{}",
            "0",
            "0",
        ),
        response(
            "2",
            CompleteResponseKind::TextDelta,
            "answer.",
            "",
            "",
            "{}",
            "0",
            "0",
        ),
        response("3", CompleteResponseKind::Usage, "", "", "", "{}", "8", "2"),
    ]
}

fn previous_response(previous: &str) -> Vec<CompleteResponse> {
    vec![
        response(
            "1",
            CompleteResponseKind::TextDelta,
            format!("Previous answer: {previous}"),
            "",
            "",
            "{}",
            "0",
            "0",
        ),
        response(
            "2",
            CompleteResponseKind::Usage,
            "",
            "",
            "",
            "{}",
            "16",
            "8",
        ),
    ]
}

fn tool_request(index: usize) -> Vec<CompleteResponse> {
    vec![
        response(
            "1",
            CompleteResponseKind::ToolCall,
            "",
            &format!("call-readme-{index}"),
            "workspace.read_text",
            r#"{"path":"README.md"}"#,
            "0",
            "0",
        ),
        response(
            "2",
            CompleteResponseKind::Usage,
            "",
            "",
            "",
            "{}",
            "24",
            "8",
        ),
    ]
}

fn summary_response(first_line: &str) -> Vec<CompleteResponse> {
    vec![
        response(
            "1",
            CompleteResponseKind::TextDelta,
            format!("README summary: {first_line}"),
            "",
            "",
            "{}",
            "0",
            "0",
        ),
        response(
            "2",
            CompleteResponseKind::Usage,
            "",
            "",
            "",
            "{}",
            "32",
            "12",
        ),
    ]
}

#[allow(clippy::too_many_arguments)]
fn response(
    sequence: &str,
    kind: CompleteResponseKind,
    text: impl Into<String>,
    tool_call_id: &str,
    tool_name: &str,
    arguments_json: &str,
    input_tokens: &str,
    output_tokens: &str,
) -> CompleteResponse {
    CompleteResponse {
        sequence: sequence.to_owned(),
        kind,
        text: text.into(),
        tool_call_id: tool_call_id.to_owned(),
        tool_name: tool_name.to_owned(),
        arguments_json: arguments_json.to_owned(),
        input_tokens: input_tokens.to_owned(),
        output_tokens: output_tokens.to_owned(),
    }
}
