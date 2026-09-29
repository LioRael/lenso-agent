use super::*;
use lenso_app_plan::{
    AppComposition, CapabilityBinding, CapabilityEndpointPlan, CapabilityRequirementPlan,
    PluginInstancePlan,
};
use lenso_kernel::{
    CancellationToken, InvocationContext, Kernel, NativeRequestFuture, RuntimeFailure,
};
use lenso_native_adapter::{
    NativePluginFactory, NativePluginFactoryContext, NativePluginInstance, NativePluginRegistry,
};
use lenso_runner::TokioDriver;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

type CapturedContexts = Rc<RefCell<Vec<(u64, Option<String>)>>>;

#[derive(Debug, Clone)]
struct Owner {
    visible: Rc<Cell<bool>>,
    calls: Rc<Cell<u32>>,
    states: Rc<RefCell<management::InvocationState>>,
    contexts: CapturedContexts,
}
impl NativePluginFactory for Owner {
    fn package_id(&self) -> &'static str {
        "test.management.owner"
    }
    fn instantiate(
        &self,
        _: NativePluginFactoryContext<'_>,
    ) -> Result<NativePluginInstance, RuntimeFailure> {
        Ok(NativePluginInstance::new(vec![Rc::new(
            management::ManagementEndpoint::new(self.clone()),
        )]))
    }
}
impl management::ManagementProvider for Owner {
    fn catalog(
        &self,
        _: InvocationContext,
        _: management::CatalogRequest,
    ) -> NativeRequestFuture<management::ManagementCatalog> {
        let entries = if self.visible.get() {
            vec![management::Entry {id:"state.set".into(),target_instance:"example.state/alpha".into(),capability:"example.state@1".into(),version:"1.0.0".into(),operation:"set".into(),input_schema_json:r#"{"type":"object","additionalProperties":false,"properties":{"value":{"type":"integer"}},"required":["value"]}"#.parse().unwrap(),description:"Set the exact reference state".into(),effect:management::Effect::Write,requires_approval:true}]
        } else {
            vec![]
        };
        Box::pin(async move {
            Ok(Ok(management::CatalogResponse {
                deployment: "alpha".into(),
                revision: "1".into(),
                entries,
            }))
        })
    }
    fn invoke(
        &self,
        context: InvocationContext,
        request: management::InvokeRequest,
    ) -> NativeRequestFuture<management::ManagementInvoke> {
        self.calls.set(self.calls.get() + 1);
        self.contexts.borrow_mut().push((
            context.request_id(),
            context.caller_instance().map(str::to_owned),
        ));
        assert_eq!(request.entry_id, "state.set");
        assert_eq!(request.version, "1.0.0");
        let state = self.states.borrow().clone();
        Box::pin(async move {
            Ok(Ok(management::InvokeResponse {
                operation_id: Some("operation-1".into()),
                state,
                result_json: None,
                receipt: None,
                audit_pending: false,
            }))
        })
    }
    fn status(
        &self,
        _: InvocationContext,
        _: management::StatusRequest,
    ) -> NativeRequestFuture<management::ManagementStatus> {
        let state = self.states.borrow().clone();
        Box::pin(async move {
            Ok(Ok(management::InvokeResponse {
                operation_id: Some("operation-1".into()),
                state,
                result_json: None,
                receipt: None,
                audit_pending: false,
            }))
        })
    }
}
#[derive(Debug)]
struct Caller;
impl NativePluginFactory for Caller {
    fn package_id(&self) -> &'static str {
        "test.management.tools"
    }
    fn instantiate(
        &self,
        _: NativePluginFactoryContext<'_>,
    ) -> Result<NativePluginInstance, RuntimeFailure> {
        Ok(NativePluginInstance::default())
    }
}
fn context() -> InvocationContext {
    InvocationContext::new(71, None, CancellationToken::new())
}

#[tokio::test(flavor = "current_thread")]
#[allow(clippy::too_many_lines)]
async fn bound_catalog_is_refreshed_and_pending_unknown_are_preserved() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let owner = Owner {
                visible: Rc::new(Cell::new(true)),
                calls: Rc::new(Cell::new(0)),
                states: Rc::new(RefCell::new(management::InvocationState::PendingApproval)),
                contexts: Rc::new(RefCell::new(vec![])),
            };
            let plan = AppComposition::new(
                vec![
                    PluginInstancePlan::new("tools", "test.management.tools").with_requirement(
                        CapabilityRequirementPlan::one(
                            management::CAPABILITY_ID,
                            management::DESCRIPTOR_VERSION,
                        ),
                    ),
                    PluginInstancePlan::new("management", "test.management.owner").with_capability(
                        CapabilityEndpointPlan::new(
                            management::CAPABILITY_ID,
                            management::DESCRIPTOR_VERSION,
                            ["catalog", "invoke", "status"],
                        ),
                    ),
                ],
                vec![CapabilityBinding::new(
                    "tools",
                    management::CAPABILITY_ID,
                    management::DESCRIPTOR_VERSION,
                    "management",
                )],
            )
            .resolve()
            .unwrap();
            let app = Kernel::start_native(
                plan,
                TokioDriver::new(),
                NativePluginRegistry::new()
                    .with_factory(Caller)
                    .with_factory(owner.clone()),
            )
            .await
            .unwrap();
            let provider = ManagementTools {
                management: Port::new(),
            };
            provider
                .management
                .connect(&app.dependencies("tools").unwrap())
                .unwrap();
            let catalog = provider
                .catalog(context(), tools::CatalogRequest {})
                .await
                .unwrap();
            assert_eq!(catalog.tools.len(), 2);
            let name = catalog.tools[0].name.clone();
            let call = || tools::ExecuteRequest {
                name: name.clone(),
                arguments_json: r#"{"input":{"value":1},"idempotency_key":"request-1"}"#
                    .parse()
                    .unwrap(),
            };
            let pending = provider.execute(context(), call()).await.unwrap();
            let parsed: management::InvokeResponse =
                serde_json::from_str(&pending.content).unwrap();
            assert_eq!(parsed.state, management::InvocationState::PendingApproval);
            assert!(parsed.receipt.is_none());
            owner.states.replace(management::InvocationState::Unknown);
            let unknown = provider.execute(context(), call()).await.unwrap();
            assert_eq!(
                serde_json::from_str::<management::InvokeResponse>(&unknown.content)
                    .unwrap()
                    .state,
                management::InvocationState::Unknown
            );
            assert_eq!(owner.contexts.borrow()[0], (71, Some("tools".into())));
            owner.visible.set(false);
            assert!(matches!(
                provider.execute(context(), call()).await,
                Err(PluginError::Domain(tools::ExecuteError::NotFound))
            ));
            assert_eq!(owner.calls.get(), 2);
            let bad = tools::ExecuteRequest {
                name: name.clone(),
                arguments_json: r#"{"input":{},"deployment":"other","approved":true}"#
                    .parse()
                    .unwrap(),
            };
            owner.visible.set(true);
            assert!(matches!(
                provider.execute(context(), bad).await,
                Err(PluginError::Domain(tools::ExecuteError::InvalidArguments))
            ));
            assert_eq!(owner.calls.get(), 2);
            assert!(
                !catalog
                    .tools
                    .iter()
                    .any(|tool| tool.name.contains("approve") || tool.name.contains("credential"))
            );
            assert_eq!(
                app.shutdown(Duration::from_secs(1)).await,
                lenso_kernel::ShutdownOutcome::Clean
            );
        })
        .await;
}
