use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::{Trajectory, TrajectoryKind, TrajectoryStatus};

/// Deterministic acceptance criteria evaluated only from durable Session facts.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct EvaluationCriteria {
    pub require_status: Option<TrajectoryStatus>,
    pub max_duration_ms: Option<u64>,
    pub max_failed_operations: u32,
    pub max_tool_calls: Option<u32>,
    pub required_tools: Vec<String>,
}

impl Default for EvaluationCriteria {
    fn default() -> Self {
        Self {
            require_status: Some(TrajectoryStatus::Completed),
            max_duration_ms: None,
            max_failed_operations: 0,
            max_tool_calls: None,
            required_tools: Vec::new(),
        }
    }
}

/// One machine-readable evaluation outcome.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EvaluationCheck {
    pub id: String,
    pub passed: bool,
    pub expected: String,
    pub actual: String,
}

/// CI-ready report for one durable Session trajectory.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EvaluationReport {
    pub schema: String,
    pub session_id: String,
    pub trajectory_revision: u64,
    pub passed: bool,
    pub checks: Vec<EvaluationCheck>,
}

impl EvaluationReport {
    pub const SCHEMA: &'static str = "lenso.agent.evaluation@1";
}

/// Evaluates stable semantic records without re-executing model or Tool code.
pub fn evaluate_trajectory(
    trajectory: &Trajectory,
    criteria: &EvaluationCriteria,
) -> EvaluationReport {
    let mut checks = Vec::new();
    if let Some(expected) = criteria.require_status {
        checks.push(check(
            "status",
            trajectory.summary.status == expected,
            format!("{expected:?}").to_lowercase(),
            format!("{:?}", trajectory.summary.status).to_lowercase(),
        ));
    }
    checks.push(check(
        "failed_operations",
        trajectory.summary.failed_operations <= criteria.max_failed_operations,
        format!("<= {}", criteria.max_failed_operations),
        trajectory.summary.failed_operations.to_string(),
    ));
    if let Some(maximum) = criteria.max_duration_ms {
        let actual = trajectory.summary.duration_ms.unwrap_or(u64::MAX);
        checks.push(check(
            "duration_ms",
            actual <= maximum,
            format!("<= {maximum}"),
            trajectory
                .summary
                .duration_ms
                .map_or_else(|| "unavailable".to_owned(), |value| value.to_string()),
        ));
    }
    if let Some(maximum) = criteria.max_tool_calls {
        checks.push(check(
            "tool_calls",
            trajectory.summary.tool_calls <= maximum,
            format!("<= {maximum}"),
            trajectory.summary.tool_calls.to_string(),
        ));
    }
    let actual_tools = trajectory
        .records
        .iter()
        .filter(|record| record.kind == TrajectoryKind::Tool)
        .filter_map(|record| record.detail.tool_name.as_deref())
        .collect::<BTreeSet<_>>();
    for required in &criteria.required_tools {
        checks.push(check(
            &format!("required_tool:{required}"),
            actual_tools.contains(required.as_str()),
            "present".to_owned(),
            if actual_tools.contains(required.as_str()) {
                "present"
            } else {
                "missing"
            }
            .to_owned(),
        ));
    }
    EvaluationReport {
        schema: EvaluationReport::SCHEMA.to_owned(),
        session_id: trajectory.session_id.clone(),
        trajectory_revision: trajectory.revision,
        passed: checks.iter().all(|check| check.passed),
        checks,
    }
}

fn check(id: &str, passed: bool, expected: String, actual: String) -> EvaluationCheck {
    EvaluationCheck {
        id: id.to_owned(),
        passed,
        expected,
        actual,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TrajectoryDetail, TrajectoryRecord, TrajectorySummary};

    #[test]
    fn evaluation_fails_closed_on_missing_required_tool() {
        let trajectory = Trajectory {
            schema: Trajectory::SCHEMA.to_owned(),
            session_id: "session-1".to_owned(),
            revision: 3,
            summary: TrajectorySummary {
                status: TrajectoryStatus::Completed,
                turns: 1,
                model_calls: 1,
                tool_calls: 1,
                failed_operations: 0,
                input_tokens: 10,
                output_tokens: 5,
                started_at: None,
                updated_at: None,
                duration_ms: Some(10),
            },
            records: vec![TrajectoryRecord {
                id: "tool-1".to_owned(),
                turn: 1,
                kind: TrajectoryKind::Tool,
                status: TrajectoryStatus::Completed,
                label: "read".to_owned(),
                preview: String::new(),
                started_at: "2026-08-30T00:00:00Z".to_owned(),
                completed_at: Some("2026-08-30T00:00:00Z".to_owned()),
                duration_ms: Some(0),
                time_to_first_token_ms: None,
                step: None,
                input_tokens: None,
                output_tokens: None,
                detail: TrajectoryDetail {
                    tool_name: Some("read".to_owned()),
                    ..TrajectoryDetail::default()
                },
                source_event_ids: vec!["event-1".to_owned()],
            }],
        };
        let report = evaluate_trajectory(
            &trajectory,
            &EvaluationCriteria {
                required_tools: vec!["read".to_owned(), "search".to_owned()],
                ..EvaluationCriteria::default()
            },
        );
        assert!(!report.passed);
        assert_eq!(report.checks.last().unwrap().actual, "missing");
    }
}
