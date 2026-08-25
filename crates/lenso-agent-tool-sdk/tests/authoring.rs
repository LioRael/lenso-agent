use lenso::{Ctx, ModuleError};
use lenso_agent_tool_sdk::prelude::*;
use lenso_capability_agent_tool_provider::{CatalogRequest, ExecuteRequest};
use lenso_kernel::CancellationToken;
use schemars::JsonSchema;

#[derive(JsonSchema, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Message {
    value: String,
}

#[lenso::module]
#[derive(Clone, Copy, Debug)]
struct FixtureTools {}

#[tool_provider]
impl FixtureTools {
    #[tool(name = "sync_echo", description = "Echo synchronously.")]
    #[allow(
        clippy::trivially_copy_pass_by_ref,
        clippy::unnecessary_wraps,
        clippy::unused_self,
        reason = "the fixture exercises stateful synchronous Tool dispatch"
    )]
    fn sync_echo(&self, message: Message) -> Result<ExecuteResponse, ExecuteError> {
        Ok(response(message.value))
    }

    #[tool(name = "async_echo", description = "Echo asynchronously.")]
    async fn async_echo(message: Message) -> Result<ExecuteResponse, ExecuteError> {
        std::future::ready(Ok(response(message.value))).await
    }
}

fn response(content: String) -> ExecuteResponse {
    ExecuteResponse {
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
        ["sync_echo", "async_echo"]
    );

    for name in ["sync_echo", "async_echo"] {
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
        Err(ModuleError::Domain(ExecuteError::InvalidArguments))
    ));
}
