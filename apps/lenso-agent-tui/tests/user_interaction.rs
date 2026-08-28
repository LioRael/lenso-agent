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
async fn tui_answers_ask_user_through_the_same_generation() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let host = AgentHost::builder()
                .plugins(default_plugins::link)
                .surface(TuiSurface::terminal())
                .build()
                .unwrap();
            let mut app = host
                .run(Profile::resolved_plan(support::plan("base")))
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
