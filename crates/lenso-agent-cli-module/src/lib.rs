//! Endpoint-free CLI consumer Module.

use lenso_kernel::RuntimeFailure;
use lenso_native_adapter::{NativeModuleFactory, NativeModuleFactoryContext, NativeModuleInstance};

/// Runtime package identity selected by App Composition.
pub const PACKAGE_ID: &str = "lenso.agent.cli";
/// Exact linked package version.
pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Native factory for the endpoint-free CLI consumer identity.
#[derive(Clone, Debug, Default)]
pub struct CliModuleFactory;

impl NativeModuleFactory for CliModuleFactory {
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
        if context.entrypoint() != "default" || context.configuration() != "{}" {
            return Err(RuntimeFailure::InvalidResolvedPlan {
                detail: "CLI Module requires entrypoint `default` and empty configuration"
                    .to_owned(),
            });
        }
        Ok(NativeModuleInstance::default())
    }
}
