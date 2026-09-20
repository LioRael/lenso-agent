# ADR 0116: Evaluate Agent outcomes from task evidence

Status: Accepted

## Context

`sessions replay` and `sessions evaluate` already project durable Session facts
into a stable Trajectory. They can prove that a Turn reached a terminal state,
that an expected Tool was called, or that a Tool did not report a failure. None
of those facts proves that a bug was actually fixed, a refactor preserved
behavior, a read-only task left the Workspace alone, or a proposal respected
revision-scoped approval.

Replaying Tools or test commands during evaluation would add side effects and
would make a historical result depend on the current Host. Adding Workspace,
test, or approval policy to Kernel would also blur existing ownership:
Workspace Edit owns writes, test adapters own test execution, and the approval
authority owns proposal decisions.

## Decision

Add a separate `lenso.agent.outcome-evaluation@1` report alongside the
trajectory-only `lenso.agent.evaluation@1` report. It evaluates one immutable
Trajectory with bounded, externally collected `lenso.agent.outcome-evidence@1`
facts:

- a before/after content-only Workspace snapshot;
- named test terminal results, without command output or automatic reruns;
- optional structured analysis findings from a deterministic or reviewed grader;
  and
- optional approval-flow facts from the authority that owns proposals.

The criteria have their own
`lenso.agent.outcome-evaluation-criteria@1` schema and make the expected task
family explicit: bug fix, refactor, read-only analysis, or approval flow. Every
report has separately inspectable `workspaceState`, `testResult`,
`policyCompliance`, and `terminalOutcome` dimensions. A missing or invalid
required evidence item fails that dimension; a successful Tool record cannot
make it pass.

Evidence binds its Session ID and Trajectory revision. Reusing an otherwise
valid evidence document for a different Session or a later Session revision is
rejected before any outcome dimension is evaluated.

Workspace snapshots are read-only and bounded. They include only regular files,
exclude Git's private top-level `.git` entry, reject symbolic links and special
paths, record a canonical SHA-256 plus byte count, and retain neither absolute
paths nor contents. The evaluator never invokes a Model, Tool, approval action,
or test command. A fixture runner or Host integration captures evidence at the
task boundary and passes only final facts to the evaluator.

`lenso-agent-cli sessions evaluate-outcome --session <id> --criteria <json>
--evidence <json>` is a read-only reporting command. It returns nonzero when a
dimension fails, preserving the existing CLI evaluation convention.

## Ownership

The Session inspection crate owns offline outcome criteria, evidence validation,
snapshot comparison, and report projection. Session Plugins remain the durable
owners of Trajectory facts. Workspace Plugins retain write authority; a test
adapter or fixture runner retains test execution authority; the approval
authority retains proposal and revision semantics. Kernel gains no Agent
evaluation, filesystem, test, or policy behavior.

## Consequences

Fixture repositories can now distinguish a working result from an apparently
healthy session:

- a bug fix requires a graded root-cause finding, the expected file state, and
  a passing regression test;
- a refactor requires the expected structural change and preserved behavior;
- a read-only analysis requires graded findings and a byte-identical Workspace;
  and
- an approval flow requires proposal creation, a current approved effect, and a
  stale approval rejected without an effect.

The report is evidence-driven, not a universal natural-language judge. A
fixture or product integration must supply the task-specific test results and,
for analysis, a trusted mapping from the delivered answer to finding IDs.
