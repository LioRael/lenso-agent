//! Endpoint-free TUI Shell consumer Module.

use lenso_kernel::RuntimeFailure;
use lenso_native_adapter::{NativeModuleFactoryContext, NativeModuleInstance};

/// Instantiates the endpoint-free TUI Shell identity.
#[lenso_native_adapter::module(
    descriptor = r#"{"provided_capabilities":[],"required_capabilities":[{"capability_id":"lenso.agent@1","descriptor_version":"1.1.0","cardinality":"one"},{"capability_id":"lenso.agent.tui-contribution@1","descriptor_version":"1.0.0","cardinality":"many"}]}"#
)]
fn instantiate(
    context: NativeModuleFactoryContext<'_>,
) -> Result<NativeModuleInstance, RuntimeFailure> {
    if context.entrypoint() != "default" || context.configuration() != "{}" {
        return Err(RuntimeFailure::InvalidResolvedPlan {
            detail: "TUI Shell Module requires entrypoint `default` and empty configuration"
                .to_owned(),
        });
    }
    Ok(NativeModuleInstance::default())
}
