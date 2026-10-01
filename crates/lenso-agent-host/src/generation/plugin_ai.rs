//! Host-only lightweight Model access. No Session or Agent turn is opened.
//!
//! Ingress must verify actor realm/audience and Console policy before using this
//! lease. This module carries the assertion; it is not an authorization service.

use super::{
    AgentApp, DurableGenerationRoute, NativeApp, RetainedActor, attach_actor, control_error,
    selected_model_catalog, selected_surface_agent_provider,
};
use lenso_capability_agent_model::{
    COMPLETE_OPERATION, CatalogResponse, CompleteOpen, ModelComplete,
};
use lenso_kernel::{CancellationToken, InvocationContext, NativeStream, NativeStreamHandle};

/// Pins exact provider binding and identity without acquiring a history lease.
#[derive(Debug)]
pub struct CompletionGeneration {
    route: DurableGenerationRoute<NativeApp>,
    handle: NativeStreamHandle<ModelComplete>,
    catalog: CatalogResponse,
    actor: RetainedActor,
    provider: String,
}

impl AgentApp {
    /// Captures the active Generation's already selected Model provider.
    /// A verified Host-issued actor is mandatory; ingress verifies its audience.
    pub async fn lease_plugin_completion(
        &self,
        actor: lenso_auth_sdk::ActorAssertion,
    ) -> Result<CompletionGeneration, String> {
        let route = self.host.route().await.map_err(control_error)?;
        let plan = self
            .retained_generation_plan(route.generation_spec_digest())
            .ok_or_else(|| "active Generation has no retained Plan".to_owned())?;
        let consumer = selected_surface_agent_provider(&plan)?;
        let catalog = selected_model_catalog(&route, &consumer).await?;
        let handle = route
            .target()
            .stream_handle::<ModelComplete>(&consumer)
            .map_err(|error| format!("Generation has no Model completion binding: {error:?}"))?;
        let provider = route
            .target()
            .dependencies(&consumer)
            .map_err(|error| format!("Model binding unavailable: {error:?}"))?
            .bindings()
            .iter()
            .find(|binding| binding.capability_id() == lenso_capability_agent_model::CAPABILITY_ID)
            .map(|binding| binding.provider_instance().to_owned())
            .ok_or_else(|| "no exact Model provider binding".to_owned())?;
        Ok(CompletionGeneration {
            route,
            handle,
            catalog,
            actor: RetainedActor(actor),
            provider,
        })
    }
}

impl CompletionGeneration {
    pub fn generation_digest(&self) -> &str {
        self.route.generation_spec_digest()
    }

    pub fn provider_instance(&self) -> &str {
        &self.provider
    }

    /// Provider-declared hard input ceiling; missing limits cannot be priced.
    pub fn input_ceiling(&self, model: &str) -> Option<u64> {
        self.catalog
            .models
            .iter()
            .find(|entry| entry.id == model)?
            .limits
            .max_input_tokens
            .as_ref()?
            .as_ref()?
            .parse()
            .ok()
    }

    /// Keep this lease alive until the stream terminates. Cancellation is caller
    /// owned. Tools and continuation are excluded from lightweight completion.
    pub async fn complete(
        &self,
        request: CompleteOpen,
        cancellation: CancellationToken,
    ) -> Result<NativeStream<ModelComplete>, String> {
        validate_lightweight(
            &request,
            self.catalog.models.iter().map(|model| model.id.as_str()),
        )?;
        let request_id = super::NEXT_ROOT_REQUEST_ID
            .fetch_update(super::Ordering::Relaxed, super::Ordering::Relaxed, |id| {
                id.checked_add(1)
            })
            .map_err(|_| "completion request identity exhausted".to_owned())?;
        let context = InvocationContext::new(request_id, None, cancellation)
            .with_extension(
                super::GENERATION_SPEC_DIGEST_EXTENSION,
                self.generation_digest().as_bytes().to_vec(),
            )
            .map_err(|error| format!("cannot attach Generation: {error}"))?;
        let context = attach_actor(context, Some(&self.actor.0))?;
        self.handle
            .open_with_context(COMPLETE_OPERATION, context, request)
            .await
            .map_err(|error| format!("Model completion transport failed: {error:?}"))?
            .map_err(|error| format!("Model completion rejected: {error:?}"))
    }
}

fn validate_lightweight<'a>(
    request: &CompleteOpen,
    models: impl Iterator<Item = &'a str>,
) -> Result<(), String> {
    if !request.tools.is_empty() || request.continuation_scope.is_some() {
        return Err("lightweight completion forbids tools and continuation".to_owned());
    }
    if request.max_output_tokens <= 0 || !models.into_iter().any(|model| model == request.model) {
        return Err("completion requires positive output limit and exact catalog model".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn plugin_completion_fixture_does_not_create_session_history() {
        tokio::task::LocalSet::new()
            .run_until(Box::pin(async {
                let home = tempfile::tempdir().unwrap();
                for (plugin, file, config) in [
                    (
                        "lenso.agent.model.fixture",
                        "model.toml",
                        "model = \"fixture/readme-summary-v1\"\n",
                    ),
                    (
                        "lenso.agent.loop",
                        "agent.toml",
                        "model = \"fixture/readme-summary-v1\"\n",
                    ),
                ] {
                    let directory = home.path().join("plugins").join(plugin);
                    std::fs::create_dir_all(&directory).unwrap();
                    std::fs::write(directory.join(file), config).unwrap();
                }
                let host = crate::AgentHost::builder()
                    .plugins(lenso_agent_default_plugins::link)
                    .agent_home(home.path())
                    .unwrap()
                    .surface(crate::WebSurface::browser())
                    .build()
                    .unwrap();
                host.prepare_authoring().unwrap();
                let mut app = host.run(crate::Profile::Default).await.unwrap();
                let history = app.lease_web_history().await.unwrap();
                assert!(history.list_sessions(50).await.unwrap().sessions.is_empty());
                let issuer = lenso_auth_sdk::ActorAssertionIssuer::from_signing_key(
                    "synthetic.auth",
                    [9; 32],
                );
                let now = time::OffsetDateTime::now_utc();
                let actor = issuer.issue(
                    "synthetic-user",
                    "user",
                    "synthetic",
                    [lenso_auth_sdk::audience(
                        lenso_capability_agent_model::CAPABILITY_ID,
                        COMPLETE_OPERATION,
                    )],
                    lenso_auth_sdk::Validity::new(
                        now - time::Duration::seconds(1),
                        now + time::Duration::minutes(2),
                    )
                    .unwrap(),
                    std::collections::BTreeMap::new(),
                );
                let lease = app.lease_plugin_completion(actor).await.unwrap();
                let request: CompleteOpen = serde_json::from_value(serde_json::json!({
                "model":"fixture/readme-summary-v1",
                "messages":[{"role":"user","content":"What did you summarize?"}], "tools":[],
                    "temperature":0.0, "max_output_tokens":100
                }))
                .unwrap();
                let stream = lease
                    .complete(request, CancellationToken::new())
                    .await
                    .unwrap();
                let mut messages = 0;
                loop {
                    match stream.receive().await.unwrap() {
                        lenso_kernel::StreamEvent::Message(_) => messages += 1,
                        lenso_kernel::StreamEvent::PeerHalfClosed => {}
                        lenso_kernel::StreamEvent::Terminal(Ok(())) => break,
                        other @ lenso_kernel::StreamEvent::Terminal(_) => {
                            panic!("unexpected fixture event: {other:?}")
                        }
                    }
                }
                assert!(messages > 0);
                assert!(history.list_sessions(50).await.unwrap().sessions.is_empty());
                drop((stream, lease, history));
                app.shutdown().await.unwrap();
            }))
            .await;
    }

    #[test]
    fn plugin_completion_rejects_unknown_model_and_history_affinity() {
        let mut request: CompleteOpen = serde_json::from_value(serde_json::json!({
            "model":"synthetic", "messages":[], "tools":[],
            "temperature":0.0, "max_output_tokens":10
        }))
        .unwrap();
        assert!(validate_lightweight(&request, ["synthetic"].into_iter()).is_ok());
        assert!(validate_lightweight(&request, ["other"].into_iter()).is_err());
        request.continuation_scope = Some("assistant-session".into());
        assert!(validate_lightweight(&request, ["synthetic"].into_iter()).is_err());
        request.continuation_scope = None;
        request.max_output_tokens = 0;
        assert!(validate_lightweight(&request, ["synthetic"].into_iter()).is_err());
    }
}
