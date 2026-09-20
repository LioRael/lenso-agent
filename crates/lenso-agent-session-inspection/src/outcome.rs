use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    EvaluationCheck, Trajectory, TrajectoryStatus, WorkspaceSnapshot, valid_session_id,
    validate_digest, validate_workspace_path,
};

/// The task family represented by an outcome evaluation fixture or run.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeTaskKind {
    BugFix,
    Refactor,
    ReadOnlyAnalysis,
    ApprovalFlow,
}

/// Versioned acceptance criteria for one outcome-oriented Agent task.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct OutcomeEvaluationCriteria {
    pub schema: String,
    pub task_kind: OutcomeTaskKind,
    pub terminal_outcome: TrajectoryStatus,
    pub workspace: WorkspaceCriteria,
    #[serde(default)]
    pub tests: Vec<TestExpectation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub analysis: Option<AnalysisCriteria>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<ApprovalFlowCriteria>,
}

impl OutcomeEvaluationCriteria {
    pub const SCHEMA: &'static str = "lenso.agent.outcome-evaluation-criteria@1";

    fn validate(&self) -> Result<(), String> {
        if self.schema != Self::SCHEMA {
            return Err(format!(
                "Outcome evaluation criteria schema `{}` is unsupported",
                self.schema
            ));
        }
        if matches!(
            self.terminal_outcome,
            TrajectoryStatus::Idle | TrajectoryStatus::Running
        ) {
            return Err("outcome evaluation requires a terminal Trajectory status".to_owned());
        }
        self.workspace.validate()?;
        validate_unique_test_expectations(&self.tests)?;
        if matches!(
            self.task_kind,
            OutcomeTaskKind::BugFix | OutcomeTaskKind::Refactor | OutcomeTaskKind::ApprovalFlow
        ) && self.tests.is_empty()
        {
            return Err("this outcome task kind requires at least one test expectation".to_owned());
        }
        if matches!(self.task_kind, OutcomeTaskKind::BugFix) && self.analysis.is_none() {
            return Err("bug-fix evaluation requires root-cause acceptance criteria".to_owned());
        }
        if matches!(self.task_kind, OutcomeTaskKind::ReadOnlyAnalysis) && self.analysis.is_none() {
            return Err("read-only analysis requires analysis acceptance criteria".to_owned());
        }
        if matches!(self.task_kind, OutcomeTaskKind::ApprovalFlow) && self.approval.is_none() {
            return Err(
                "approval-flow evaluation requires approval acceptance criteria".to_owned(),
            );
        }
        if let Some(criteria) = &self.analysis {
            criteria.validate()?;
        }
        if let Some(criteria) = &self.approval {
            criteria.validate()?;
        }
        validate_task_workspace_contract(self)?;
        Ok(())
    }
}

/// Required and forbidden changes for the Workspace state around one task.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct WorkspaceCriteria {
    /// Paths that must have changed between the before and after snapshots.
    pub expected_changed_paths: BTreeSet<String>,
    /// Additional changed paths that do not count as unrelated breakage.
    pub allowed_changed_paths: BTreeSet<String>,
    /// Whether a changed path outside the expected and allowed sets fails the task.
    pub reject_unexpected_changes: bool,
    /// Exact final source digests for behavior or structure that must be present.
    pub expected_file_digests: BTreeMap<String, String>,
    /// Paths that must be absent from the final snapshot.
    pub expected_absent_paths: BTreeSet<String>,
    /// Existing paths whose final digest must equal the before snapshot.
    pub unchanged_paths: BTreeSet<String>,
}

impl WorkspaceCriteria {
    pub fn exact_changes(paths: impl IntoIterator<Item = String>) -> Self {
        Self {
            expected_changed_paths: paths.into_iter().collect(),
            reject_unexpected_changes: true,
            ..Self::default()
        }
    }

    fn validate(&self) -> Result<(), String> {
        for path in self
            .expected_changed_paths
            .iter()
            .chain(&self.allowed_changed_paths)
            .chain(self.expected_file_digests.keys())
            .chain(&self.expected_absent_paths)
            .chain(&self.unchanged_paths)
        {
            validate_workspace_path(path)?;
        }
        for digest in self.expected_file_digests.values() {
            validate_digest(digest)?;
        }
        Ok(())
    }
}

impl Default for WorkspaceCriteria {
    fn default() -> Self {
        Self {
            expected_changed_paths: BTreeSet::new(),
            allowed_changed_paths: BTreeSet::new(),
            reject_unexpected_changes: true,
            expected_file_digests: BTreeMap::new(),
            expected_absent_paths: BTreeSet::new(),
            unchanged_paths: BTreeSet::new(),
        }
    }
}

/// One named test result expected from the final Workspace state.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TestExpectation {
    pub id: String,
    #[serde(default = "default_require_success")]
    pub require_success: bool,
}

/// A test status recorded by a fixture runner or another trusted test adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TestResultStatus {
    Passed,
    Failed,
    Skipped,
    TimedOut,
}

impl TestResultStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
            Self::TimedOut => "timed_out",
        }
    }
}

/// Result metadata emitted by a test adapter; output stays out of the report.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TestResultEvidence {
    pub id: String,
    pub status: TestResultStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_sha256: Option<String>,
}

/// Correctness facts from a deterministic or human-reviewed analysis grader.
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct AnalysisCriteria {
    pub required_finding_ids: BTreeSet<String>,
    pub reject_unexpected_findings: bool,
}

impl AnalysisCriteria {
    fn validate(&self) -> Result<(), String> {
        for finding in &self.required_finding_ids {
            validate_evidence_id(finding, "analysis finding")?;
        }
        Ok(())
    }
}

/// Findings observed in the Agent's delivered analysis, mapped by a trusted grader.
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct AnalysisEvidence {
    pub finding_ids: BTreeSet<String>,
}

/// Approval-flow properties that must be true of a state-changing task.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ApprovalFlowCriteria {
    pub proposal_id: String,
    pub current_revision: u64,
    #[serde(default = "default_require_approved_effect")]
    pub require_approved_effect: bool,
    #[serde(default = "default_require_stale_rejection")]
    pub require_stale_rejection: bool,
}

impl ApprovalFlowCriteria {
    fn validate(&self) -> Result<(), String> {
        validate_evidence_id(&self.proposal_id, "proposal")?;
        if self.current_revision == 0 {
            return Err("approval current revision must be greater than zero".to_owned());
        }
        Ok(())
    }
}

/// Terminal decision observed for one proposal-approval attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Applied,
    Denied,
    RejectedStale,
    Unknown,
}

/// Whether the effect covered by an approval attempt was observed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectState {
    Applied,
    NotApplied,
    Unknown,
}

/// One approval decision paired with its observed side-effect state.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ApprovalAttemptEvidence {
    pub approval_revision: u64,
    pub decision: ApprovalDecision,
    pub effect: EffectState,
}

/// Durable proposal facts supplied by the authority that owns approval.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ApprovalFlowEvidence {
    pub proposal_id: String,
    pub proposal_created: bool,
    pub current_revision: u64,
    #[serde(default)]
    pub attempts: Vec<ApprovalAttemptEvidence>,
}

/// Evidence collected outside Session replay and evaluated with its Trajectory.
///
/// Test execution and approval remain owned by their existing adapters. This
/// transfer object deliberately records only their bounded terminal facts.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct OutcomeEvidence {
    pub schema: String,
    /// The durable Session that this external evidence was captured for.
    pub session_id: String,
    /// The exact Trajectory revision observed when the evidence was captured.
    pub trajectory_revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_before: Option<WorkspaceSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_after: Option<WorkspaceSnapshot>,
    pub tests: Vec<TestResultEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub analysis: Option<AnalysisEvidence>,
    pub approval_flows: Vec<ApprovalFlowEvidence>,
}

impl OutcomeEvidence {
    pub const SCHEMA: &'static str = "lenso.agent.outcome-evidence@1";

    /// Starts evidence that is bound to one immutable Trajectory projection.
    pub fn for_trajectory(trajectory: &Trajectory) -> Self {
        Self {
            schema: Self::SCHEMA.to_owned(),
            session_id: trajectory.session_id.clone(),
            trajectory_revision: trajectory.revision,
            workspace_before: None,
            workspace_after: None,
            tests: Vec::new(),
            analysis: None,
            approval_flows: Vec::new(),
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema != Self::SCHEMA {
            return Err(format!(
                "Outcome evidence schema `{}` is unsupported",
                self.schema
            ));
        }
        if !valid_session_id(&self.session_id) {
            return Err("outcome evidence Session ID is invalid".to_owned());
        }
        if let Some(snapshot) = &self.workspace_before {
            snapshot.validate()?;
        }
        if let Some(snapshot) = &self.workspace_after {
            snapshot.validate()?;
        }
        let mut test_ids = BTreeSet::new();
        for test in &self.tests {
            validate_evidence_id(&test.id, "test")?;
            if !test_ids.insert(test.id.as_str()) {
                return Err(format!("outcome evidence repeats test `{}`", test.id));
            }
            if let Some(digest) = &test.output_sha256 {
                validate_digest(digest)?;
            }
        }
        if let Some(analysis) = &self.analysis {
            for finding in &analysis.finding_ids {
                validate_evidence_id(finding, "analysis finding")?;
            }
        }
        let mut proposal_ids = BTreeSet::new();
        for flow in &self.approval_flows {
            validate_evidence_id(&flow.proposal_id, "proposal")?;
            if flow.current_revision == 0 {
                return Err(
                    "approval evidence current revision must be greater than zero".to_owned(),
                );
            }
            if !proposal_ids.insert(flow.proposal_id.as_str()) {
                return Err(format!(
                    "outcome evidence repeats proposal `{}`",
                    flow.proposal_id
                ));
            }
            if flow.attempts.iter().any(|attempt| {
                attempt.approval_revision == 0 || attempt.approval_revision > flow.current_revision
            }) {
                return Err(
                    "approval evidence revision must be greater than zero and no later than the current revision"
                        .to_owned(),
                );
            }
        }
        Ok(())
    }
}

impl Default for OutcomeEvidence {
    fn default() -> Self {
        Self {
            schema: Self::SCHEMA.to_owned(),
            session_id: String::new(),
            trajectory_revision: 0,
            workspace_before: None,
            workspace_after: None,
            tests: Vec::new(),
            analysis: None,
            approval_flows: Vec::new(),
        }
    }
}

/// Status for one independently inspectable outcome dimension.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeDimensionStatus {
    Passed,
    Failed,
    NotApplicable,
}

/// Checks and terminal status for one task-result dimension.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct OutcomeDimension {
    pub status: OutcomeDimensionStatus,
    pub checks: Vec<EvaluationCheck>,
}

impl OutcomeDimension {
    fn not_applicable() -> Self {
        Self {
            status: OutcomeDimensionStatus::NotApplicable,
            checks: Vec::new(),
        }
    }

    fn from_checks(checks: Vec<EvaluationCheck>) -> Self {
        let status = if checks.iter().all(|check| check.passed) {
            OutcomeDimensionStatus::Passed
        } else {
            OutcomeDimensionStatus::Failed
        };
        Self { status, checks }
    }
}

/// Stable Session and Tool facts retained as context for an outcome report.
///
/// These facts explain the execution that produced the evaluated state, but do
/// not substitute for Workspace, test, or policy acceptance checks.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct OutcomeTrajectoryEvidence {
    pub terminal_status: TrajectoryStatus,
    pub tool_calls: u32,
    pub failed_operations: u32,
}

/// Machine-readable answer to whether an Agent completed one real task.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct OutcomeEvaluationReport {
    pub schema: String,
    pub task_kind: OutcomeTaskKind,
    pub session_id: String,
    pub trajectory_revision: u64,
    pub passed: bool,
    pub trajectory_evidence: OutcomeTrajectoryEvidence,
    pub workspace_state: OutcomeDimension,
    pub test_result: OutcomeDimension,
    pub policy_compliance: OutcomeDimension,
    pub terminal_outcome: OutcomeDimension,
    pub analysis: OutcomeDimension,
}

impl OutcomeEvaluationReport {
    pub const SCHEMA: &'static str = "lenso.agent.outcome-evaluation@1";
}

/// Evaluates real task evidence without replaying a Model, Tool, approval, or test command.
pub fn evaluate_outcome(
    trajectory: &Trajectory,
    criteria: &OutcomeEvaluationCriteria,
    evidence: &OutcomeEvidence,
) -> Result<OutcomeEvaluationReport, String> {
    if trajectory.schema != Trajectory::SCHEMA || !valid_session_id(&trajectory.session_id) {
        return Err("trajectory is not a supported completed Session projection".to_owned());
    }
    criteria.validate()?;
    evidence.validate()?;
    if evidence.session_id != trajectory.session_id
        || evidence.trajectory_revision != trajectory.revision
    {
        return Err("outcome evidence is not bound to this exact Trajectory revision".to_owned());
    }

    let workspace_state = evaluate_workspace(&criteria.workspace, evidence);
    let test_result = evaluate_tests(&criteria.tests, evidence);
    let policy_compliance = criteria
        .approval
        .as_ref()
        .map_or_else(OutcomeDimension::not_applicable, |criteria| {
            evaluate_approval(criteria, evidence)
        });
    let terminal_outcome = OutcomeDimension::from_checks(vec![check(
        "terminal_outcome",
        trajectory.summary.status == criteria.terminal_outcome,
        status_label(criteria.terminal_outcome).to_owned(),
        status_label(trajectory.summary.status).to_owned(),
    )]);
    let analysis = criteria
        .analysis
        .as_ref()
        .map_or_else(OutcomeDimension::not_applicable, |criteria| {
            evaluate_analysis(criteria, evidence)
        });

    let dimensions = [
        &workspace_state,
        &test_result,
        &policy_compliance,
        &terminal_outcome,
        &analysis,
    ];
    let passed = dimensions
        .iter()
        .all(|dimension| dimension.status != OutcomeDimensionStatus::Failed);
    Ok(OutcomeEvaluationReport {
        schema: OutcomeEvaluationReport::SCHEMA.to_owned(),
        task_kind: criteria.task_kind,
        session_id: trajectory.session_id.clone(),
        trajectory_revision: trajectory.revision,
        passed,
        trajectory_evidence: OutcomeTrajectoryEvidence {
            terminal_status: trajectory.summary.status,
            tool_calls: trajectory.summary.tool_calls,
            failed_operations: trajectory.summary.failed_operations,
        },
        workspace_state,
        test_result,
        policy_compliance,
        terminal_outcome,
        analysis,
    })
}

fn evaluate_workspace(
    criteria: &WorkspaceCriteria,
    evidence: &OutcomeEvidence,
) -> OutcomeDimension {
    let (Some(before), Some(after)) = (&evidence.workspace_before, &evidence.workspace_after)
    else {
        return OutcomeDimension::from_checks(vec![check(
            "workspace_evidence",
            false,
            "before and after snapshots".to_owned(),
            "missing".to_owned(),
        )]);
    };
    let delta = match before.diff(after) {
        Ok(delta) => delta,
        Err(error) => {
            return OutcomeDimension::from_checks(vec![check(
                "workspace_evidence",
                false,
                "valid snapshots".to_owned(),
                error,
            )]);
        }
    };
    let changed = delta.all_changed_paths();
    let mut checks = workspace_change_checks(criteria, &changed);
    checks.extend(workspace_final_state_checks(criteria, before, after));
    OutcomeDimension::from_checks(checks)
}

fn workspace_change_checks(
    criteria: &WorkspaceCriteria,
    changed: &BTreeSet<String>,
) -> Vec<EvaluationCheck> {
    let allowed = criteria
        .expected_changed_paths
        .iter()
        .chain(&criteria.allowed_changed_paths)
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut checks = criteria
        .expected_changed_paths
        .iter()
        .map(|path| {
            check(
                &format!("workspace.changed:{path}"),
                changed.contains(path),
                "changed".to_owned(),
                if changed.contains(path) {
                    "changed"
                } else {
                    "unchanged"
                }
                .to_owned(),
            )
        })
        .collect::<Vec<_>>();
    if criteria.reject_unexpected_changes {
        checks.extend(changed.difference(&allowed).map(|path| {
            check(
                &format!("workspace.unexpected_change:{path}"),
                false,
                "not changed".to_owned(),
                "changed".to_owned(),
            )
        }));
    }
    checks
}

fn workspace_final_state_checks(
    criteria: &WorkspaceCriteria,
    before: &WorkspaceSnapshot,
    after: &WorkspaceSnapshot,
) -> Vec<EvaluationCheck> {
    let mut checks = criteria
        .expected_file_digests
        .iter()
        .map(|(path, expected)| {
            let actual = after
                .files
                .get(path)
                .map_or("absent", |file| file.sha256.as_str());
            check(
                &format!("workspace.digest:{path}"),
                actual == expected,
                expected.clone(),
                actual.to_owned(),
            )
        })
        .collect::<Vec<_>>();
    checks.extend(criteria.expected_absent_paths.iter().map(|path| {
        check(
            &format!("workspace.absent:{path}"),
            !after.files.contains_key(path),
            "absent".to_owned(),
            if after.files.contains_key(path) {
                "present"
            } else {
                "absent"
            }
            .to_owned(),
        )
    }));
    checks.extend(criteria.unchanged_paths.iter().map(|path| {
        let unchanged = before.files.get(path).is_some_and(|before_file| {
            after
                .files
                .get(path)
                .is_some_and(|after_file| before_file == after_file)
        });
        check(
            &format!("workspace.unchanged:{path}"),
            unchanged,
            "same digest".to_owned(),
            if unchanged {
                "same digest"
            } else {
                "changed or absent"
            }
            .to_owned(),
        )
    }));
    checks
}

fn evaluate_tests(criteria: &[TestExpectation], evidence: &OutcomeEvidence) -> OutcomeDimension {
    if criteria.is_empty() {
        return OutcomeDimension::not_applicable();
    }
    let results = evidence
        .tests
        .iter()
        .map(|result| (result.id.as_str(), result))
        .collect::<BTreeMap<_, _>>();
    let checks = criteria
        .iter()
        .map(|expectation| match results.get(expectation.id.as_str()) {
            Some(result) => {
                let passed =
                    (result.status == TestResultStatus::Passed) == expectation.require_success;
                check(
                    &format!("test:{}", expectation.id),
                    passed,
                    if expectation.require_success {
                        "passed"
                    } else {
                        "not passed"
                    }
                    .to_owned(),
                    result.status.label().to_owned(),
                )
            }
            None => check(
                &format!("test:{}", expectation.id),
                false,
                if expectation.require_success {
                    "passed"
                } else {
                    "not passed"
                }
                .to_owned(),
                "missing".to_owned(),
            ),
        })
        .collect();
    OutcomeDimension::from_checks(checks)
}

fn evaluate_analysis(criteria: &AnalysisCriteria, evidence: &OutcomeEvidence) -> OutcomeDimension {
    let Some(evidence) = &evidence.analysis else {
        return OutcomeDimension::from_checks(vec![check(
            "analysis_evidence",
            false,
            "graded findings".to_owned(),
            "missing".to_owned(),
        )]);
    };
    let mut checks = criteria
        .required_finding_ids
        .iter()
        .map(|finding| {
            check(
                &format!("analysis.finding:{finding}"),
                evidence.finding_ids.contains(finding),
                "present".to_owned(),
                if evidence.finding_ids.contains(finding) {
                    "present"
                } else {
                    "missing"
                }
                .to_owned(),
            )
        })
        .collect::<Vec<_>>();
    if criteria.reject_unexpected_findings {
        checks.extend(
            evidence
                .finding_ids
                .difference(&criteria.required_finding_ids)
                .map(|finding| {
                    check(
                        &format!("analysis.unexpected_finding:{finding}"),
                        false,
                        "not present".to_owned(),
                        "present".to_owned(),
                    )
                }),
        );
    }
    OutcomeDimension::from_checks(checks)
}

fn evaluate_approval(
    criteria: &ApprovalFlowCriteria,
    evidence: &OutcomeEvidence,
) -> OutcomeDimension {
    let Some(flow) = evidence
        .approval_flows
        .iter()
        .find(|flow| flow.proposal_id == criteria.proposal_id)
    else {
        return OutcomeDimension::from_checks(vec![check(
            "policy.proposal",
            false,
            criteria.proposal_id.clone(),
            "missing".to_owned(),
        )]);
    };
    let mut checks = vec![
        check(
            "policy.proposal_created",
            flow.proposal_created,
            "created".to_owned(),
            if flow.proposal_created {
                "created"
            } else {
                "not_created"
            }
            .to_owned(),
        ),
        check(
            "policy.current_revision",
            flow.current_revision == criteria.current_revision,
            criteria.current_revision.to_string(),
            flow.current_revision.to_string(),
        ),
    ];
    if criteria.require_approved_effect {
        let approved = flow.attempts.iter().any(|attempt| {
            attempt.approval_revision == criteria.current_revision
                && attempt.decision == ApprovalDecision::Applied
                && attempt.effect == EffectState::Applied
        });
        checks.push(check(
            "policy.approved_effect",
            approved,
            "approved revision applied".to_owned(),
            if approved {
                "approved revision applied"
            } else {
                "missing"
            }
            .to_owned(),
        ));
    }
    if criteria.require_stale_rejection {
        let stale_attempts = flow
            .attempts
            .iter()
            .filter(|attempt| attempt.approval_revision < criteria.current_revision)
            .collect::<Vec<_>>();
        let stale = !stale_attempts.is_empty()
            && stale_attempts.iter().all(|attempt| {
                attempt.decision == ApprovalDecision::RejectedStale
                    && attempt.effect == EffectState::NotApplied
            });
        checks.push(check(
            "policy.stale_approval",
            stale,
            "stale revision rejected without effect".to_owned(),
            if stale {
                "stale revision rejected without effect"
            } else {
                "missing"
            }
            .to_owned(),
        ));
    }
    OutcomeDimension::from_checks(checks)
}

fn validate_task_workspace_contract(criteria: &OutcomeEvaluationCriteria) -> Result<(), String> {
    let workspace = &criteria.workspace;
    match criteria.task_kind {
        OutcomeTaskKind::BugFix | OutcomeTaskKind::Refactor => {
            if workspace.expected_changed_paths.is_empty() {
                return Err(
                    "bug-fix and refactor evaluation require at least one expected changed Workspace path"
                        .to_owned(),
                );
            }
            if workspace.expected_file_digests.is_empty()
                && workspace.expected_absent_paths.is_empty()
            {
                return Err(
                    "bug-fix and refactor evaluation require an expected final Workspace state"
                        .to_owned(),
                );
            }
            if !workspace.reject_unexpected_changes {
                return Err(
                    "bug-fix and refactor evaluation must reject unrelated Workspace changes"
                        .to_owned(),
                );
            }
        }
        OutcomeTaskKind::ReadOnlyAnalysis => {
            if !workspace.expected_changed_paths.is_empty()
                || !workspace.allowed_changed_paths.is_empty()
                || !workspace.reject_unexpected_changes
            {
                return Err(
                    "read-only analysis evaluation requires an unchanged Workspace contract"
                        .to_owned(),
                );
            }
        }
        OutcomeTaskKind::ApprovalFlow => {}
    }
    Ok(())
}

fn validate_unique_test_expectations(expectations: &[TestExpectation]) -> Result<(), String> {
    let mut ids = BTreeSet::new();
    for expectation in expectations {
        validate_evidence_id(&expectation.id, "test")?;
        if !ids.insert(expectation.id.as_str()) {
            return Err(format!(
                "outcome criteria repeats test `{}`",
                expectation.id
            ));
        }
    }
    Ok(())
}

fn validate_evidence_id(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(format!("{label} id `{value}` is invalid"));
    }
    Ok(())
}

fn default_require_success() -> bool {
    true
}

fn default_require_approved_effect() -> bool {
    true
}

fn default_require_stale_rejection() -> bool {
    true
}

fn status_label(status: TrajectoryStatus) -> &'static str {
    match status {
        TrajectoryStatus::Idle => "idle",
        TrajectoryStatus::Running => "running",
        TrajectoryStatus::Completed => "completed",
        TrajectoryStatus::Failed => "failed",
        TrajectoryStatus::Cancelled => "cancelled",
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
    use crate::{
        TrajectoryDetail, TrajectoryKind, TrajectoryRecord, TrajectorySummary, WorkspaceFile,
    };
    use sha2::Digest;

    fn digest(value: &str) -> String {
        format!("sha256:{:x}", sha2::Sha256::digest(value.as_bytes()))
    }

    fn snapshot(files: &[(&str, &str)]) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            schema: WorkspaceSnapshot::SCHEMA.to_owned(),
            files: files
                .iter()
                .map(|(path, content)| {
                    (
                        (*path).to_owned(),
                        WorkspaceFile {
                            sha256: digest(content),
                            bytes: u64::try_from(content.len()).unwrap(),
                        },
                    )
                })
                .collect(),
        }
    }

    fn completed_trajectory() -> Trajectory {
        Trajectory {
            schema: Trajectory::SCHEMA.to_owned(),
            session_id: "session-1".to_owned(),
            revision: 4,
            summary: TrajectorySummary {
                status: TrajectoryStatus::Completed,
                turns: 1,
                model_calls: 1,
                tool_calls: 1,
                failed_operations: 0,
                input_tokens: 0,
                output_tokens: 0,
                started_at: None,
                updated_at: None,
                duration_ms: Some(1),
            },
            records: vec![TrajectoryRecord {
                id: "tool-1".to_owned(),
                turn: 1,
                kind: TrajectoryKind::Tool,
                status: TrajectoryStatus::Completed,
                label: "write_file".to_owned(),
                preview: "Completed".to_owned(),
                started_at: "2026-09-20T00:00:00Z".to_owned(),
                completed_at: Some("2026-09-20T00:00:01Z".to_owned()),
                duration_ms: Some(1),
                time_to_first_token_ms: None,
                step: None,
                input_tokens: None,
                output_tokens: None,
                detail: TrajectoryDetail::default(),
                source_event_ids: vec!["event-1".to_owned()],
            }],
        }
    }

    fn criteria() -> OutcomeEvaluationCriteria {
        OutcomeEvaluationCriteria {
            schema: OutcomeEvaluationCriteria::SCHEMA.to_owned(),
            task_kind: OutcomeTaskKind::BugFix,
            terminal_outcome: TrajectoryStatus::Completed,
            workspace: WorkspaceCriteria {
                expected_changed_paths: BTreeSet::from(["src/lib.rs".to_owned()]),
                reject_unexpected_changes: true,
                expected_file_digests: BTreeMap::from([("src/lib.rs".to_owned(), digest("fixed"))]),
                ..WorkspaceCriteria::default()
            },
            tests: vec![TestExpectation {
                id: "unit".to_owned(),
                require_success: true,
            }],
            analysis: Some(AnalysisCriteria {
                required_finding_ids: BTreeSet::from(["parse_port.missing_trim".to_owned()]),
                reject_unexpected_findings: true,
            }),
            approval: None,
        }
    }

    #[test]
    fn successful_tool_and_completed_session_do_not_mask_a_wrong_workspace_result() {
        let trajectory = completed_trajectory();
        let mut evidence = OutcomeEvidence::for_trajectory(&trajectory);
        evidence.workspace_before = Some(snapshot(&[("src/lib.rs", "broken")]));
        evidence.workspace_after = Some(snapshot(&[("src/lib.rs", "still broken")]));
        evidence.tests = vec![TestResultEvidence {
            id: "unit".to_owned(),
            status: TestResultStatus::Passed,
            output_sha256: None,
        }];
        evidence.analysis = Some(AnalysisEvidence {
            finding_ids: BTreeSet::from(["parse_port.missing_trim".to_owned()]),
        });

        let report = evaluate_outcome(&trajectory, &criteria(), &evidence).unwrap();
        assert!(!report.passed);
        assert_eq!(
            report.terminal_outcome.status,
            OutcomeDimensionStatus::Passed
        );
        assert_eq!(report.test_result.status, OutcomeDimensionStatus::Passed);
        assert_eq!(
            report.workspace_state.status,
            OutcomeDimensionStatus::Failed
        );
    }

    #[test]
    fn approval_requires_both_a_current_effect_and_stale_rejection_without_effect() {
        let trajectory = completed_trajectory();
        let mut criteria = criteria();
        criteria.task_kind = OutcomeTaskKind::ApprovalFlow;
        criteria.analysis = None;
        criteria.workspace = WorkspaceCriteria {
            reject_unexpected_changes: true,
            ..WorkspaceCriteria::default()
        };
        criteria.approval = Some(ApprovalFlowCriteria {
            proposal_id: "proposal-1".to_owned(),
            current_revision: 2,
            require_approved_effect: true,
            require_stale_rejection: true,
        });
        let mut evidence = OutcomeEvidence::for_trajectory(&trajectory);
        evidence.workspace_before = Some(snapshot(&[("src/lib.rs", "fixed")]));
        evidence.workspace_after = Some(snapshot(&[("src/lib.rs", "fixed")]));
        evidence.tests = vec![TestResultEvidence {
            id: "unit".to_owned(),
            status: TestResultStatus::Passed,
            output_sha256: None,
        }];
        evidence.approval_flows = vec![ApprovalFlowEvidence {
            proposal_id: "proposal-1".to_owned(),
            proposal_created: true,
            current_revision: 2,
            attempts: vec![
                ApprovalAttemptEvidence {
                    approval_revision: 2,
                    decision: ApprovalDecision::Applied,
                    effect: EffectState::Applied,
                },
                ApprovalAttemptEvidence {
                    approval_revision: 1,
                    decision: ApprovalDecision::RejectedStale,
                    effect: EffectState::NotApplied,
                },
            ],
        }];

        let report = evaluate_outcome(&trajectory, &criteria, &evidence).unwrap();
        assert!(report.passed);
        assert_eq!(
            report.policy_compliance.status,
            OutcomeDimensionStatus::Passed
        );

        let mut stale_effect = evidence.clone();
        stale_effect.approval_flows[0].attempts[1].effect = EffectState::Applied;
        let report = evaluate_outcome(&trajectory, &criteria, &stale_effect).unwrap();
        assert!(!report.passed);
        assert_eq!(
            report.policy_compliance.status,
            OutcomeDimensionStatus::Failed
        );
    }

    #[test]
    fn malformed_or_incomplete_evidence_fails_before_it_can_be_counted_as_success() {
        let trajectory = completed_trajectory();
        let mut evidence = OutcomeEvidence::for_trajectory(&trajectory);
        evidence.workspace_before = Some(snapshot(&[("src/lib.rs", "broken")]));
        assert!(evaluate_outcome(&trajectory, &criteria(), &evidence).is_ok());
        let report = evaluate_outcome(&trajectory, &criteria(), &evidence).unwrap();
        assert!(!report.passed);
        assert_eq!(
            report.workspace_state.status,
            OutcomeDimensionStatus::Failed
        );

        evidence.schema = "unknown".to_owned();
        assert!(evaluate_outcome(&trajectory, &criteria(), &evidence).is_err());
    }

    #[test]
    fn evidence_cannot_be_reused_for_another_trajectory_revision() {
        let trajectory = completed_trajectory();
        let mut evidence = OutcomeEvidence::for_trajectory(&trajectory);
        evidence.trajectory_revision -= 1;

        let error = evaluate_outcome(&trajectory, &criteria(), &evidence).unwrap_err();
        assert!(error.contains("not bound to this exact Trajectory revision"));
    }

    #[test]
    fn task_kinds_reject_criteria_that_cannot_prove_their_declared_outcome() {
        let trajectory = completed_trajectory();
        let evidence = OutcomeEvidence::for_trajectory(&trajectory);

        let mut invalid_bug_fix = criteria();
        invalid_bug_fix.workspace.expected_changed_paths.clear();
        assert!(
            evaluate_outcome(&trajectory, &invalid_bug_fix, &evidence)
                .unwrap_err()
                .contains("expected changed Workspace path")
        );

        let mut invalid_refactor = criteria();
        invalid_refactor.task_kind = OutcomeTaskKind::Refactor;
        invalid_refactor.analysis = None;
        invalid_refactor.workspace.expected_file_digests.clear();
        assert!(
            evaluate_outcome(&trajectory, &invalid_refactor, &evidence)
                .unwrap_err()
                .contains("expected final Workspace state")
        );

        let mut invalid_read_only = criteria();
        invalid_read_only.task_kind = OutcomeTaskKind::ReadOnlyAnalysis;
        invalid_read_only.workspace = WorkspaceCriteria {
            expected_changed_paths: BTreeSet::from(["src/lib.rs".to_owned()]),
            ..WorkspaceCriteria::default()
        };
        assert!(
            evaluate_outcome(&trajectory, &invalid_read_only, &evidence)
                .unwrap_err()
                .contains("unchanged Workspace contract")
        );

        let mut invalid_terminal = criteria();
        invalid_terminal.terminal_outcome = TrajectoryStatus::Running;
        assert!(
            evaluate_outcome(&trajectory, &invalid_terminal, &evidence)
                .unwrap_err()
                .contains("terminal Trajectory status")
        );
    }

    #[test]
    fn policy_fails_when_any_stale_approval_has_an_observed_effect() {
        let trajectory = completed_trajectory();
        let mut criteria = criteria();
        criteria.task_kind = OutcomeTaskKind::ApprovalFlow;
        criteria.analysis = None;
        criteria.workspace = WorkspaceCriteria {
            reject_unexpected_changes: true,
            ..WorkspaceCriteria::default()
        };
        criteria.approval = Some(ApprovalFlowCriteria {
            proposal_id: "proposal-1".to_owned(),
            current_revision: 2,
            require_approved_effect: true,
            require_stale_rejection: true,
        });
        let mut evidence = OutcomeEvidence::for_trajectory(&trajectory);
        evidence.workspace_before = Some(snapshot(&[("src/lib.rs", "fixed")]));
        evidence.workspace_after = Some(snapshot(&[("src/lib.rs", "fixed")]));
        evidence.tests = vec![TestResultEvidence {
            id: "unit".to_owned(),
            status: TestResultStatus::Passed,
            output_sha256: None,
        }];
        evidence.approval_flows = vec![ApprovalFlowEvidence {
            proposal_id: "proposal-1".to_owned(),
            proposal_created: true,
            current_revision: 2,
            attempts: vec![
                ApprovalAttemptEvidence {
                    approval_revision: 2,
                    decision: ApprovalDecision::Applied,
                    effect: EffectState::Applied,
                },
                ApprovalAttemptEvidence {
                    approval_revision: 1,
                    decision: ApprovalDecision::RejectedStale,
                    effect: EffectState::NotApplied,
                },
                ApprovalAttemptEvidence {
                    approval_revision: 1,
                    decision: ApprovalDecision::Applied,
                    effect: EffectState::Applied,
                },
            ],
        }];

        let report = evaluate_outcome(&trajectory, &criteria, &evidence).unwrap();
        assert_eq!(
            report.policy_compliance.status,
            OutcomeDimensionStatus::Failed
        );

        evidence.approval_flows[0].attempts[2].approval_revision = 3;
        assert!(
            evaluate_outcome(&trajectory, &criteria, &evidence)
                .unwrap_err()
                .contains("no later than the current revision")
        );
    }
}
