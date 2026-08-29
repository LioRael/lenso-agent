//! Portable active Agent Turn input Capability.

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

    impl TurnInputProvider for ConformanceFake {
        fn submit(
            &self,
            _: InvocationContext,
            request: SubmitRequest,
        ) -> NativeRequestFuture<TurnInput> {
            Box::pin(async move {
                Ok(Ok(SubmitResponse {
                    session_id: request.session_id,
                    accepted_revision: "4".to_owned(),
                }))
            })
        }
    }

    #[test]
    fn provider_acceptance_identifies_the_durable_session_revision() {
        let context = InvocationContext::new(1, None, CancellationToken::new());
        let response = block_on(ConformanceFake.submit(
            context,
            SubmitRequest {
                session_id: "session-1".to_owned(),
                input: "also check the tests".to_owned(),
            },
        ))
        .unwrap()
        .unwrap();

        assert_eq!(response.session_id, "session-1");
        assert_eq!(response.accepted_revision, "4");
    }

    #[test]
    fn domain_errors_and_unknown_codes_round_trip() {
        for error in [
            SubmitError::InputClosed,
            SubmitError::InvalidInput,
            SubmitError::TurnNotActive,
        ] {
            let wire = encode_submit_error(&error).unwrap();
            assert_eq!(decode_submit_error(&wire).unwrap(), error);
        }

        let wire = r#"{"code":"future_error","payload":{"retryable":true},"trace_id":"t-1"}"#;
        let error = decode_submit_error(wire).unwrap();
        let SubmitError::Unknown(unknown) = &error else {
            panic!("future domain code must remain unknown");
        };
        assert_eq!(unknown.code, "future_error");
        assert_eq!(
            unknown.payload,
            Some(serde_json::json!({"retryable": true}))
        );
        assert_eq!(
            unknown.extra.get("trace_id"),
            Some(&serde_json::json!("t-1"))
        );
        assert_eq!(
            decode_submit_error(&encode_submit_error(&error).unwrap()).unwrap(),
            error
        );
    }
}
