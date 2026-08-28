//! Portable curated-memory Capability for Agent Plugins.

#[allow(dead_code)]
mod contract;

include!("generated.rs");

#[cfg(test)]
mod tests {
    use futures::executor::block_on;
    use lenso_kernel::{CancellationToken, InvocationContext, NativeRequestFuture};

    use super::*;

    #[derive(Debug)]
    struct ConformanceFake;

    impl MemoryProvider for ConformanceFake {
        fn observe(
            &self,
            _: InvocationContext,
            request: ObserveRequest,
        ) -> NativeRequestFuture<MemoryObserve> {
            Box::pin(async move {
                Ok(Ok(ObserveResponse {
                    memory_ids: vec![format!(
                        "{}:{}",
                        request.source.session_id, request.source.turn_id
                    )],
                }))
            })
        }

        fn recall(
            &self,
            _: InvocationContext,
            request: RecallRequest,
        ) -> NativeRequestFuture<MemoryRecall> {
            Box::pin(async move {
                Ok(Ok(RecallResponse {
                    items: vec![MemoryItem {
                        memory_id: "remote-1".to_owned(),
                        content: request.query,
                        source: MemorySource {
                            session_id: "remote-session".to_owned(),
                            turn_id: "remote-turn".to_owned(),
                        },
                        confidence_milli: 900,
                    }],
                }))
            })
        }

        fn remember(
            &self,
            _: InvocationContext,
            _: RememberRequest,
        ) -> NativeRequestFuture<MemoryRemember> {
            Box::pin(async {
                Ok(Ok(RememberResponse {
                    memory_id: "remote-explicit".to_owned(),
                }))
            })
        }

        fn forget(
            &self,
            _: InvocationContext,
            request: ForgetRequest,
        ) -> NativeRequestFuture<MemoryForget> {
            Box::pin(async move {
                Ok(Ok(ForgetResponse {
                    forgotten: i64::try_from(request.memory_ids.len()).unwrap_or(i64::MAX),
                }))
            })
        }
    }

    #[test]
    fn third_party_provider_implements_memory_without_loop_or_sqlite_internals() {
        let context = || InvocationContext::new(1, None, CancellationToken::new());
        let recalled = block_on(ConformanceFake.recall(
            context(),
            RecallRequest {
                session_id: "session-1".to_owned(),
                query: "SQLite".to_owned(),
                max_items: 4,
                max_characters: 4096,
            },
        ))
        .unwrap()
        .unwrap();
        assert_eq!(recalled.items[0].content, "SQLite");

        let forgotten = block_on(ConformanceFake.forget(
            context(),
            ForgetRequest {
                memory_ids: vec!["remote-1".to_owned()],
            },
        ))
        .unwrap()
        .unwrap();
        assert_eq!(forgotten.forgotten, 1);
    }
}
