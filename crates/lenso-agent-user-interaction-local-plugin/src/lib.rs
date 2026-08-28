//! In-process user interaction broker Adapter for local Agent surfaces.

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    time::Duration,
};

use lenso::prelude::*;
use lenso_capability_agent_user_interaction::{
    self as interaction_contract, AnswerError, AnswerRequest, AnswerResponse, AskError, AskRequest,
    AskResponse, InteractiveSurface, PendingInteraction, PendingRequest, PendingResponse,
    UserInteractionProvider,
};
use lenso_kernel::{InvocationContext, RuntimeFailure};
use tokio::sync::oneshot;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalInteractionConfig {
    max_pending: usize,
    timeout_ms: u64,
}

fn validate_config(config: &LocalInteractionConfig) -> Result<(), RuntimeFailure> {
    if !(1..=16).contains(&config.max_pending) || !(1..=3_600_000).contains(&config.timeout_ms) {
        return Err(RuntimeFailure::InvalidResolvedPlan {
            detail: "User Interaction limits are invalid".to_owned(),
        });
    }
    Ok(())
}

#[derive(Debug)]
struct PendingEntry {
    request: AskRequest,
    sender: Option<oneshot::Sender<String>>,
}

type PendingState = Rc<RefCell<BTreeMap<String, PendingEntry>>>;

#[lenso::plugin(configuration_schema = "config.schema.json", validate = validate_config)]
#[derive(Clone, Debug)]
struct LocalUserInteractionPlugin {
    #[config]
    config: LocalInteractionConfig,
    pending: PendingState,
}

#[lenso::provides(interaction_contract::UserInteraction)]
impl UserInteractionProvider for LocalUserInteractionPlugin {
    fn ask(
        &self,
        context: InvocationContext,
        request: AskRequest,
    ) -> lenso_kernel::NativeRequestFuture<interaction_contract::UserInteractionAsk> {
        let config = self.config.clone();
        let pending = self.pending.clone();
        Box::pin(async move { ask(&config, &pending, context, request).await })
    }

    fn pending(
        &self,
        _: InvocationContext,
        _: PendingRequest,
    ) -> lenso_kernel::NativeRequestFuture<interaction_contract::UserInteractionPending> {
        let pending = self.pending.clone();
        Box::pin(async move {
            Ok(Ok(PendingResponse {
                interactions: pending
                    .borrow()
                    .values()
                    .map(|entry| PendingInteraction {
                        interaction_id: entry.request.interaction_id.clone(),
                        prompt: entry.request.prompt.clone(),
                        options: entry.request.options.clone(),
                        allow_freeform: entry.request.allow_freeform,
                    })
                    .collect(),
            }))
        })
    }

    fn answer(
        &self,
        _: InvocationContext,
        request: AnswerRequest,
    ) -> lenso_kernel::NativeRequestFuture<interaction_contract::UserInteractionAnswer> {
        let pending = self.pending.clone();
        Box::pin(async move { answer(&pending, request) })
    }
}

async fn ask(
    config: &LocalInteractionConfig,
    pending: &PendingState,
    context: InvocationContext,
    request: AskRequest,
) -> Result<Result<AskResponse, AskError>, RuntimeFailure> {
    let interactive = context
        .typed_extension::<InteractiveSurface>()
        .map_err(|error| RuntimeFailure::PluginFailure {
            detail: format!("User Interaction scope is invalid: {error}"),
        })?;
    if interactive.is_none() {
        return Ok(Err(AskError::Unavailable));
    }
    if !valid_request(&request) {
        return Ok(Err(AskError::InvalidRequest));
    }
    let (sender, receiver) = oneshot::channel();
    {
        let mut state = pending.borrow_mut();
        if state.len() >= config.max_pending {
            return Ok(Err(AskError::TooManyPending));
        }
        if state.contains_key(&request.interaction_id) {
            return Ok(Err(AskError::InvalidRequest));
        }
        state.insert(
            request.interaction_id.clone(),
            PendingEntry {
                request: request.clone(),
                sender: Some(sender),
            },
        );
    }
    let _guard = PendingGuard {
        interaction_id: request.interaction_id,
        pending: pending.clone(),
    };
    let cancellation = context.cancellation();
    tokio::select! {
        () = cancellation.cancelled() => Err(RuntimeFailure::Cancelled { request_id: context.request_id() }),
        () = tokio::time::sleep(Duration::from_millis(config.timeout_ms)) => Ok(Err(AskError::Timeout)),
        answer = receiver => match answer {
            Ok(answer) => Ok(Ok(AskResponse { answer })),
            Err(_) => Ok(Err(AskError::Unavailable)),
        }
    }
}

fn answer(
    pending: &PendingState,
    request: AnswerRequest,
) -> Result<Result<AnswerResponse, AnswerError>, RuntimeFailure> {
    let entry = {
        let mut state = pending.borrow_mut();
        let Some(entry) = state.get(&request.interaction_id) else {
            return Ok(Err(AnswerError::NotFound));
        };
        if !valid_answer(&entry.request, &request.answer) {
            return Ok(Err(AnswerError::InvalidAnswer));
        }
        state.remove(&request.interaction_id)
    };
    let Some(sender) = entry.and_then(|mut entry| entry.sender.take()) else {
        return Ok(Err(AnswerError::NotFound));
    };
    sender
        .send(request.answer)
        .map_err(|_| RuntimeFailure::PluginFailure {
            detail: "User Interaction receiver disappeared".to_owned(),
        })?;
    Ok(Ok(AnswerResponse {}))
}

fn valid_request(request: &AskRequest) -> bool {
    let options = request.options.iter().collect::<BTreeSet<_>>();
    !request.interaction_id.is_empty()
        && request.interaction_id.len() <= 128
        && !request.prompt.trim().is_empty()
        && request.prompt.len() <= 4096
        && request.options.len() <= 16
        && request
            .options
            .iter()
            .all(|option| !option.trim().is_empty() && option.len() <= 256)
        && options.len() == request.options.len()
        && (request.allow_freeform || !request.options.is_empty())
}

fn valid_answer(request: &AskRequest, answer: &str) -> bool {
    !answer.trim().is_empty()
        && answer.len() <= 4096
        && (request.allow_freeform || request.options.iter().any(|option| option == answer))
}

struct PendingGuard {
    interaction_id: String,
    pending: PendingState,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.pending.borrow_mut().remove(&self.interaction_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> LocalInteractionConfig {
        LocalInteractionConfig {
            max_pending: 2,
            timeout_ms: 1_000,
        }
    }

    fn request() -> AskRequest {
        AskRequest {
            interaction_id: "question-1".to_owned(),
            prompt: "Choose a mode".to_owned(),
            options: vec!["safe".to_owned(), "fast".to_owned()],
            allow_freeform: false,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn headless_context_rejects_questions_without_waiting() {
        let pending = PendingState::default();
        let response = ask(
            &config(),
            &pending,
            InvocationContext::new(1, None, lenso_kernel::CancellationToken::new()),
            request(),
        )
        .await
        .unwrap();
        assert_eq!(response, Err(AskError::Unavailable));
        assert!(pending.borrow().is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn interactive_question_is_visible_and_resumes_with_a_valid_answer() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let pending = PendingState::default();
                let ask_pending = pending.clone();
                let context =
                    InvocationContext::new(2, None, lenso_kernel::CancellationToken::new())
                        .with_typed_extension(&InteractiveSurface)
                        .unwrap();
                let task = tokio::task::spawn_local(async move {
                    ask(&config(), &ask_pending, context, request()).await
                });
                tokio::task::yield_now().await;

                {
                    let snapshot = pending.borrow();
                    assert_eq!(snapshot.len(), 1);
                    assert_eq!(snapshot["question-1"].request.prompt, "Choose a mode");
                }

                assert_eq!(
                    answer(
                        &pending,
                        AnswerRequest {
                            interaction_id: "question-1".to_owned(),
                            answer: "custom".to_owned(),
                        },
                    )
                    .unwrap(),
                    Err(AnswerError::InvalidAnswer)
                );
                assert_eq!(
                    answer(
                        &pending,
                        AnswerRequest {
                            interaction_id: "question-1".to_owned(),
                            answer: "safe".to_owned(),
                        },
                    )
                    .unwrap(),
                    Ok(AnswerResponse {})
                );
                assert_eq!(
                    task.await.unwrap().unwrap(),
                    Ok(AskResponse {
                        answer: "safe".to_owned()
                    })
                );
                assert!(pending.borrow().is_empty());
            })
            .await;
    }
}
