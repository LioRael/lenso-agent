//! Shared deterministic orchestration for lifecycle observers.

use lenso_kernel::{InvocationContext, RuntimeFailure};
use lenso_plugin_authoring::ManyPort;

use crate::{LifecycleClient, LifecycleInvocationError, ObserveRequest};

/// Delivers one typed lifecycle event to every observer in resolved Plan order.
pub async fn observe_all(
    observers: &ManyPort<LifecycleClient>,
    context: &InvocationContext,
    request: ObserveRequest,
) -> Result<(), RuntimeFailure> {
    for (index, observer) in observers.iter().enumerate() {
        observer
            .observe_with_context(context.clone(), request.clone())
            .await
            .map_err(|error| observer_failure(index, error))?;
    }
    Ok(())
}

fn observer_failure(index: usize, error: LifecycleInvocationError) -> RuntimeFailure {
    match error {
        LifecycleInvocationError::Runtime(error) => error,
        LifecycleInvocationError::Domain(_) => RuntimeFailure::PluginFailure {
            detail: format!("lifecycle observer {index} rejected the event"),
        },
    }
}
