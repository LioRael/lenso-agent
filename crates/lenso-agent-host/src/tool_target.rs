use std::{rc::Rc, sync::Arc};

use lenso_app_plan::{CapabilityEndpointPlan, authoring::PluginDescriptor};
use lenso_capability_agent_tool_target as contract;
use lenso_kernel::{InvocationContext, RuntimeFailure};
use lenso_native_adapter::{NativePluginFactory, NativePluginFactoryContext, NativePluginInstance};

pub(crate) const BRIDGE_PLUGIN_ID: &str = "lenso.agent.tool-target-bridge";
pub(crate) const BRIDGE_PLUGIN_VERSION: &str = "0.1.0";

/// Host adapter that freezes and routes the Console Agent's App Agent Tool catalog.
pub trait AgentToolTarget: std::fmt::Debug + Send + Sync + 'static {
    fn catalog(
        &self,
        context: InvocationContext,
        request: contract::CatalogRequest,
    ) -> lenso_kernel::NativeRequestFuture<contract::ToolTargetCatalog>;

    fn execute(
        &self,
        context: InvocationContext,
        request: contract::ExecuteRequest,
    ) -> lenso_kernel::NativeRequestFuture<contract::ToolTargetExecute>;
}

#[derive(Clone, Debug)]
pub(crate) struct AgentToolTargetBridgeFactory {
    target: Option<Arc<dyn AgentToolTarget>>,
}

impl AgentToolTargetBridgeFactory {
    pub(crate) fn new(target: Option<Arc<dyn AgentToolTarget>>) -> Self {
        Self { target }
    }
}

impl NativePluginFactory for AgentToolTargetBridgeFactory {
    fn package_id(&self) -> &'static str {
        BRIDGE_PLUGIN_ID
    }

    fn package_version(&self) -> &'static str {
        BRIDGE_PLUGIN_VERSION
    }

    fn instantiate(
        &self,
        _context: NativePluginFactoryContext<'_>,
    ) -> Result<NativePluginInstance, RuntimeFailure> {
        Ok(NativePluginInstance::new(vec![Rc::new(
            contract::ToolTargetEndpoint::new(Provider {
                target: self.target.clone(),
            }),
        )]))
    }
}

#[derive(Clone, Debug)]
struct Provider {
    target: Option<Arc<dyn AgentToolTarget>>,
}

impl contract::ToolTargetProvider for Provider {
    fn catalog(
        &self,
        context: InvocationContext,
        request: contract::CatalogRequest,
    ) -> lenso_kernel::NativeRequestFuture<contract::ToolTargetCatalog> {
        match self.target.as_ref() {
            Some(target) => target.catalog(context, request),
            None => Box::pin(async { Ok(Ok(contract::CatalogResponse { tools: Vec::new() })) }),
        }
    }

    fn execute(
        &self,
        context: InvocationContext,
        request: contract::ExecuteRequest,
    ) -> lenso_kernel::NativeRequestFuture<contract::ToolTargetExecute> {
        match self.target.as_ref() {
            Some(target) => target.execute(context, request),
            None => Box::pin(async { Ok(Err(contract::ExecuteError::TargetNotFound)) }),
        }
    }
}

pub(crate) fn bridge_descriptor() -> PluginDescriptor {
    PluginDescriptor::new(BRIDGE_PLUGIN_ID, BRIDGE_PLUGIN_VERSION, "tool-target")
        .with_runtime_package(BRIDGE_PLUGIN_ID, BRIDGE_PLUGIN_VERSION)
        .with_capability(
            CapabilityEndpointPlan::new(
                contract::CAPABILITY_ID,
                contract::DESCRIPTOR_VERSION,
                [contract::CATALOG_OPERATION, contract::EXECUTE_OPERATION],
            )
            .with_limits(8, 1),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use contract::ToolTargetProvider;
    use lenso_kernel::CancellationToken;
    use std::time::Duration;

    #[derive(Debug)]
    struct InspectTarget;

    impl AgentToolTarget for InspectTarget {
        fn catalog(
            &self,
            context: InvocationContext,
            _request: contract::CatalogRequest,
        ) -> lenso_kernel::NativeRequestFuture<contract::ToolTargetCatalog> {
            assert_context(&context);
            Box::pin(async { Ok(Ok(contract::CatalogResponse { tools: Vec::new() })) })
        }

        fn execute(
            &self,
            context: InvocationContext,
            _request: contract::ExecuteRequest,
        ) -> lenso_kernel::NativeRequestFuture<contract::ToolTargetExecute> {
            assert_context(&context);
            Box::pin(async move {
                Err(RuntimeFailure::Cancelled {
                    request_id: context.request_id(),
                })
            })
        }
    }

    fn assert_context(context: &InvocationContext) {
        assert_eq!(context.request_id(), 42);
        assert_eq!(context.caller_instance(), Some("console-agent"));
        assert_eq!(context.deadline(), Some(Duration::from_secs(5)));
        assert!(context.is_cancelled());
    }

    #[tokio::test]
    async fn bridge_preserves_invocation_context_and_runtime_failure() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let context = InvocationContext::new(42, Some(Duration::from_secs(5)), cancellation)
            .with_caller_instance("console-agent");
        let provider = Provider {
            target: Some(Arc::new(InspectTarget)),
        };
        provider
            .catalog(context.clone(), contract::CatalogRequest {})
            .await
            .unwrap()
            .unwrap();
        let result = provider
            .execute(
                context,
                contract::ExecuteRequest {
                    name: "projects_get_issue".into(),
                    arguments_json: "{}".parse().unwrap(),
                },
            )
            .await;
        assert!(matches!(
            result,
            Err(RuntimeFailure::Cancelled { request_id: 42 })
        ));
    }
}
