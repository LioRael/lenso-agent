//! Portable Session control Capability for Agent surfaces.

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

    impl SessionControlProvider for ConformanceFake {
        fn compact_session(
            &self,
            _: InvocationContext,
            _: CompactSessionRequest,
        ) -> NativeRequestFuture<SessionControl> {
            Box::pin(async move {
                Ok(Ok(CompactSessionResponse {
                    revision: "4".to_owned(),
                    compacted_through_revision: "2".to_owned(),
                    source_message_count: 2,
                }))
            })
        }
    }

    #[test]
    fn generated_client_and_provider_preserve_manual_compaction_result() {
        let response = block_on(ConformanceFake.compact_session(
            InvocationContext::new(1, None, CancellationToken::new()),
            CompactSessionRequest {
                session_id: "session-1".to_owned(),
            },
        ))
        .unwrap()
        .unwrap();
        assert_eq!(response.revision, "4");
        assert_eq!(response.compacted_through_revision, "2");
        assert_eq!(response.source_message_count, 2);
    }
}
