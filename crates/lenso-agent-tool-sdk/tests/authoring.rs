use lenso::{Ctx, PluginError, PluginResult};
use lenso_agent_tool_sdk::prelude::*;
use lenso_capability_agent_tool_provider::{CatalogRequest, ExecuteRequest, ToolExecutionClass};
use lenso_kernel::{CancellationToken, RuntimeFailure};
use schemars::JsonSchema;

#[derive(JsonSchema, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Message {
    value: String,
}

#[lenso::plugin]
#[derive(Clone, Copy, Debug)]
struct FixtureTools {}

#[tool_provider]
impl FixtureTools {
    #[tool(
        name = "downstream",
        description = "Preserve a downstream outcome and its invocation context.",
        execution = "exclusive"
    )]
    async fn downstream(
        message: Message,
        context: Ctx,
    ) -> PluginResult<ExecuteResponse, ExecuteError> {
        assert_eq!(context.request_id(), 42);
        assert_eq!(context.caller_instance(), Some("authorized-consumer"));
        assert_eq!(context.deadline(), Some(std::time::Duration::from_secs(5)));
        if context.is_cancelled() {
            return Err(PluginError::runtime(RuntimeFailure::AdmissionClosed));
        }
        std::future::ready(match message.value.as_str() {
            "denied" => Err(PluginError::domain(ExecuteError::PermissionDenied)),
            "offline" => Err(PluginError::runtime(RuntimeFailure::PluginFailure {
                detail: "downstream unavailable".to_owned(),
            })),
            _ => Ok(ExecuteResponse {
                content_blocks: None,
                content: r#"{"id":"issue-1","revision":"2"}"#.to_owned(),
                content_type: ContentType::Text,
                metadata_json: r#"{"revision":"2"}"#.try_into().unwrap(),
            }),
        })
        .await
    }

    #[tool(
        name = "sync_echo",
        description = "Echo synchronously.",
        execution = "exclusive"
    )]
    #[allow(
        clippy::trivially_copy_pass_by_ref,
        clippy::unnecessary_wraps,
        clippy::unused_self,
        reason = "the fixture exercises stateful synchronous Tool dispatch"
    )]
    fn sync_echo(&self, message: Message) -> Result<ExecuteResponse, ExecuteError> {
        Ok(response(message.value))
    }

    #[tool(
        name = "async_echo",
        description = "Echo asynchronously.",
        execution = "parallel_safe"
    )]
    async fn async_echo(message: Message) -> Result<ExecuteResponse, ExecuteError> {
        std::future::ready(Ok(response(message.value))).await
    }

    #[tool(
        name = "context_echo",
        description = "Echo through an inherited invocation context.",
        execution = "exclusive"
    )]
    #[allow(
        clippy::needless_pass_by_value,
        clippy::trivially_copy_pass_by_ref,
        clippy::unnecessary_wraps,
        clippy::unused_self,
        reason = "the fixture exercises stateful context-aware Tool dispatch"
    )]
    fn context_echo(
        &self,
        message: Message,
        context: Ctx,
    ) -> Result<ExecuteResponse, ExecuteError> {
        assert!(!context.is_cancelled());
        Ok(response(message.value))
    }
}

fn response(content: String) -> ExecuteResponse {
    ExecuteResponse {
        content_blocks: None,
        content,
        content_type: ContentType::Text,
        metadata_json: "{}".try_into().unwrap(),
    }
}

fn context() -> Ctx {
    Ctx::new(1, None, CancellationToken::new())
}

#[test]
fn one_provider_derives_and_dispatches_multiple_typed_tools() {
    let provider = FixtureTools {};
    let catalog = futures::executor::block_on(provider.catalog(context(), CatalogRequest {}))
        .expect("catalog must be derived");
    assert_eq!(
        catalog
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["downstream", "sync_echo", "async_echo", "context_echo"]
    );
    assert_eq!(
        catalog
            .tools
            .iter()
            .map(|tool| tool.execution.clone())
            .collect::<Vec<_>>(),
        [
            ToolExecutionClass::Exclusive,
            ToolExecutionClass::Exclusive,
            ToolExecutionClass::ParallelSafe,
            ToolExecutionClass::Exclusive
        ]
    );

    for name in ["sync_echo", "async_echo", "context_echo"] {
        let result = futures::executor::block_on(provider.execute(
            context(),
            ExecuteRequest {
                name: name.to_owned(),
                arguments_json: r#"{"value":"hello"}"#.try_into().unwrap(),
            },
        ))
        .unwrap();
        assert_eq!(result.content, "hello");
    }

    let invalid = futures::executor::block_on(provider.execute(
        context(),
        ExecuteRequest {
            name: "sync_echo".to_owned(),
            arguments_json: r#"{"unknown":true}"#.try_into().unwrap(),
        },
    ));
    assert!(matches!(
        invalid,
        Err(PluginError::Domain(ExecuteError::InvalidArguments))
    ));
}

#[test]
fn native_tools_preserve_domain_runtime_and_structured_outcomes() {
    let invoke = |value: &str, cancelled: bool| {
        let cancellation = CancellationToken::new();
        if cancelled {
            cancellation.cancel();
        }
        futures::executor::block_on(
            FixtureTools {}.execute(
                Ctx::new(42, Some(std::time::Duration::from_secs(5)), cancellation)
                    .with_caller_instance("authorized-consumer"),
                ExecuteRequest {
                    name: "downstream".to_owned(),
                    arguments_json: serde_json::json!({"value": value})
                        .to_string()
                        .try_into()
                        .unwrap(),
                },
            ),
        )
    };
    assert!(matches!(
        invoke("denied", false),
        Err(PluginError::Domain(ExecuteError::PermissionDenied))
    ));
    assert!(
        matches!(invoke("offline", false), Err(PluginError::Runtime(RuntimeFailure::PluginFailure { detail })) if detail == "downstream unavailable")
    );
    assert!(matches!(
        invoke("ok", true),
        Err(PluginError::Runtime(RuntimeFailure::AdmissionClosed))
    ));
    let success = invoke("ok", false).unwrap();
    assert_eq!(success.content_type, ContentType::Text);
    assert_eq!(success.content, r#"{"id":"issue-1","revision":"2"}"#);
    assert_eq!(success.metadata_json.as_str(), r#"{"revision":"2"}"#);
}
