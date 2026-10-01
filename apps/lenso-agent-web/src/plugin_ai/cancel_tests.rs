use super::*;
use lenso_app_plan::{
    CapabilityEndpointPlan, CapabilityRequirementPlan,
    authoring::{
        HostCatalog, HostDefaultPlugin, HostPluginRelease, HostSlot, PluginDescriptor,
        PluginRootSnapshot, resolve_plugin_root,
    },
};
use lenso_kernel::{
    InvocationContext, Kernel, NativeStreamEndpoint, NoopPluginLifecycle, RuntimeFailure,
    ShutdownOutcome,
};
use lenso_native_adapter::{
    NativePluginFactory, NativePluginFactoryContext, NativePluginInstance, NativePluginRegistry,
};
use std::{any::Any, cell::Cell, rc::Rc};

#[derive(Debug)]
struct SlowOpen {
    entered: Rc<Cell<bool>>,
    cancelled: Rc<Cell<bool>>,
}
struct CancelWitness {
    token: CancellationToken,
    observed: Rc<Cell<bool>>,
}
impl Drop for CancelWitness {
    fn drop(&mut self) {
        if self.token.is_cancelled() {
            self.observed.set(true);
        }
    }
}
impl NativeStreamEndpoint for SlowOpen {
    fn capability_id(&self) -> &'static str {
        CAPABILITY_ID
    }
    fn descriptor_version(&self) -> &'static str {
        lenso_capability_agent_model::DESCRIPTOR_VERSION
    }
    fn operations(&self) -> &'static [&'static str] {
        &[COMPLETE_OPERATION]
    }
    fn open(
        &self,
        _: &str,
        _: Box<dyn Any>,
        context: InvocationContext,
    ) -> futures::future::LocalBoxFuture<
        'static,
        Result<Result<Box<dyn lenso_kernel::NativeStreamSession>, Box<dyn Any>>, RuntimeFailure>,
    > {
        let entered = self.entered.clone();
        let cancelled = self.cancelled.clone();
        Box::pin(async move {
            let _witness = CancelWitness {
                token: context.cancellation(),
                observed: cancelled.clone(),
            };
            entered.set(true);
            context.cancellation().cancelled().await;
            cancelled.set(true);
            Err(RuntimeFailure::PluginFailure {
                detail: "synthetic provider observed native cancellation".into(),
            })
        })
    }
}
#[derive(Debug)]
struct Factory {
    id: &'static str,
    endpoint: Rc<SlowOpen>,
}
impl NativePluginFactory for Factory {
    fn package_id(&self) -> &'static str {
        self.id
    }
    fn package_version(&self) -> &'static str {
        "1.0.0"
    }
    fn runtime_profile(&self) -> &'static str {
        if self.id == "test.slow-model" {
            "lenso.native-authoring@2"
        } else {
            "lenso.native-authoring@1"
        }
    }
    fn factory_identity(&self) -> String {
        "1.0.0".into()
    }
    fn instantiate(
        &self,
        _: NativePluginFactoryContext<'_>,
    ) -> Result<NativePluginInstance, RuntimeFailure> {
        Ok(if self.id == "test.slow-model" {
            NativePluginInstance::with_stream_endpoints(
                vec![self.endpoint.clone()],
                NoopPluginLifecycle,
            )
        } else {
            NativePluginInstance::default()
        })
    }
}

#[tokio::test(flavor = "current_thread")]
async fn cancel_before_stream_open_finishes_reaches_dispatched_kernel_provider() {
    tokio::task::LocalSet::new().run_until(async {
        let entered = Rc::new(Cell::new(false)); let cancelled = Rc::new(Cell::new(false));
        let endpoint = Rc::new(SlowOpen { entered: entered.clone(), cancelled: cancelled.clone() });
        let provider = PluginDescriptor::new("test.slow-model","1.0.0","model").with_authoring(2,"lenso.native-authoring@2")
            .with_capability(CapabilityEndpointPlan::new(CAPABILITY_ID,lenso_capability_agent_model::DESCRIPTOR_VERSION,[COMPLETE_OPERATION]).with_stream_operation(COMPLETE_OPERATION));
        let consumer = PluginDescriptor::new("test.model-consumer","1.0.0","consumer")
            .with_requirement(CapabilityRequirementPlan::one(CAPABILITY_ID,lenso_capability_agent_model::DESCRIPTOR_VERSION));
        let catalog = HostCatalog::new([HostSlot::one("model"),HostSlot::one("consumer")],
            [HostPluginRelease::new(provider),HostPluginRelease::new(consumer)],
            [HostDefaultPlugin::new("test.slow-model","default"),HostDefaultPlugin::new("test.model-consumer","default")]);
        let plan = resolve_plugin_root(&catalog,&PluginRootSnapshot::default()).unwrap();
        let registry = NativePluginRegistry::new().with_factory(Factory { id:"test.slow-model",endpoint:endpoint.clone() })
            .with_factory(Factory { id:"test.model-consumer",endpoint });
        let app = Kernel::start_native(plan.plan().clone(),lenso_runner::TokioDriver::new(),registry).await.unwrap();
        let handle = app.stream_handle::<lenso_capability_agent_model::ModelComplete>("test.model-consumer/default").unwrap();
        let request = serde_json::from_value(serde_json::json!({"model":"synthetic","messages":[],"tools":[],"temperature":0.0,"max_output_tokens":10})).unwrap();
        let native = CancellationToken::new(); let signal = Signal::new();
        let open = async {
            handle.open_with_context(COMPLETE_OPERATION,InvocationContext::new(1,None,native.clone()),request).await
                .map_err(|error| format!("{error:?}"))?.map_err(|error| format!("{error:?}"))
        };
        let cancel = async {
            while !entered.get() { tokio::task::yield_now().await; }
            signal.cancel();
        };
        let (result,()) = tokio::join!(cancel_open(&native,&signal,open),cancel);
        assert!(result.is_err());
        tokio::time::timeout(Duration::from_secs(1), async { while !cancelled.get() { tokio::task::yield_now().await; } }).await.unwrap();
        assert_eq!(app.shutdown(Duration::from_secs(2)).await,ShutdownOutcome::Clean);
    }).await;
}
