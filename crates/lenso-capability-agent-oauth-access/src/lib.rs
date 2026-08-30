//! Short-lived OAuth access Capability for remote Agent services.

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

    impl OauthAccessProvider for Fake {
        fn access(
            &self,
            _: InvocationContext,
            request: AccessRequest,
        ) -> NativeRequestFuture<OauthAccessAccess> {
            Box::pin(async move {
                if request.resource_uri.contains("unknown") {
                    return Ok(Err(AccessError::UnknownResource));
                }
                Ok(Ok(AccessResponse {
                    access_token: "secret-token".to_owned(),
                    token_type: "Bearer".to_owned(),
                    expires_at_millis: "1000".to_owned(),
                    scopes: request.scopes,
                }))
            })
        }

        fn invalidate(
            &self,
            _: InvocationContext,
            _: InvalidateRequest,
        ) -> NativeRequestFuture<OauthAccessInvalidate> {
            Box::pin(async move { Ok(Ok(InvalidateResponse { invalidated: true })) })
        }
    }

    fn context() -> InvocationContext {
        InvocationContext::new(1, None, CancellationToken::new())
    }

    #[test]
    fn generated_provider_preserves_access_and_domain_error_channels() {
        let access = block_on(Fake.access(
            context(),
            AccessRequest {
                resource_uri: "https://example.com/mcp".to_owned(),
                scopes: vec!["tools.read".to_owned()],
            },
        ))
        .unwrap()
        .unwrap();
        assert_eq!(access.scopes, ["tools.read"]);
        assert!(!format!("{access:?}").contains("secret-token"));
        let error = block_on(Fake.access(
            context(),
            AccessRequest {
                resource_uri: "https://unknown.example/mcp".to_owned(),
                scopes: Vec::new(),
            },
        ))
        .unwrap();
        assert_eq!(error, Err(AccessError::UnknownResource));
    }
}
