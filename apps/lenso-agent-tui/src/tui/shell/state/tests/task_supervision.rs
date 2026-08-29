use futures::{FutureExt, future};
use lenso_capability_agent_task_supervisor::SnapshotResponse as TaskSnapshotResponse;
use std::time::Instant;

use super::*;

#[test]
fn task_snapshot_projects_without_overwriting_a_plugin_panel() {
    let snapshot: TaskSnapshotResponse = serde_json::from_value(serde_json::json!({
        "tasks": [{
            "agent": "lenso.agent.loop/worker-a",
            "child_session_id": "child-session",
            "generation_spec_digest": "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            "owner": {
                "session_id": "parent-session",
                "tool_call_id": "tool-1",
                "turn_id": "turn-1"
            },
            "progress": {
                "content": "worker is checking tests",
                "content_truncated": false,
                "message_count": 3,
                "revision": 3,
                "text_delta_count": 1,
                "tool_call_count": 2
            },
            "status": "failed",
            "task_id": "task-1",
            "terminal_result": {
                "content": "tests failed",
                "content_truncated": false,
                "reason_code": "child_failed"
            },
            "workspace": "/tmp/child-worktrees/task-1"
        }]
    }))
    .unwrap();
    let plugin_panel = SnapshotResponsePanelsItem {
        body: "Plugin-owned body".to_owned(),
        id: "agent.tasks.supervisor".to_owned(),
        title: "Plugin-owned title".to_owned(),
    };
    let mut state = TuiState::new(&TuiOptions::default(), vec![plugin_panel]);

    apply_task_snapshot(&mut state, Ok(snapshot.clone()));
    apply_task_snapshot(&mut state, Ok(snapshot));

    assert_eq!(state.panels.len(), 1);
    assert_eq!(state.panels[0].body, "Plugin-owned body");
    assert_eq!(state.panel_count(), 2);
    let (title, body) = state.panel_at(1).expect("system Tasks panel");
    assert_eq!(title, "Tasks");
    assert!(body.contains("1 tasks · 0 active"));
    assert!(body.contains("task-1 · failed"));
    assert!(body.contains("lenso.agent.loop/worker-a"));
    assert!(body.contains("progress r3 · 3 messages · 2 tools"));
    assert!(body.contains("worker is checking tests"));
    assert!(body.contains("reason child_failed"));
}

#[test]
fn task_snapshot_failure_is_neutral_and_reported_once() {
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());

    apply_task_snapshot(&mut state, Err("route unavailable".to_owned()));
    apply_task_snapshot(&mut state, Err("route still unavailable".to_owned()));

    assert!(state.task_panel_body.contains("Retrying automatically"));
    assert!(!state.task_panel_body.contains("Generation"));
    assert_eq!(
        state
            .transcript
            .iter()
            .filter(|entry| matches!(entry, TranscriptEntry::Error { text } if text.contains("supervised tasks")))
            .count(),
        1
    );
}

#[tokio::test]
async fn pending_task_snapshot_keeps_ui_work_ready_and_stays_single_flight() {
    let state = TuiState::new(&TuiOptions::default(), Vec::new());
    let mut poll = TaskSnapshotPoll::new();
    assert!(poll.start_for_test(&state, future::pending().boxed_local()));

    let ui_completed_first = tokio::select! {
        _ = poll.receive() => false,
        () = future::ready(()) => true,
    };

    assert!(ui_completed_first);
    assert!(poll.is_in_flight());
    assert!(!poll.start_for_test(
        &state,
        future::ready(Ok(TaskSnapshotResponse { tasks: Vec::new() })).boxed_local()
    ));
}

#[test]
fn online_generation_change_invalidates_a_pending_idle_snapshot() {
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    apply_task_snapshot(&mut state, Ok(TaskSnapshotResponse { tasks: Vec::new() }));
    let mut poll = TaskSnapshotPoll::new();
    assert!(poll.start_for_test(&state, future::pending().boxed_local()));

    state.advance_task_generation_epoch();
    poll.reset_if_stale(&mut state);

    assert!(!poll.is_in_flight());
    assert!(poll.next_poll() <= Instant::now());
    assert_eq!(state.task_panel_body, "Loading supervised tasks…");
    assert_eq!(state.task_projection_scope, None);
}

#[test]
fn new_generation_starts_a_new_task_snapshot_error_episode() {
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    apply_task_snapshot(&mut state, Err("old route failed".to_owned()));

    state.advance_task_generation_epoch();
    assert!(state.reset_task_projection_if_stale());
    apply_task_snapshot(&mut state, Err("new route failed".to_owned()));

    assert_eq!(
        state
            .transcript
            .iter()
            .filter(|entry| matches!(entry, TranscriptEntry::Error { text } if text.contains("supervised tasks")))
            .count(),
        2
    );
}

#[tokio::test]
async fn queued_generation_change_fences_an_old_snapshot_error() {
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    let mut poll = TaskSnapshotPoll::new();
    assert!(poll.start_for_test(
        &state,
        future::ready(Err("old route failed".to_owned())).boxed_local()
    ));
    let result = poll.receive().await;

    // An online Switched/RolledBack/Failed event is drained before finish.
    state.advance_task_generation_epoch();
    poll.finish(&mut state, result);

    assert!(state.transcript.is_empty());
    assert_eq!(state.task_projection_scope, None);
}

#[test]
fn completed_turn_projection_does_not_leak_into_current_generation() {
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    state.task_panel_body = "old Turn tasks".to_owned();
    state.task_poll_status = PollStatus::ErrorReported;
    state.task_projection_scope = Some(TaskRouteScope::Turn(7));

    assert!(state.reset_task_projection_if_stale());

    assert_eq!(state.task_panel_body, "Loading supervised tasks…");
    assert_eq!(state.task_poll_status, PollStatus::Ready);
    assert_eq!(state.task_projection_scope, None);
}

#[test]
fn plugin_panel_refresh_preserves_the_system_tasks_selection() {
    let mut state = TuiState::new(
        &TuiOptions::default(),
        vec![SnapshotResponsePanelsItem {
            body: "old".to_owned(),
            id: "old".to_owned(),
            title: "Old".to_owned(),
        }],
    );
    state.selected_panel = state.panels.len();

    state.replace_plugin_panels(vec![
        SnapshotResponsePanelsItem {
            body: "first".to_owned(),
            id: "first".to_owned(),
            title: "First".to_owned(),
        },
        SnapshotResponsePanelsItem {
            body: "second".to_owned(),
            id: "second".to_owned(),
            title: "Second".to_owned(),
        },
    ]);

    assert_eq!(state.selected_panel, 2);
    assert_eq!(
        state.panel_at(state.selected_panel).map(|panel| panel.0),
        Some("Tasks")
    );
}

#[tokio::test]
async fn task_poll_intervals_start_when_each_snapshot_completes() {
    let mut state = TuiState::new(&TuiOptions::default(), Vec::new());
    let mut successful = TaskSnapshotPoll::new();
    assert!(successful.start_for_test(
        &state,
        future::ready(Ok(TaskSnapshotResponse { tasks: Vec::new() })).boxed_local()
    ));
    let result = successful.receive().await;
    let completed_at = Instant::now();
    successful.finish(&mut state, result);
    assert!(successful.next_poll() >= completed_at + TASK_POLL_INTERVAL);

    let mut failed = TaskSnapshotPoll::new();
    assert!(failed.start_for_test(
        &state,
        future::ready(Err("snapshot timed out".to_owned())).boxed_local()
    ));
    let result = failed.receive().await;
    let completed_at = Instant::now();
    failed.finish(&mut state, result);
    assert!(failed.next_poll() >= completed_at + TASK_POLL_ERROR_INTERVAL);

    state.advance_task_generation_epoch();
    failed.reset_if_stale(&mut state);
    assert!(failed.next_poll() <= Instant::now());
}
