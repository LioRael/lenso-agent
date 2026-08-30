use std::time::Duration;

use lenso_agent_default_plugins as default_plugins;
use lenso_agent_host::{AgentHost, Profile, TuiSurface};
use lenso_agent_tui_plugin as _;
use lenso_capability_agent::{RUN_TURN_OPERATION, RunTurnRequest, RunTurnResponseKind};
use lenso_capability_agent_user_interaction::InteractionAnswer;
use lenso_kernel::StreamEvent;

#[path = "../../../tests/support/mod.rs"]
mod support;

#[tokio::test(flavor = "current_thread")]
async fn tui_turn_may_select_an_admitted_model_without_rebuilding_the_generation() {
    let temporary = tempfile::tempdir().unwrap();
    tokio::task::LocalSet::new()
        .run_until(async {
            let host = AgentHost::builder()
                .agent_home(temporary.path())
                .unwrap()
                .plugins(default_plugins::link)
                .surface(TuiSurface::terminal())
                .build()
                .unwrap();
            let mut app = host
                .run(Profile::resolved_plan(support::plan_for_home(
                    "model-switch",
                    temporary.path(),
                )))
                .await
                .unwrap();
            let lease = app.lease_tui_turn().await.unwrap();
            assert!(!lease.supports_dynamic_model_selection());
            assert!(lease.invocation_context_for_model("auto").is_err());
            assert_eq!(
                lease.available_models(),
                vec![
                    "fixture/alternate-v1".to_owned(),
                    "fixture/readme-summary-v1".to_owned()
                ]
            );
            let stream = lease
                .handle()
                .open_with_context(
                    RUN_TURN_OPERATION,
                    lease
                        .invocation_context_for_model("fixture/alternate-v1")
                        .unwrap(),
                    RunTurnRequest {
                        input: "Answer directly: selected model".to_owned(),
                        session_id: None,
                    },
                )
                .await
                .unwrap()
                .unwrap();
            stream.close_send().await.unwrap();

            let mut session_id = None;
            loop {
                match stream.receive().await.unwrap() {
                    StreamEvent::Message(message) => {
                        session_id = session_id.or(message.session_id);
                    }
                    StreamEvent::Terminal(Ok(())) => break,
                    StreamEvent::Terminal(Err(error)) => panic!("Agent Turn failed: {error:?}"),
                    StreamEvent::PeerHalfClosed => {}
                }
            }
            let durable = lease
                .read_session(session_id.unwrap(), 0, 100)
                .await
                .unwrap();
            let requested = durable.events.iter().find(|event| {
                format!("{:?}", event.kind) == "ModelRequested"
                    && event.payload_json.as_str().contains("fixture/alternate-v1")
            });
            assert!(requested.is_some());

            drop(stream);
            drop(lease);
            app.shutdown().await.unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn tui_turn_may_resolve_a_dynamic_model_policy_before_turn_start() {
    let temporary = tempfile::tempdir().unwrap();
    tokio::task::LocalSet::new()
        .run_until(async {
            let host = AgentHost::builder()
                .agent_home(temporary.path())
                .unwrap()
                .plugins(default_plugins::link)
                .surface(TuiSurface::terminal())
                .build()
                .unwrap();
            let mut app = host
                .run(Profile::resolved_plan(support::plan_for_home(
                    "dynamic-model-selection",
                    temporary.path(),
                )))
                .await
                .unwrap();
            let lease = app.lease_tui_turn().await.unwrap();
            assert!(lease.supports_dynamic_model_selection());
            let stream = lease
                .handle()
                .open_with_context(
                    RUN_TURN_OPERATION,
                    lease.invocation_context_for_model("auto").unwrap(),
                    RunTurnRequest {
                        input: "Answer directly: selected model".to_owned(),
                        session_id: None,
                    },
                )
                .await
                .unwrap()
                .unwrap();
            stream.close_send().await.unwrap();

            let mut session_id = None;
            loop {
                match stream.receive().await.unwrap() {
                    StreamEvent::Message(message) => {
                        session_id = session_id.or(message.session_id);
                    }
                    StreamEvent::Terminal(Ok(())) => break,
                    StreamEvent::Terminal(Err(error)) => panic!("Agent Turn failed: {error:?}"),
                    StreamEvent::PeerHalfClosed => {}
                }
            }
            let durable = lease
                .read_session(session_id.unwrap(), 0, 100)
                .await
                .unwrap();
            let started = durable
                .events
                .iter()
                .find(|event| format!("{:?}", event.kind) == "TurnStarted")
                .expect("Turn start is durable");
            let payload: serde_json::Value =
                serde_json::from_str(started.payload_json.as_str()).unwrap();
            assert_eq!(
                payload["resolved_turn_profile"]["model"],
                "fixture/alternate-v1"
            );
            assert_eq!(payload["model_selection"]["policy"], "auto");
            assert_eq!(payload["model_selection"]["strategy"], "rules");
            assert_eq!(payload["model_selection"]["reason_code"], "strong_rule");

            drop(stream);
            drop(lease);
            app.shutdown().await.unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn tui_manual_compaction_uses_the_agent_session_control_transaction() {
    let temporary = tempfile::tempdir().unwrap();
    tokio::task::LocalSet::new()
        .run_until(async {
            let host = AgentHost::builder()
                .agent_home(temporary.path())
                .unwrap()
                .plugins(default_plugins::link)
                .surface(TuiSurface::terminal())
                .build()
                .unwrap();
            let mut app = host
                .run(Profile::resolved_plan(support::plan_for_home(
                    "base",
                    temporary.path(),
                )))
                .await
                .unwrap();
            let lease = app.lease_tui_turn().await.unwrap();
            let stream = lease
                .handle()
                .open_with_context(
                    RUN_TURN_OPERATION,
                    lease.invocation_context().unwrap(),
                    RunTurnRequest {
                        input: "Answer directly: preserve this context".to_owned(),
                        session_id: None,
                    },
                )
                .await
                .unwrap()
                .unwrap();
            stream.close_send().await.unwrap();

            let mut session_id = None;
            loop {
                match stream.receive().await.unwrap() {
                    StreamEvent::Message(message) => {
                        session_id = session_id.or(message.session_id);
                    }
                    StreamEvent::Terminal(Ok(())) => break,
                    StreamEvent::Terminal(Err(error)) => panic!("Agent Turn failed: {error:?}"),
                    StreamEvent::PeerHalfClosed => {}
                }
            }
            let session_id = session_id.expect("Turn must expose its Session identity");
            drop(stream);

            let compacted = lease.compact_session(session_id.clone()).await.unwrap();
            assert!(compacted.source_message_count >= 2);
            assert!(
                compacted.revision.parse::<u64>().unwrap()
                    > compacted.compacted_through_revision.parse::<u64>().unwrap()
            );
            let durable = lease.read_session(session_id, 0, 100).await.unwrap();
            let kinds = durable
                .events
                .iter()
                .map(|event| format!("{:?}", event.kind))
                .collect::<Vec<_>>();
            assert!(kinds.iter().any(|kind| kind == "ContextCompactionStarted"));
            assert!(
                kinds
                    .iter()
                    .any(|kind| kind == "ContextCompactionCommitted")
            );

            drop(lease);
            app.shutdown().await.unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn tui_answers_ask_user_through_the_same_generation() {
    let temporary = tempfile::tempdir().unwrap();
    tokio::task::LocalSet::new()
        .run_until(async {
            let host = AgentHost::builder()
                .agent_home(temporary.path())
                .unwrap()
                .plugins(default_plugins::link)
                .surface(TuiSurface::terminal())
                .build()
                .unwrap();
            let mut app = host
                .run(Profile::resolved_plan(support::plan_for_home(
                    "base",
                    temporary.path(),
                )))
                .await
                .unwrap();
            let lease = app.lease_tui_turn().await.unwrap();
            let stream = lease
                .handle()
                .open_with_context(
                    RUN_TURN_OPERATION,
                    lease.invocation_context().unwrap(),
                    RunTurnRequest {
                        input: "Ask me which mode to use.".to_owned(),
                        session_id: None,
                    },
                )
                .await
                .unwrap()
                .unwrap();
            stream.close_send().await.unwrap();

            loop {
                let event = tokio::time::timeout(Duration::from_secs(2), stream.receive())
                    .await
                    .expect("Agent should start the ask_user Tool")
                    .unwrap();
                if matches!(
                    event,
                    StreamEvent::Message(message)
                        if message.kind == Some(RunTurnResponseKind::ToolStarted)
                ) {
                    break;
                }
            }

            let question = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let pending = lease.pending_interactions().await.unwrap();
                    if let Some(question) = pending.into_iter().next() {
                        break question;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("ask_user should publish one pending question");
            assert_eq!(question.questions[0].prompt, "Which mode should I use?");
            assert_eq!(question.questions[0].options[0].label, "safe");
            assert_eq!(question.questions[0].options[1].label, "fast");
            assert_eq!(
                question.questions[0].options[0]
                    .preview
                    .as_ref()
                    .and_then(Option::as_deref),
                Some("mode = \"safe\"")
            );
            lease
                .answer_interaction(
                    question.interaction_id,
                    vec![InteractionAnswer {
                        question_id: "mode".to_owned(),
                        selected_option_ids: vec!["safe".to_owned()],
                        other: Some(None),
                    }],
                )
                .await
                .unwrap();

            let mut output = String::new();
            loop {
                match tokio::time::timeout(Duration::from_secs(2), stream.receive())
                    .await
                    .expect("Agent should resume after the answer")
                    .unwrap()
                {
                    StreamEvent::Message(message) if message.is_text_delta() => {
                        output.push_str(&message.text);
                    }
                    StreamEvent::Terminal(Ok(())) => break,
                    StreamEvent::Terminal(Err(error)) => panic!("Agent Turn failed: {error:?}"),
                    StreamEvent::Message(_) | StreamEvent::PeerHalfClosed => {}
                }
            }
            assert_eq!(output, "Selected mode: safe");

            drop(stream);
            drop(lease);
            app.shutdown().await.unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn answered_interaction_renews_the_turn_execution_budget() {
    let temporary = tempfile::tempdir().unwrap();
    tokio::task::LocalSet::new()
        .run_until(async {
            let host = AgentHost::builder()
                .agent_home(temporary.path())
                .unwrap()
                .plugins(default_plugins::link)
                .surface(TuiSurface::terminal())
                .build()
                .unwrap();
            let plan = support::plan_for_home("interaction-resume-budget", temporary.path());
            let mut app = host.run(Profile::resolved_plan(plan)).await.unwrap();
            let lease = app.lease_tui_turn().await.unwrap();
            let stream = lease
                .handle()
                .open_with_context(
                    RUN_TURN_OPERATION,
                    lease.invocation_context().unwrap(),
                    RunTurnRequest {
                        input: "Inspect before and after asking me which mode to use.".to_owned(),
                        session_id: None,
                    },
                )
                .await
                .unwrap()
                .unwrap();
            stream.close_send().await.unwrap();

            let question = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    tokio::select! {
                        event = stream.receive() => match event.unwrap() {
                            StreamEvent::Terminal(Ok(())) => {
                                panic!("Agent Turn completed before ask_user")
                            }
                            StreamEvent::Terminal(Err(error)) => {
                                panic!("Agent Turn failed before ask_user: {error:?}")
                            }
                            StreamEvent::Message(_) | StreamEvent::PeerHalfClosed => {}
                        },
                        () = tokio::time::sleep(Duration::from_millis(10)) => {
                            let pending = lease.pending_interactions().await.unwrap();
                            if let Some(question) = pending.into_iter().next() {
                                break question;
                            }
                        }
                    }
                }
            })
            .await
            .expect("ask_user should publish one pending question after earlier Tool calls");
            lease
                .answer_interaction(
                    question.interaction_id,
                    vec![InteractionAnswer {
                        question_id: "mode".to_owned(),
                        selected_option_ids: vec!["safe".to_owned()],
                        other: Some(None),
                    }],
                )
                .await
                .unwrap();

            let mut output = String::new();
            loop {
                match tokio::time::timeout(Duration::from_secs(2), stream.receive())
                    .await
                    .expect("Agent should resume after the answer")
                    .unwrap()
                {
                    StreamEvent::Message(message) if message.is_text_delta() => {
                        output.push_str(&message.text);
                    }
                    StreamEvent::Terminal(Ok(())) => break,
                    StreamEvent::Terminal(Err(error)) => panic!("Agent Turn failed: {error:?}"),
                    StreamEvent::Message(_) | StreamEvent::PeerHalfClosed => {}
                }
            }
            assert_eq!(output, "Interaction resume completed");

            drop(stream);
            drop(lease);
            app.shutdown().await.unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn answered_interaction_does_not_reset_the_total_tool_call_limit() {
    let temporary = tempfile::tempdir().unwrap();
    tokio::task::LocalSet::new()
        .run_until(async {
            let host = AgentHost::builder()
                .agent_home(temporary.path())
                .unwrap()
                .plugins(default_plugins::link)
                .surface(TuiSurface::terminal())
                .build()
                .unwrap();
            let plan = support::plan_for_home("interaction-total-tool-limit", temporary.path());
            let mut app = host.run(Profile::resolved_plan(plan)).await.unwrap();
            let lease = app.lease_tui_turn().await.unwrap();
            let stream = lease
                .handle()
                .open_with_context(
                    RUN_TURN_OPERATION,
                    lease.invocation_context().unwrap(),
                    RunTurnRequest {
                        input: "Inspect before and after asking me which mode to use.".to_owned(),
                        session_id: None,
                    },
                )
                .await
                .unwrap()
                .unwrap();
            stream.close_send().await.unwrap();

            let question = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    tokio::select! {
                        event = stream.receive() => match event.unwrap() {
                            StreamEvent::Terminal(Ok(())) => {
                                panic!("Agent Turn completed before ask_user")
                            }
                            StreamEvent::Terminal(Err(error)) => {
                                panic!("Agent Turn failed before ask_user: {error:?}")
                            }
                            StreamEvent::Message(_) | StreamEvent::PeerHalfClosed => {}
                        },
                        () = tokio::time::sleep(Duration::from_millis(10)) => {
                            let pending = lease.pending_interactions().await.unwrap();
                            if let Some(question) = pending.into_iter().next() {
                                break question;
                            }
                        }
                    }
                }
            })
            .await
            .expect("ask_user should publish one pending question at the total Tool limit");
            lease
                .answer_interaction(
                    question.interaction_id,
                    vec![InteractionAnswer {
                        question_id: "mode".to_owned(),
                        selected_option_ids: vec!["safe".to_owned()],
                        other: Some(None),
                    }],
                )
                .await
                .unwrap();

            let error = loop {
                match tokio::time::timeout(Duration::from_secs(2), stream.receive())
                    .await
                    .expect("Agent should settle after the answer")
                    .unwrap()
                {
                    StreamEvent::Terminal(Err(error)) => break error,
                    StreamEvent::Terminal(Ok(())) => {
                        panic!("Agent Turn ignored the total Tool-call limit")
                    }
                    StreamEvent::Message(_) | StreamEvent::PeerHalfClosed => {}
                }
            };
            assert!(format!("{error:?}").contains("ToolCallLimitExceeded"));

            drop(stream);
            drop(lease);
            app.shutdown().await.unwrap();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn explicit_zero_user_resume_limit_stops_after_the_answer() {
    let temporary = tempfile::tempdir().unwrap();
    tokio::task::LocalSet::new()
        .run_until(async {
            let host = AgentHost::builder()
                .agent_home(temporary.path())
                .unwrap()
                .plugins(default_plugins::link)
                .surface(TuiSurface::terminal())
                .build()
                .unwrap();
            let plan = support::plan_for_home("interaction-resume-limit", temporary.path());
            let mut app = host.run(Profile::resolved_plan(plan)).await.unwrap();
            let lease = app.lease_tui_turn().await.unwrap();
            let stream = lease
                .handle()
                .open_with_context(
                    RUN_TURN_OPERATION,
                    lease.invocation_context().unwrap(),
                    RunTurnRequest {
                        input: "Ask me which mode to use.".to_owned(),
                        session_id: None,
                    },
                )
                .await
                .unwrap()
                .unwrap();
            stream.close_send().await.unwrap();

            let question = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    tokio::select! {
                        event = stream.receive() => match event.unwrap() {
                            StreamEvent::Terminal(Ok(())) => {
                                panic!("Agent Turn completed before ask_user")
                            }
                            StreamEvent::Terminal(Err(error)) => {
                                panic!("Agent Turn failed before ask_user: {error:?}")
                            }
                            StreamEvent::Message(_) | StreamEvent::PeerHalfClosed => {}
                        },
                        () = tokio::time::sleep(Duration::from_millis(10)) => {
                            let pending = lease.pending_interactions().await.unwrap();
                            if let Some(question) = pending.into_iter().next() {
                                break question;
                            }
                        }
                    }
                }
            })
            .await
            .expect("ask_user should publish one pending question");
            lease
                .answer_interaction(
                    question.interaction_id,
                    vec![InteractionAnswer {
                        question_id: "mode".to_owned(),
                        selected_option_ids: vec!["safe".to_owned()],
                        other: Some(None),
                    }],
                )
                .await
                .unwrap();

            let error = loop {
                match tokio::time::timeout(Duration::from_secs(2), stream.receive())
                    .await
                    .expect("Agent should enforce the resume limit after the answer")
                    .unwrap()
                {
                    StreamEvent::Terminal(Err(error)) => break error,
                    StreamEvent::Terminal(Ok(())) => {
                        panic!("Agent Turn ignored max_user_resumes = 0")
                    }
                    StreamEvent::Message(_) | StreamEvent::PeerHalfClosed => {}
                }
            };
            assert!(format!("{error:?}").contains("StepLimitExceeded"));

            drop(stream);
            drop(lease);
            app.shutdown().await.unwrap();
        })
        .await;
}
