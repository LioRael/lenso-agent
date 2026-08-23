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
        let tool_result = request
            .messages
            .iter()
            .rev()
            .find(|message| message.role == CompleteRequestMessagesItemRole::Tool);
        if let Some(tool_result) = tool_result {
            let first_line = tool_result
                .content
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("The README is empty.")
                .trim();
            return Ok(vec![
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
            ]);
        }
        let has_workspace_tool = request
            .tools
            .iter()
            .any(|tool| tool.name == "workspace.read_text");
        if !has_workspace_tool {
            return Err(ModelInvocationError::Domain(CompleteError::InvalidRequest));
        }
        Ok(vec![
            response(
                "1",
                CompleteResponseKind::ToolCall,
                "",
                "call-readme-1",
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
        ])
    }
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
