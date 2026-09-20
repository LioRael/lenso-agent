//! Pure durable-task validation and recovery rules shared by task Providers.

use crate::{RecoveryAction, TaskSnapshot, TaskStatus};

/// One legal persisted state transition. Providers still own their own
/// storage, wake-up queue, external-effect ledger, and child-task policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskTransition {
    Start,
    WaitForSignal,
    Resume,
    Complete,
    Cancel,
    Timeout,
    MarkUncertainExternalEffect,
    RequireUpgrade,
}

/// Pure result of deciding whether an already persisted task can be recovered
/// by the currently selected Plugin artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryAssessment {
    Resume,
    Wait,
    ObserveOnly,
    BlockedUpgrade,
    BlockedUncertainExternalEffect,
}

/// Validates a start request before an owner-specific Provider writes state.
pub fn validate_start(
    task_id: &str,
    idempotency_key: &str,
    workflow_kind: &str,
    state_version: &str,
    state_json: &str,
    owner_instance: &str,
) -> Result<(), String> {
    validate_identity("task id", task_id, 128)?;
    validate_identity("idempotency key", idempotency_key, 192)?;
    validate_identity("workflow kind", workflow_kind, 192)?;
    validate_identity("state version", state_version, 64)?;
    validate_identity("owner instance", owner_instance, 256)?;
    validate_json("task state", state_json)?;
    Ok(())
}

/// Validates a durable record read from storage without executing it.
pub fn validate_snapshot(task: &TaskSnapshot) -> Result<(), String> {
    validate_identity("task id", &task.task_id, 128)?;
    validate_identity("owner instance", &task.owner_instance, 256)?;
    validate_identity("workflow kind", &task.workflow_kind, 192)?;
    validate_identity("state version", &task.state_version, 64)?;
    parse_decimal("task revision", &task.revision)?;
    validate_json("task state", task.state_json.as_str())?;
    if let Some(Some(parent)) = &task.parent_task_id {
        validate_identity("parent task id", parent, 128)?;
    }
    if let Some(Some(deadline)) = &task.deadline_unix_ms {
        parse_decimal("task deadline", deadline)?;
    }
    if let Some(Some(reason)) = &task.terminal_reason_code {
        validate_identity("task terminal reason", reason, 128)?;
    }
    Ok(())
}

/// Enforces transitions that make cancellation, stale approvals, uncertain
/// effects, and upgrade blocks observable instead of silently retrying work.
pub fn validate_transition(
    current: &TaskSnapshot,
    transition: TaskTransition,
) -> Result<TaskStatus, String> {
    use TaskStatus as Status;
    use TaskTransition as Transition;
    validate_snapshot(current)?;
    match (current.status.clone(), transition) {
        (Status::Ready, Transition::Start | Transition::Resume)
        | (Status::WaitingForSignal, Transition::Resume) => Ok(Status::Running),
        (Status::Running, Transition::WaitForSignal) => Ok(Status::WaitingForSignal),
        (Status::Running | Status::WaitingForSignal | Status::Ready, Transition::Complete) => {
            Ok(Status::Completed)
        }
        (Status::Ready | Status::Running | Status::WaitingForSignal, Transition::Cancel) => {
            Ok(Status::Cancelled)
        }
        (Status::Ready | Status::Running | Status::WaitingForSignal, Transition::Timeout) => {
            Ok(Status::TimedOut)
        }
        (
            Status::Ready | Status::Running | Status::WaitingForSignal,
            Transition::MarkUncertainExternalEffect,
        ) => Ok(Status::UncertainExternalEffect),
        (_, Transition::RequireUpgrade) => Ok(Status::UpgradeRequired),
        (Status::UncertainExternalEffect, Transition::Resume) => Err(
            "a task with an uncertain external effect cannot be resumed or retried automatically"
                .to_owned(),
        ),
        (status, _) => Err(format!(
            "transition {transition:?} is invalid from {status:?}"
        )),
    }
}

/// Returns the recovery action without invoking model calls, Tools, external
/// services, or arbitrary callback code.
#[must_use]
pub fn assess_recovery(
    task: &TaskSnapshot,
    supported_state_versions: &[String],
) -> RecoveryAssessment {
    if !supported_state_versions
        .iter()
        .any(|version| version == &task.state_version)
    {
        return RecoveryAssessment::BlockedUpgrade;
    }
    match task.status {
        TaskStatus::Ready | TaskStatus::Running => RecoveryAssessment::Resume,
        TaskStatus::WaitingForSignal => RecoveryAssessment::Wait,
        TaskStatus::UncertainExternalEffect => RecoveryAssessment::BlockedUncertainExternalEffect,
        TaskStatus::Completed
        | TaskStatus::Cancelled
        | TaskStatus::TimedOut
        | TaskStatus::UpgradeRequired => RecoveryAssessment::ObserveOnly,
    }
}

impl From<RecoveryAssessment> for RecoveryAction {
    fn from(value: RecoveryAssessment) -> Self {
        match value {
            RecoveryAssessment::Resume => Self::Resume,
            RecoveryAssessment::Wait => Self::Wait,
            RecoveryAssessment::ObserveOnly => Self::ObserveOnly,
            RecoveryAssessment::BlockedUpgrade => Self::BlockedUpgrade,
            RecoveryAssessment::BlockedUncertainExternalEffect => {
                Self::BlockedUncertainExternalEffect
            }
        }
    }
}

fn validate_identity(label: &str, value: &str, max: usize) -> Result<(), String> {
    if value.is_empty() || value.len() > max || value.chars().any(char::is_control) {
        return Err(format!(
            "durable task {label} is empty, too long, or contains control text"
        ));
    }
    Ok(())
}

fn validate_json(label: &str, value: &str) -> Result<(), String> {
    if value.len() > 65_536 {
        return Err(format!("durable task {label} exceeds its bounded size"));
    }
    serde_json::from_str::<serde_json::Value>(value)
        .map_err(|_| format!("durable task {label} is not valid JSON"))?;
    Ok(())
}

fn parse_decimal(label: &str, value: &str) -> Result<u64, String> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(format!("durable task {label} is not a decimal u64"));
    }
    value
        .parse::<u64>()
        .map_err(|_| format!("durable task {label} is not a decimal u64"))
}

#[cfg(test)]
mod tests {
    use super::{RecoveryAssessment, TaskTransition, assess_recovery, validate_transition};
    use crate::{RawJson, TaskSnapshot, TaskStatus};

    fn snapshot(status: TaskStatus, version: &str) -> TaskSnapshot {
        TaskSnapshot {
            task_id: "approval-1".to_owned(),
            owner_instance: "example.approval-workflow/default".to_owned(),
            workflow_kind: "example.approval-workflow@1".to_owned(),
            state_version: version.to_owned(),
            revision: "4".to_owned(),
            status,
            state_json: RawJson::new(r#"{"step":"waiting"}"#).unwrap(),
            parent_task_id: None,
            deadline_unix_ms: None,
            terminal_reason_code: None,
        }
    }

    #[test]
    fn uncertain_effects_are_not_retried_and_unknown_versions_block_recovery() {
        let uncertain = snapshot(TaskStatus::UncertainExternalEffect, "1");
        assert!(validate_transition(&uncertain, TaskTransition::Resume).is_err());
        assert_eq!(
            assess_recovery(&uncertain, &["1".to_owned()]),
            RecoveryAssessment::BlockedUncertainExternalEffect
        );
        assert_eq!(
            assess_recovery(
                &snapshot(TaskStatus::WaitingForSignal, "2"),
                &["1".to_owned()]
            ),
            RecoveryAssessment::BlockedUpgrade
        );
    }

    #[test]
    fn waiting_task_can_resume_but_cancelled_task_cannot() {
        let waiting = snapshot(TaskStatus::WaitingForSignal, "1");
        assert_eq!(
            validate_transition(&waiting, TaskTransition::Resume).unwrap(),
            TaskStatus::Running
        );
        let cancelled = snapshot(TaskStatus::Cancelled, "1");
        assert!(validate_transition(&cancelled, TaskTransition::Resume).is_err());
    }
}
