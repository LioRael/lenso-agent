//! In-process user interaction broker Adapter for local Agent surfaces.

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    time::Duration,
};

use lenso::prelude::*;
use lenso_auth_sdk::{
    ActorAssertion, ActorAssertionVerifier, ActorProjectionError, FixedClock, TypedActor,
};
use lenso_capability_agent_user_interaction::{
    self as interaction_contract, AnswerError, AnswerRequest, AnswerResponse, AskError, AskRequest,
    AskResponse, InteractionAnswer, InteractiveSurface, PendingInteraction, PendingRequest,
    PendingResponse, UserInteractionProvider,
};
use lenso_kernel::{InvocationContext, RuntimeFailure};
use tokio::sync::oneshot;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalInteractionConfig {
    max_pending: usize,
    timeout_ms: u64,
    #[serde(default)]
    authentication: Option<InteractionAuthentication>,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct InteractionAuthentication {
    issuer: String,
    verification_key: String,
}
struct InteractionOwner(String);
impl TypedActor for InteractionOwner {
    fn from_assertion(assertion: &ActorAssertion) -> Result<Self, ActorProjectionError> {
        if assertion.actor_kind() != "user" {
            return Err(ActorProjectionError::UnexpectedActorKind {
                expected: "user".into(),
                actual: assertion.actor_kind().into(),
            });
        }
        Ok(Self(
            serde_json::json!([assertion.issuer(), assertion.subject()]).to_string(),
        ))
    }
}
impl LocalInteractionConfig {
    fn owner(
        &self,
        context: &InvocationContext,
        operation: &str,
    ) -> Result<Option<String>, RuntimeFailure> {
        let fail = || RuntimeFailure::PluginFailure {
            detail: "User Interaction authentication required".into(),
        };
        let Some(auth) = &self.authentication else {
            if context
                .sealed_extension(lenso_auth_sdk::ACTOR_ASSERTION_EXTENSION)
                .is_some()
            {
                return Err(fail());
            }
            return Ok(None);
        };
        let verifier =
            ActorAssertionVerifier::from_public_key_base64(&auth.issuer, &auth.verification_key)
                .map_err(|_| fail())?;
        let owner = verifier
            .project_context::<InteractionOwner>(
                context,
                interaction_contract::CAPABILITY_ID,
                operation,
                &FixedClock::new(time::OffsetDateTime::now_utc()),
            )
            .map_err(|_| fail())?;
        Ok(Some(owner.0))
    }
}

fn validate_config(config: &LocalInteractionConfig) -> Result<(), RuntimeFailure> {
    if !(1..=16).contains(&config.max_pending) || !(1..=3_600_000).contains(&config.timeout_ms) {
        return Err(RuntimeFailure::InvalidResolvedPlan {
            detail: "User Interaction limits are invalid".to_owned(),
        });
    }
    if let Some(auth) = &config.authentication
        && (auth.issuer.is_empty()
            || ActorAssertionVerifier::from_public_key_base64(&auth.issuer, &auth.verification_key)
                .is_err())
    {
        return Err(RuntimeFailure::InvalidResolvedPlan {
            detail: "User Interaction assertion configuration is invalid".into(),
        });
    }
    Ok(())
}

#[derive(Debug)]
struct PendingEntry {
    request: AskRequest,
    sender: Option<oneshot::Sender<Vec<InteractionAnswer>>>,
}

type PendingKey = (Option<String>, String);
type PendingState = Rc<RefCell<BTreeMap<PendingKey, PendingEntry>>>;

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
        context: InvocationContext,
        _: PendingRequest,
    ) -> lenso_kernel::NativeRequestFuture<interaction_contract::UserInteractionPending> {
        let pending = self.pending.clone();
        let owner = self.config.owner(&context, "pending");
        Box::pin(async move {
            let Ok(owner) = owner else {
                return Ok(Err(interaction_contract::PendingError::PermissionDenied));
            };
            Ok(Ok(PendingResponse {
                interactions: pending
                    .borrow()
                    .iter()
                    .filter(|((entry_owner, _), _)| entry_owner == &owner)
                    .map(|(_, entry)| PendingInteraction {
                        interaction_id: entry.request.interaction_id.clone(),
                        questions: entry.request.questions.clone(),
                    })
                    .collect(),
            }))
        })
    }

    fn answer(
        &self,
        context: InvocationContext,
        request: AnswerRequest,
    ) -> lenso_kernel::NativeRequestFuture<interaction_contract::UserInteractionAnswer> {
        let pending = self.pending.clone();
        let owner = self.config.owner(&context, "answer");
        Box::pin(async move {
            match owner {
                Ok(owner) => answer(&pending, owner, request),
                Err(_) => Ok(Err(AnswerError::PermissionDenied)),
            }
        })
    }
}

async fn ask(
    config: &LocalInteractionConfig,
    pending: &PendingState,
    context: InvocationContext,
    request: AskRequest,
) -> Result<Result<AskResponse, AskError>, RuntimeFailure> {
    let Ok(owner) = config.owner(&context, "ask") else {
        return Ok(Err(AskError::PermissionDenied));
    };
    let key = (owner, request.interaction_id.clone());
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
        if state.contains_key(&key) {
            return Ok(Err(AskError::InvalidRequest));
        }
        state.insert(
            key.clone(),
            PendingEntry {
                request: request.clone(),
                sender: Some(sender),
            },
        );
    }
    let _guard = PendingGuard {
        key,
        pending: pending.clone(),
    };
    let cancellation = context.cancellation();
    tokio::select! {
        () = cancellation.cancelled() => Err(RuntimeFailure::Cancelled { request_id: context.request_id() }),
        () = tokio::time::sleep(Duration::from_millis(config.timeout_ms)) => Ok(Err(AskError::Timeout)),
        answers = receiver => match answers {
            Ok(answers) => Ok(Ok(AskResponse { answers })),
            Err(_) => Ok(Err(AskError::Unavailable)),
        }
    }
}

fn answer(
    pending: &PendingState,
    owner: Option<String>,
    request: AnswerRequest,
) -> Result<Result<AnswerResponse, AnswerError>, RuntimeFailure> {
    let key = (owner, request.interaction_id.clone());
    let entry = {
        let mut state = pending.borrow_mut();
        let Some(entry) = state.get(&key) else {
            return Ok(Err(AnswerError::NotFound));
        };
        if !valid_answers(&entry.request, &request.answers) {
            return Ok(Err(AnswerError::InvalidAnswer));
        }
        state.remove(&key)
    };
    let Some(sender) = entry.and_then(|mut entry| entry.sender.take()) else {
        return Ok(Err(AnswerError::NotFound));
    };
    sender
        .send(request.answers)
        .map_err(|_| RuntimeFailure::PluginFailure {
            detail: "User Interaction receiver disappeared".to_owned(),
        })?;
    Ok(Ok(AnswerResponse {}))
}

fn valid_request(request: &AskRequest) -> bool {
    let question_ids = request
        .questions
        .iter()
        .map(|question| question.question_id.as_str())
        .collect::<BTreeSet<_>>();
    !request.interaction_id.is_empty()
        && request.interaction_id.len() <= 128
        && !request.questions.is_empty()
        && request.questions.len() <= 8
        && question_ids.len() == request.questions.len()
        && request.questions.iter().all(|question| {
            let option_ids = question
                .options
                .iter()
                .map(|option| option.option_id.as_str())
                .collect::<BTreeSet<_>>();
            !question.question_id.trim().is_empty()
                && question.question_id.len() <= 128
                && !question.header.trim().is_empty()
                && question.header.len() <= 64
                && !question.prompt.trim().is_empty()
                && question.prompt.len() <= 4096
                && question.options.len() <= 16
                && option_ids.len() == question.options.len()
                && question.options.iter().all(|option| {
                    !option.option_id.trim().is_empty()
                        && option.option_id.len() <= 128
                        && !option.label.trim().is_empty()
                        && option.label.len() <= 256
                        && option.description.len() <= 1024
                        && option
                            .preview
                            .as_ref()
                            .and_then(Option::as_ref)
                            .is_none_or(|preview| !question.multi_select && preview.len() <= 16_384)
                })
        })
}

fn valid_answers(request: &AskRequest, answers: &[InteractionAnswer]) -> bool {
    let by_question = answers
        .iter()
        .map(|answer| (answer.question_id.as_str(), answer))
        .collect::<BTreeMap<_, _>>();
    answers.len() == request.questions.len()
        && by_question.len() == answers.len()
        && request.questions.iter().all(|question| {
            let Some(answer) = by_question.get(question.question_id.as_str()) else {
                return false;
            };
            let selected = answer.selected_option_ids.iter().collect::<BTreeSet<_>>();
            let other = answer
                .other
                .as_ref()
                .and_then(Option::as_deref)
                .filter(|other| !other.trim().is_empty() && other.len() <= 4096);
            let choice_count = selected.len() + usize::from(other.is_some());
            selected.len() == answer.selected_option_ids.len()
                && choice_count > 0
                && (question.multi_select || choice_count == 1)
                && answer.selected_option_ids.iter().all(|option_id| {
                    question
                        .options
                        .iter()
                        .any(|option| option.option_id == *option_id)
                })
                && answer.other.as_ref().and_then(Option::as_ref).is_none() == other.is_none()
        })
}

struct PendingGuard {
    key: PendingKey,
    pending: PendingState,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.pending.borrow_mut().remove(&self.key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lenso_capability_agent_user_interaction::{InteractionOption, InteractionQuestion};

    fn config() -> LocalInteractionConfig {
        LocalInteractionConfig {
            max_pending: 2,
            timeout_ms: 1_000,
            authentication: None,
        }
    }

    fn request() -> AskRequest {
        AskRequest {
            interaction_id: "question-1".to_owned(),
            questions: vec![InteractionQuestion {
                question_id: "mode".to_owned(),
                header: "Mode".to_owned(),
                prompt: "Choose a mode".to_owned(),
                options: ["safe", "fast"]
                    .into_iter()
                    .map(|label| InteractionOption {
                        option_id: label.to_owned(),
                        label: label.to_owned(),
                        description: String::new(),
                        preview: Some(None),
                    })
                    .collect(),
                multi_select: false,
            }],
        }
    }

    #[tokio::test(flavor = "current_thread")]
    #[allow(
        clippy::too_many_lines,
        reason = "one broker lifecycle proves simultaneous users cannot consume each other answers"
    )]
    async fn members_cannot_observe_or_answer_each_others_pending_questions() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let issuer =
                    lenso_auth_sdk::ActorAssertionIssuer::from_signing_key("test.auth", [7; 32]);
                let plugin = LocalUserInteractionPlugin {
                    config: LocalInteractionConfig {
                        authentication: Some(InteractionAuthentication {
                            issuer: "test.auth".into(),
                            verification_key: issuer.public_key_base64(),
                        }),
                        ..config()
                    },
                    pending: PendingState::default(),
                };
                let identity = |subject: &str, operation: &str| {
                    let now = time::OffsetDateTime::now_utc();
                    issuer
                        .issue(
                            subject,
                            "user",
                            "password",
                            vec![lenso_auth_sdk::audience(
                                interaction_contract::CAPABILITY_ID,
                                operation,
                            )],
                            lenso_auth_sdk::Validity::new(
                                now - time::Duration::seconds(1),
                                now + time::Duration::minutes(2),
                            )
                            .unwrap(),
                            BTreeMap::new(),
                        )
                        .attach(
                            InvocationContext::new(1, None, lenso_kernel::CancellationToken::new())
                                .with_typed_extension(&InteractiveSurface)
                                .unwrap(),
                        )
                        .unwrap()
                };
                let alice_task =
                    tokio::task::spawn_local(plugin.ask(identity("alice", "ask"), request()));
                tokio::task::yield_now().await;
                assert!(
                    plugin
                        .pending(identity("bob", "pending"), PendingRequest {})
                        .await
                        .unwrap()
                        .unwrap()
                        .interactions
                        .is_empty()
                );
                let response = || AnswerRequest {
                    interaction_id: "question-1".into(),
                    answers: vec![InteractionAnswer {
                        question_id: "mode".into(),
                        selected_option_ids: vec!["safe".into()],
                        other: Some(None),
                    }],
                };
                assert_eq!(
                    plugin
                        .answer(identity("bob", "answer"), response())
                        .await
                        .unwrap(),
                    Err(AnswerError::NotFound)
                );
                assert_eq!(
                    plugin
                        .pending(identity("alice", "pending"), PendingRequest {})
                        .await
                        .unwrap()
                        .unwrap()
                        .interactions
                        .len(),
                    1
                );
                // The same browser-generated interaction ID is valid for a different owner.
                let bob_task =
                    tokio::task::spawn_local(plugin.ask(identity("bob", "ask"), request()));
                tokio::task::yield_now().await;
                plugin
                    .answer(identity("bob", "answer"), response())
                    .await
                    .unwrap()
                    .unwrap();
                bob_task.await.unwrap().unwrap().unwrap();
                assert_eq!(
                    plugin
                        .pending(identity("alice", "pending"), PendingRequest {})
                        .await
                        .unwrap()
                        .unwrap()
                        .interactions
                        .len(),
                    1
                );
                plugin
                    .answer(identity("alice", "answer"), response())
                    .await
                    .unwrap()
                    .unwrap();
                alice_task.await.unwrap().unwrap().unwrap();
                assert!(plugin.pending.borrow().is_empty());
            })
            .await;
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
                    assert_eq!(
                        snapshot[&(None, "question-1".to_owned())].request.questions[0].prompt,
                        "Choose a mode"
                    );
                }

                assert_eq!(
                    answer(
                        &pending,
                        None,
                        AnswerRequest {
                            interaction_id: "question-1".to_owned(),
                            answers: vec![InteractionAnswer {
                                question_id: "mode".to_owned(),
                                selected_option_ids: vec!["missing".to_owned()],
                                other: Some(None),
                            }],
                        },
                    )
                    .unwrap(),
                    Err(AnswerError::InvalidAnswer)
                );
                assert_eq!(
                    answer(
                        &pending,
                        None,
                        AnswerRequest {
                            interaction_id: "question-1".to_owned(),
                            answers: vec![InteractionAnswer {
                                question_id: "mode".to_owned(),
                                selected_option_ids: vec!["safe".to_owned()],
                                other: Some(None),
                            }],
                        },
                    )
                    .unwrap(),
                    Ok(AnswerResponse {})
                );
                assert_eq!(
                    task.await.unwrap().unwrap(),
                    Ok(AskResponse {
                        answers: vec![InteractionAnswer {
                            question_id: "mode".to_owned(),
                            selected_option_ids: vec!["safe".to_owned()],
                            other: Some(None),
                        }]
                    })
                );
                assert!(pending.borrow().is_empty());
            })
            .await;
    }
}
