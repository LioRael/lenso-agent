use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use lenso_agent_session_inspection::{
    AnalysisCriteria, AnalysisEvidence, ApprovalAttemptEvidence, ApprovalDecision,
    ApprovalFlowCriteria, ApprovalFlowEvidence, EffectState, OutcomeEvaluationCriteria,
    OutcomeEvidence, OutcomeTaskKind, TestExpectation, TestResultEvidence, TestResultStatus,
    Trajectory, TrajectoryStatus, TrajectorySummary, WorkspaceCriteria, WorkspaceSnapshot,
    evaluate_outcome,
};
use sha2::{Digest, Sha256};

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/outcome-repositories"
);
const BUG_FIX_FINAL_DIGEST: &str =
    "sha256:06019b29a2ff724ce36b208c9c0a8dead6ceadc4013ca39d6016b3eade3a0aa8";
const REFACTOR_FINAL_DIGEST: &str =
    "sha256:5d86c3bb84be9f9c120d61b1d406dee4f9579c2db76ea327427bc20d6abbada8";
const APPROVAL_FINAL_DIGEST: &str =
    "sha256:87e3d8bb7eed28d4bde25c22617bc500f9eac24ad2cac1817008d59902c205f5";

#[test]
fn bug_fix_fixture_requires_the_correct_file_state_and_a_passing_regression_test() {
    let workspace = prepare_fixture("bug-fix", true);
    let before = WorkspaceSnapshot::capture(workspace.path()).unwrap();
    overlay_fixture(workspace.path(), fixture_path("bug-fix").join("final"));
    let after = WorkspaceSnapshot::capture(workspace.path()).unwrap();
    let test = run_cargo_test(workspace.path(), "cargo-test");
    assert_eq!(
        after.files["src/lib.rs"].sha256, BUG_FIX_FINAL_DIGEST,
        "fixture digest is a reviewable root-cause guard"
    );

    let mut evidence = evidence(before, after, vec![test]);
    evidence.analysis = Some(AnalysisEvidence {
        finding_ids: BTreeSet::from(["parse_port.missing_trim".to_owned()]),
    });
    let report = evaluate_outcome(
        &completed_trajectory(),
        &OutcomeEvaluationCriteria {
            schema: OutcomeEvaluationCriteria::SCHEMA.to_owned(),
            task_kind: OutcomeTaskKind::BugFix,
            terminal_outcome: TrajectoryStatus::Completed,
            workspace: WorkspaceCriteria {
                expected_changed_paths: BTreeSet::from(["src/lib.rs".to_owned()]),
                expected_file_digests: BTreeMap::from([(
                    "src/lib.rs".to_owned(),
                    BUG_FIX_FINAL_DIGEST.to_owned(),
                )]),
                reject_unexpected_changes: true,
                ..WorkspaceCriteria::default()
            },
            tests: vec![TestExpectation {
                id: "cargo-test".to_owned(),
                require_success: true,
            }],
            analysis: Some(AnalysisCriteria {
                required_finding_ids: BTreeSet::from(["parse_port.missing_trim".to_owned()]),
                reject_unexpected_findings: true,
            }),
            approval: None,
        },
        &evidence,
    )
    .unwrap();

    assert!(report.passed, "{report:#?}");
}

#[test]
fn refactor_fixture_requires_the_expected_structure_while_preserving_behavior() {
    let workspace = prepare_fixture("refactor", true);
    let before = WorkspaceSnapshot::capture(workspace.path()).unwrap();
    overlay_fixture(workspace.path(), fixture_path("refactor").join("final"));
    let after = WorkspaceSnapshot::capture(workspace.path()).unwrap();
    let test = run_cargo_test(workspace.path(), "cargo-test");
    assert_eq!(after.files["src/lib.rs"].sha256, REFACTOR_FINAL_DIGEST);

    let report = evaluate_outcome(
        &completed_trajectory(),
        &OutcomeEvaluationCriteria {
            schema: OutcomeEvaluationCriteria::SCHEMA.to_owned(),
            task_kind: OutcomeTaskKind::Refactor,
            terminal_outcome: TrajectoryStatus::Completed,
            workspace: WorkspaceCriteria {
                expected_changed_paths: BTreeSet::from(["src/lib.rs".to_owned()]),
                expected_file_digests: BTreeMap::from([(
                    "src/lib.rs".to_owned(),
                    REFACTOR_FINAL_DIGEST.to_owned(),
                )]),
                reject_unexpected_changes: true,
                ..WorkspaceCriteria::default()
            },
            tests: vec![TestExpectation {
                id: "cargo-test".to_owned(),
                require_success: true,
            }],
            analysis: None,
            approval: None,
        },
        &evidence(before, after, vec![test]),
    )
    .unwrap();

    assert!(report.passed, "{report:#?}");
}

#[test]
fn read_only_analysis_fixture_requires_correct_findings_and_an_unchanged_workspace() {
    let workspace = prepare_fixture("read-only-analysis", false);
    let before = WorkspaceSnapshot::capture(workspace.path()).unwrap();
    let after = WorkspaceSnapshot::capture(workspace.path()).unwrap();
    let mut evidence = evidence(before, after, Vec::new());
    evidence.analysis = Some(AnalysisEvidence {
        finding_ids: BTreeSet::from([
            "retry.default_budget.3".to_owned(),
            "retry.attempts_per_job.4".to_owned(),
        ]),
    });

    let report = evaluate_outcome(
        &completed_trajectory(),
        &OutcomeEvaluationCriteria {
            schema: OutcomeEvaluationCriteria::SCHEMA.to_owned(),
            task_kind: OutcomeTaskKind::ReadOnlyAnalysis,
            terminal_outcome: TrajectoryStatus::Completed,
            workspace: WorkspaceCriteria::exact_changes(Vec::new()),
            tests: Vec::new(),
            analysis: Some(AnalysisCriteria {
                required_finding_ids: BTreeSet::from([
                    "retry.default_budget.3".to_owned(),
                    "retry.attempts_per_job.4".to_owned(),
                ]),
                reject_unexpected_findings: true,
            }),
            approval: None,
        },
        &evidence,
    )
    .unwrap();

    assert!(report.passed, "{report:#?}");
}

#[test]
fn approval_fixture_requires_current_approval_and_rejects_a_stale_attempt_without_effect() {
    let workspace = prepare_fixture("approval-flow", true);
    let before = WorkspaceSnapshot::capture(workspace.path()).unwrap();
    overlay_fixture(
        workspace.path(),
        fixture_path("approval-flow").join("final"),
    );
    let after = WorkspaceSnapshot::capture(workspace.path()).unwrap();
    let test = run_cargo_test(workspace.path(), "cargo-test");
    let mut evidence = evidence(before, after, vec![test]);
    evidence.approval_flows = vec![ApprovalFlowEvidence {
        proposal_id: "publish-42".to_owned(),
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

    let report = evaluate_outcome(
        &completed_trajectory(),
        &OutcomeEvaluationCriteria {
            schema: OutcomeEvaluationCriteria::SCHEMA.to_owned(),
            task_kind: OutcomeTaskKind::ApprovalFlow,
            terminal_outcome: TrajectoryStatus::Completed,
            workspace: WorkspaceCriteria {
                expected_changed_paths: BTreeSet::from(["src/lib.rs".to_owned()]),
                expected_file_digests: BTreeMap::from([(
                    "src/lib.rs".to_owned(),
                    APPROVAL_FINAL_DIGEST.to_owned(),
                )]),
                reject_unexpected_changes: true,
                ..WorkspaceCriteria::default()
            },
            tests: vec![TestExpectation {
                id: "cargo-test".to_owned(),
                require_success: true,
            }],
            analysis: None,
            approval: Some(ApprovalFlowCriteria {
                proposal_id: "publish-42".to_owned(),
                current_revision: 2,
                require_approved_effect: true,
                require_stale_rejection: true,
            }),
        },
        &evidence,
    )
    .unwrap();

    assert!(report.passed, "{report:#?}");
}

#[test]
fn bug_fix_and_approval_baselines_do_not_pass_their_required_regressions() {
    for fixture in ["bug-fix", "approval-flow"] {
        let workspace = prepare_baseline_fixture(fixture);
        let output = cargo_test_command(workspace.path()).output().unwrap();
        assert!(
            !output.status.success(),
            "the {fixture} baseline unexpectedly passes its regression:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

fn completed_trajectory() -> Trajectory {
    Trajectory {
        schema: Trajectory::SCHEMA.to_owned(),
        session_id: "fixture-session".to_owned(),
        revision: 1,
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
        records: Vec::new(),
    }
}

fn evidence(
    before: WorkspaceSnapshot,
    after: WorkspaceSnapshot,
    tests: Vec<TestResultEvidence>,
) -> OutcomeEvidence {
    OutcomeEvidence {
        schema: OutcomeEvidence::SCHEMA.to_owned(),
        session_id: "fixture-session".to_owned(),
        trajectory_revision: 1,
        workspace_before: Some(before),
        workspace_after: Some(after),
        tests,
        analysis: None,
        approval_flows: Vec::new(),
    }
}

fn run_cargo_test(workspace: &Path, id: &str) -> TestResultEvidence {
    let output = cargo_test_command(workspace).output().unwrap();
    assert!(
        output.status.success(),
        "fixture test failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let mut bytes = output.stdout;
    bytes.extend(output.stderr);
    TestResultEvidence {
        id: id.to_owned(),
        status: TestResultStatus::Passed,
        output_sha256: Some(format!("sha256:{:x}", Sha256::digest(bytes))),
    }
}

fn cargo_test_command(workspace: &Path) -> Command {
    let mut command = Command::new("cargo");
    command
        .args(["test", "--offline", "--quiet"])
        .current_dir(workspace)
        // Every fixture deliberately reuses a tiny package name. Its target
        // directory must be private so parallel fixture runs cannot reuse a
        // different fixture's fingerprint or test executable.
        .env("CARGO_TARGET_DIR", workspace.join("target"));
    command
}

fn prepare_fixture(name: &str, has_final: bool) -> tempfile::TempDir {
    let temporary = prepare_baseline_fixture(name);
    if !has_final {
        assert!(!fixture_path(name).join("final").exists());
    }
    temporary
}

fn prepare_baseline_fixture(name: &str) -> tempfile::TempDir {
    let temporary = tempfile::tempdir().unwrap();
    overlay_fixture(temporary.path(), fixture_path(name).join("baseline"));
    temporary
}

fn fixture_path(name: &str) -> PathBuf {
    Path::new(FIXTURES).join(name)
}

fn overlay_fixture(destination: &Path, source: PathBuf) {
    let mut entries = fs::read_dir(source)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let target = destination.join(entry.file_name());
        let metadata = entry.metadata().unwrap();
        if metadata.is_dir() {
            fs::create_dir_all(&target).unwrap();
            overlay_fixture(&target, entry.path());
        } else {
            assert!(metadata.is_file());
            fs::copy(entry.path(), target).unwrap();
        }
    }
}
