//! Non-blocking projection of supervised child tasks into one stable TUI panel.

use futures::{
    FutureExt,
    future::{LocalBoxFuture, pending},
};
use std::time::{Duration, Instant};

use super::{AgentApp, PollStatus, TaskRouteScope, TranscriptEntry, TuiState};
use crate::tui::shell::text::truncate_text;
use lenso_capability_agent_task_supervisor::{
    SnapshotResponse as TaskSnapshotResponse, TaskStatus,
};

pub(super) const TASK_POLL_INTERVAL: Duration = Duration::from_secs(1);
pub(super) const TASK_POLL_ERROR_INTERVAL: Duration = Duration::from_secs(2);
const MAX_VISIBLE_TASKS: usize = 8;

pub(super) struct TaskSnapshotPoll<'app> {
    in_flight: Option<LocalBoxFuture<'app, Result<TaskSnapshotResponse, String>>>,
    scope: Option<TaskRouteScope>,
    next_poll: Instant,
}

impl<'app> TaskSnapshotPoll<'app> {
    pub(super) fn new() -> Self {
        Self {
            in_flight: None,
            scope: None,
            next_poll: Instant::now(),
        }
    }

    pub(super) fn reset_if_stale(&mut self, state: &mut TuiState) {
        let projection_was_stale = state.reset_task_projection_if_stale();
        let poll_was_stale = self
            .scope
            .is_some_and(|scope| scope != state.current_task_scope());
        if poll_was_stale {
            self.in_flight = None;
            self.scope = None;
        }
        if projection_was_stale || poll_was_stale {
            self.next_poll = Instant::now();
        }
    }

    pub(super) fn start_if_due(&mut self, app: &'app AgentApp, state: &TuiState) {
        if self.in_flight.is_some() || Instant::now() < self.next_poll {
            return;
        }
        let scope = state.current_task_scope();
        let future = if let Some(active) = state.active.as_ref() {
            let lease = active.lease.clone();
            async move { lease.task_snapshot().await }.boxed_local()
        } else {
            async move { app.tui_task_snapshot().await }.boxed_local()
        };
        self.scope = Some(scope);
        self.in_flight = Some(future);
    }

    #[cfg(test)]
    pub(super) const fn is_in_flight(&self) -> bool {
        self.in_flight.is_some()
    }

    pub(super) async fn receive(&mut self) -> Result<TaskSnapshotResponse, String> {
        match self.in_flight.as_mut() {
            Some(future) => future.await,
            None => pending().await,
        }
    }

    pub(super) fn finish(
        &mut self,
        state: &mut TuiState,
        result: Result<TaskSnapshotResponse, String>,
    ) {
        let interval = if result.is_err() {
            TASK_POLL_ERROR_INTERVAL
        } else {
            TASK_POLL_INTERVAL
        };
        if self
            .scope
            .is_some_and(|scope| scope == state.current_task_scope())
        {
            apply_task_snapshot(state, result);
            self.next_poll = Instant::now() + interval;
        } else {
            self.next_poll = Instant::now();
        }
        self.in_flight = None;
        self.scope = None;
    }

    #[cfg(test)]
    pub(super) fn start_for_test(
        &mut self,
        state: &TuiState,
        future: LocalBoxFuture<'app, Result<TaskSnapshotResponse, String>>,
    ) -> bool {
        if self.in_flight.is_some() {
            return false;
        }
        self.scope = Some(state.current_task_scope());
        self.in_flight = Some(future);
        true
    }

    #[cfg(test)]
    pub(super) const fn next_poll(&self) -> Instant {
        self.next_poll
    }
}

pub(super) fn apply_task_snapshot(
    state: &mut TuiState,
    result: Result<TaskSnapshotResponse, String>,
) {
    let scope = state.current_task_scope();
    let body = match result {
        Ok(snapshot) => {
            state.task_poll_status = PollStatus::Ready;
            task_panel_body(&snapshot)
        }
        Err(error) => {
            if state.task_poll_status == PollStatus::Ready {
                state.transcript.push(TranscriptEntry::Error {
                    text: format!("Could not read supervised tasks: {error}"),
                });
                state.task_poll_status = PollStatus::ErrorReported;
            }
            "Task supervision unavailable\nRetrying automatically.".to_owned()
        }
    };
    state.task_panel_body = body;
    state.task_projection_scope = Some(scope);
}

fn task_panel_body(snapshot: &TaskSnapshotResponse) -> String {
    if snapshot.tasks.is_empty() {
        return "No supervised child tasks in this Generation.".to_owned();
    }
    let running = snapshot
        .tasks
        .iter()
        .filter(|task| {
            matches!(
                task.status,
                TaskStatus::Running | TaskStatus::CancellationRequested
            )
        })
        .count();
    let mut lines = vec![format!("{} tasks · {running} active", snapshot.tasks.len())];
    for task in snapshot.tasks.iter().take(MAX_VISIBLE_TASKS) {
        let (symbol, status) = match task.status {
            TaskStatus::Running => ("●", "running"),
            TaskStatus::CancellationRequested => ("◐", "cancelling"),
            TaskStatus::Completed => ("✓", "completed"),
            TaskStatus::Failed => ("×", "failed"),
            TaskStatus::Cancelled => ("○", "cancelled"),
        };
        lines.push(format!(
            "\n{symbol} {} · {status}",
            truncate_text(&task.task_id, 54)
        ));
        lines.push(format!(
            "  {} · {}",
            truncate_text(&task.agent, 30),
            truncate_text(&task.workspace, 46)
        ));
        lines.push(format!(
            "  owner {}/{} · gen {}",
            truncate_text(&task.owner.session_id, 16),
            truncate_text(&task.owner.turn_id, 16),
            task.generation_spec_digest
                .chars()
                .skip(7)
                .take(8)
                .collect::<String>()
        ));
        if let Some(progress) = task.progress.as_ref().and_then(Option::as_ref) {
            lines.push(format!(
                "  progress r{} · {} messages · {} tools",
                progress.revision, progress.message_count, progress.tool_call_count
            ));
            if !progress.content.is_empty() {
                lines.push(format!("  {}", truncate_text(&progress.content, 72)));
            }
        }
        if let Some(result) = task.terminal_result.as_ref().and_then(Option::as_ref)
            && let Some(reason) = result.reason_code.as_ref().and_then(Option::as_ref)
        {
            lines.push(format!("  reason {}", truncate_text(reason, 48)));
        }
    }
    let hidden = snapshot.tasks.len().saturating_sub(MAX_VISIBLE_TASKS);
    if hidden > 0 {
        lines.push(format!("\n+ {hidden} more tasks"));
    }
    lines.join("\n")
}
