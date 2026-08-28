//! Portable model-context compaction Capability.

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

    impl ContextCompactionProvider for ConformanceFake {
        fn compact(
            &self,
            _: InvocationContext,
            request: CompactRequest,
        ) -> NativeRequestFuture<ContextCompaction> {
            Box::pin(async move {
                Ok(Ok(CompactResponse {
                    summary: format!("{} message(s) compacted", request.messages.len()),
                    retained_messages: Vec::new(),
                }))
            })
        }
    }

    #[test]
    fn third_party_provider_implements_the_portable_contract_without_loop_internals() {
        let request = CompactRequest {
            session_id: "session-1".to_owned(),
            previous_summary: Some(None),
            messages: vec![
                ContextMessage {
                    role: ContextMessageRole::User,
                    content: "hello".to_owned(),
                },
                ContextMessage {
                    role: ContextMessageRole::Assistant,
                    content: "world".to_owned(),
                },
            ],
            target_summary_characters: 512,
        };
        let context = InvocationContext::new(1, None, CancellationToken::new());
        let response = block_on(ConformanceFake.compact(context, request))
            .unwrap()
            .unwrap();
        assert_eq!(response.summary, "2 message(s) compacted");
        assert!(response.retained_messages.is_empty());
    }
}
