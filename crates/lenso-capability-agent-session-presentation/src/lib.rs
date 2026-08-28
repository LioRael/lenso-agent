//! Portable Session presentation projection Capability for Agent Plugins.

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

    impl SessionPresentationProvider for ConformanceFake {
        fn project(
            &self,
            _: InvocationContext,
            request: ProjectRequest,
        ) -> NativeRequestFuture<SessionPresentation> {
            Box::pin(async move {
                Ok(Ok(ProjectResponse {
                    title: request
                        .current_title
                        .flatten()
                        .unwrap_or_else(|| "A title".to_owned()),
                    latest_preview: request.assistant_output,
                }))
            })
        }
    }

    #[test]
    fn third_party_provider_projects_without_session_storage_access() {
        let response = block_on(ConformanceFake.project(
            InvocationContext::new(1, None, CancellationToken::new()),
            ProjectRequest {
                session_id: "session-1".to_owned(),
                turn_id: "turn-1".to_owned(),
                user_input: "Design session titles".to_owned(),
                assistant_output: "Use a replaceable projection".to_owned(),
                current_title: Some(None),
            },
        ))
        .unwrap()
        .unwrap();
        assert_eq!(response.title, "A title");
        assert_eq!(response.latest_preview, "Use a replaceable projection");
    }
}
