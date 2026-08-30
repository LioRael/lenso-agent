//! Durable content-addressed Artifact Capability for Agent results.

#[allow(dead_code)]
mod contract;

include!("generated.rs");

#[cfg(test)]
mod tests {
    use futures::executor::block_on;
    use lenso_kernel::{CancellationToken, InvocationContext, NativeRequestFuture};

    use super::*;

    #[derive(Debug)]
    struct Fake;

    impl ArtifactProvider for Fake {
        fn put(&self, _: InvocationContext, _: PutRequest) -> NativeRequestFuture<ArtifactPut> {
            Box::pin(async move {
                Ok(Ok(PutResponse {
                    handle: format!("artifact://session/{}", "a".repeat(64)),
                    digest: format!("sha256:{}", "a".repeat(64)),
                    size: "5".to_owned(),
                }))
            })
        }

        fn read(
            &self,
            _: InvocationContext,
            request: ReadRequest,
        ) -> NativeRequestFuture<ArtifactRead> {
            Box::pin(async move {
                if request.max_bytes == 0 {
                    return Ok(Err(ReadError::InvalidRange));
                }
                Ok(Ok(ReadResponse {
                    data_base64: "aGVsbG8=".to_owned(),
                    total_size: "5".to_owned(),
                    next_offset: "5".to_owned(),
                    complete: true,
                }))
            })
        }
    }

    fn context() -> InvocationContext {
        InvocationContext::new(1, None, CancellationToken::new())
    }

    #[test]
    fn generated_provider_preserves_put_read_and_domain_error_channels() {
        let put = block_on(Fake.put(
            context(),
            PutRequest {
                session_id: "session".to_owned(),
                name: "result.txt".to_owned(),
                media_type: "text/plain".to_owned(),
                data_base64: "aGVsbG8=".to_owned(),
            },
        ))
        .unwrap()
        .unwrap();
        assert!(put.handle.starts_with("artifact://session/"));
        let error = block_on(Fake.read(
            context(),
            ReadRequest {
                handle: put.handle,
                offset: "0".to_owned(),
                max_bytes: 0,
            },
        ))
        .unwrap();
        assert_eq!(error, Err(ReadError::InvalidRange));
    }
}
