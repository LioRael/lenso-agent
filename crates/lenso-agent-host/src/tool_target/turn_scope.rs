use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
};

use lenso_kernel::{InvocationContext, NativeRequestFuture, RuntimeFailure};
use uuid::Uuid;

use super::{AgentToolTarget, contract};

const SCOPE: &str = "lenso.agent.host.tool-target-turn";
const MAX_TURNS: usize = 1_024;
type Target = Option<Arc<dyn AgentToolTarget>>;

/// Native bridge custody only. Neither targets nor credentials are serialized.
pub(crate) struct TurnToolTargetRouter {
    target: Target,
    turns: Mutex<BTreeMap<Uuid, Target>>,
}

impl std::fmt::Debug for TurnToolTargetRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnToolTargetRouter")
            .finish_non_exhaustive()
    }
}

impl TurnToolTargetRouter {
    pub(crate) fn new(target: Target) -> Self {
        Self {
            target,
            turns: Mutex::new(BTreeMap::new()),
        }
    }

    pub(crate) fn capture(self: &Arc<Self>) -> Result<TurnToolTargetLease, String> {
        let target = match &self.target {
            Some(target) => target.snapshot_for_turn()?.or_else(|| Some(target.clone())),
            None => None,
        };
        let mut turns = self
            .turns
            .lock()
            .map_err(|_| "Tool target custody unavailable")?;
        if turns.len() >= MAX_TURNS {
            return Err("Too many admitted Tool target turns".into());
        }
        let id = Uuid::new_v4();
        turns.insert(id, target);
        Ok(TurnToolTargetLease {
            id,
            router: Arc::downgrade(self),
        })
    }

    fn resolve(&self, context: &InvocationContext) -> Result<Target, RuntimeFailure> {
        if context.is_cancelled() {
            return Err(RuntimeFailure::Cancelled {
                request_id: context.request_id(),
            });
        }
        let Some(bytes) = context.extension(SCOPE) else {
            // Non-Turn operator catalog requests retain their existing routing.
            return Ok(self.target.clone());
        };
        let id = Uuid::from_slice(bytes).map_err(|_| unavailable())?;
        self.turns
            .lock()
            .map_err(|_| unavailable())?
            .get(&id)
            .cloned()
            .ok_or_else(unavailable)
    }
}

fn unavailable() -> RuntimeFailure {
    RuntimeFailure::Unavailable {
        capability: contract::CAPABILITY_ID,
    }
}

impl AgentToolTarget for TurnToolTargetRouter {
    fn catalog(
        &self,
        context: InvocationContext,
        request: contract::CatalogRequest,
    ) -> NativeRequestFuture<contract::ToolTargetCatalog> {
        match self.resolve(&context) {
            Ok(Some(target)) => target.catalog(context, request),
            Ok(None) => Box::pin(async { Ok(Ok(contract::CatalogResponse { tools: Vec::new() })) }),
            Err(error) => Box::pin(async move { Err(error) }),
        }
    }

    fn execute(
        &self,
        context: InvocationContext,
        request: contract::ExecuteRequest,
    ) -> NativeRequestFuture<contract::ToolTargetExecute> {
        match self.resolve(&context) {
            Ok(Some(target)) => target.execute(context, request),
            Ok(None) => Box::pin(async { Ok(Err(contract::ExecuteError::TargetNotFound)) }),
            Err(error) => Box::pin(async move { Err(error) }),
        }
    }
}

pub(crate) struct TurnToolTargetLease {
    id: Uuid,
    router: Weak<TurnToolTargetRouter>,
}

impl std::fmt::Debug for TurnToolTargetLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TurnToolTargetLease")
            .finish_non_exhaustive()
    }
}

impl TurnToolTargetLease {
    pub(crate) fn attach(&self, context: InvocationContext) -> Result<InvocationContext, String> {
        context
            .with_extension(SCOPE, self.id.as_bytes().to_vec())
            .map_err(|_| "Cannot attach Tool target turn scope".into())
    }
}

impl Drop for TurnToolTargetLease {
    fn drop(&mut self) {
        if let Some(router) = self.router.upgrade() {
            // Even after poison, release held credentials without admitting more work.
            router
                .turns
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lenso_kernel::CancellationToken;

    struct Connection {
        credential: Mutex<Arc<str>>,
    }
    struct Snapshot(Arc<str>);
    impl std::fmt::Debug for Connection {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Connection")
        }
    }
    impl std::fmt::Debug for Snapshot {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Snapshot")
        }
    }
    impl AgentToolTarget for Connection {
        fn snapshot_for_turn(&self) -> Result<Option<Arc<dyn AgentToolTarget>>, String> {
            Ok(Some(Arc::new(Snapshot(
                self.credential.lock().unwrap().clone(),
            ))))
        }
        fn catalog(
            &self,
            _: InvocationContext,
            _: contract::CatalogRequest,
        ) -> NativeRequestFuture<contract::ToolTargetCatalog> {
            panic!("must use snapshot")
        }
        fn execute(
            &self,
            _: InvocationContext,
            _: contract::ExecuteRequest,
        ) -> NativeRequestFuture<contract::ToolTargetExecute> {
            panic!("must use snapshot")
        }
    }
    impl AgentToolTarget for Snapshot {
        fn catalog(
            &self,
            _: InvocationContext,
            _: contract::CatalogRequest,
        ) -> NativeRequestFuture<contract::ToolTargetCatalog> {
            Box::pin(async { Ok(Ok(contract::CatalogResponse { tools: vec![] })) })
        }
        fn execute(
            &self,
            context: InvocationContext,
            _: contract::ExecuteRequest,
        ) -> NativeRequestFuture<contract::ToolTargetExecute> {
            // Assert at the adapter seam; credentials never become the result.
            let expected = if context.request_id() == 1 {
                "first-secret"
            } else {
                "second-secret"
            };
            assert_eq!(self.0.as_ref(), expected);
            Box::pin(async { Ok(Err(contract::ExecuteError::TargetNotFound)) })
        }
    }
    fn context(id: u64) -> InvocationContext {
        InvocationContext::new(id, None, CancellationToken::new())
    }
    fn request() -> contract::ExecuteRequest {
        contract::ExecuteRequest {
            name: "projects_get_issue".into(),
            arguments_json: "{}".parse().unwrap(),
        }
    }

    #[tokio::test]
    async fn login_switch_before_first_call_preserves_admitted_identity() {
        let first: Arc<str> = Arc::from("first-secret");
        let weak = Arc::downgrade(&first);
        let connection = Arc::new(Connection {
            credential: Mutex::new(first),
        });
        let router = Arc::new(TurnToolTargetRouter::new(Some(connection.clone())));
        let admitted = router.capture().unwrap();
        *connection.credential.lock().unwrap() = Arc::from("second-secret");
        let next = router.capture().unwrap();
        let old_context = admitted.attach(context(1)).unwrap();
        assert!(!format!("{router:?} {admitted:?} {old_context:?}").contains("secret"));
        router
            .catalog(old_context.clone(), contract::CatalogRequest {})
            .await
            .unwrap()
            .unwrap();
        router
            .execute(old_context.clone(), request())
            .await
            .unwrap()
            .unwrap_err();
        router
            .execute(next.attach(context(2)).unwrap(), request())
            .await
            .unwrap()
            .unwrap_err();
        assert!(weak.upgrade().is_some());
        drop(admitted);
        assert!(weak.upgrade().is_none());
        assert!(matches!(
            router.execute(old_context, request()).await,
            Err(RuntimeFailure::Unavailable { .. })
        ));
        drop(next);
        assert!(router.turns.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn unknown_scope_and_cancellation_never_fall_back_to_current_connection() {
        let router = Arc::new(TurnToolTargetRouter::new(Some(Arc::new(Connection {
            credential: Mutex::new(Arc::from("secret")),
        }))));
        let foreign = Arc::new(TurnToolTargetRouter::new(None));
        let lease = foreign.capture().unwrap();
        for ctx in [
            lease.attach(context(1)).unwrap(),
            context(1).with_extension(SCOPE, vec![0]).unwrap(),
        ] {
            assert!(matches!(
                router.execute(ctx, request()).await,
                Err(RuntimeFailure::Unavailable { .. })
            ));
        }
        let lease = router.capture().unwrap();
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let ctx = lease
            .attach(InvocationContext::new(3, None, cancellation))
            .unwrap();
        assert!(matches!(
            router.execute(ctx, request()).await,
            Err(RuntimeFailure::Cancelled { request_id: 3 })
        ));
    }

    #[test]
    fn turn_custody_is_bounded_and_capacity_is_released() {
        let router = Arc::new(TurnToolTargetRouter::new(None));
        let mut leases = (0..MAX_TURNS)
            .map(|_| router.capture().unwrap())
            .collect::<Vec<_>>();
        assert!(router.capture().is_err());
        leases.pop();
        assert!(router.capture().is_ok());
        drop(leases);
        assert!(router.turns.lock().unwrap().is_empty());
    }
}
