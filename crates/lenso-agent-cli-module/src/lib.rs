//! Endpoint-free CLI consumer Module.

use lenso_kernel::RuntimeFailure;
use lenso_native_adapter::{NativeModuleFactoryContext, NativeModuleInstance};

/// Instantiates the endpoint-free CLI consumer identity.
#[lenso_native_adapter::module(
    descriptor = r#"{"provided_capabilities":[],"required_capabilities":[{"capability_id":"lenso.agent@1","descriptor_version":"1.1.0","cardinality":"one"}]}"#
)]
fn instantiate(
    context: NativeModuleFactoryContext<'_>,
) -> Result<NativeModuleInstance, RuntimeFailure> {
    if context.entrypoint() != "default" || context.configuration() != "{}" {
        return Err(RuntimeFailure::InvalidResolvedPlan {
            detail: "CLI Module requires entrypoint `default` and empty configuration".to_owned(),
        });
    }
    Ok(NativeModuleInstance::default())
}
