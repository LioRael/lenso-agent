# Outcome-oriented Agent evaluation

An Agent Turn that completes without a Tool error is not necessarily a completed
task. Outcome evaluation joins the durable Session Trajectory with immutable
task-result evidence. It is an offline, read-only check: it never runs a model,
Tool, approval action, or test command itself.

The evaluator reports these independent dimensions:

| Dimension | What it proves |
| --- | --- |
| `workspaceState` | Required files changed (or did not change), expected final digests match, and unrelated changes are rejected. |
| `testResult` | Each named test adapter result reached the expected terminal status. |
| `policyCompliance` | A proposal was created, a current approval had the required effect, and a stale approval was rejected without one. |
| `terminalOutcome` | The durable Session reached the requested terminal trajectory status. |
| `analysis` | Optional trusted finding IDs prove a read-only answer covered the requested facts. |

The report's `passed` field is true only when every applicable dimension passes.
`not_applicable` is explicit for a dimension that a task family does not use.
It also retains the source Trajectory's terminal status, Tool-call count, and
failed-operation count as context; those execution facts never replace outcome
acceptance.

Task-family contracts are deliberately fail-closed. A bug fix or refactor must
name at least one changed path, an expected final digest or absence, and reject
unrelated changes. A read-only analysis permits neither expected nor allowed
Workspace changes. An outcome must name a terminal Trajectory status; it cannot
accept an idle or running Session as a completed task.

## Capturing evidence

A fixture runner or Host integration captures `WorkspaceSnapshot` before and
after the task. The snapshot contains sorted relative paths, a SHA-256 digest,
and a byte count. It rejects symlinks and non-regular files, ignores only the
top-level `.git` entry, and has fixed file-count and byte limits. It carries no
source content or absolute Workspace path. The resulting evidence binds the
exact Session ID and Trajectory revision; evaluation rejects evidence captured
for another Session or an older revision.

The same trusted runner records test terminal facts. It should run the tests
once in the task's final Workspace state, outside the evaluator, then store the
command output digest if reproducibility evidence is useful. The evaluator does
not execute arbitrary commands from JSON.

Approval evidence must come from the authority that owns the proposal. It
contains the proposal ID, current revision, whether the proposal was created,
and each observed approval attempt. A stale rejection counts only when the
attempt used an older revision and no effect was observed. When stale rejection
is required, every observed older-revision attempt must meet that condition; a
single good rejection cannot mask another stale side effect.

For read-only analysis, a deterministic checker or reviewed evaluator maps the
delivered answer to stable finding IDs. The Agent's claim alone is not evidence.

## Run a report

Write a criteria document and an evidence document with their required schema
identifiers, then run:

```text
lenso-agent-cli sessions evaluate-outcome \
  --session session-123 \
  --criteria outcome-criteria.json \
  --evidence outcome-evidence.json \
  --database /path/to/sessions.sqlite
```

The command emits `lenso.agent.outcome-evaluation@1` on standard output and
exits unsuccessfully when any required dimension fails. Existing
`sessions evaluate` remains the compact Trajectory-only compatibility check.

## Fixture coverage

The source tree includes four small fixture repositories in
`crates/lenso-agent-session-inspection/tests/fixtures/outcome-repositories/`:

- `bug-fix`: a whitespace parsing regression must identify the missing trim as
  its root cause, be fixed in the correct source file, and pass its regression
  test;
- `refactor`: a normalization helper must exist while public behavior remains
  unchanged;
- `read-only-analysis`: two configured retry facts must be identified without a
  Workspace mutation; and
- `approval-flow`: the current revision is applied and a stale approval is
  rejected without a second effect.

They are deliberately small and deterministic. They validate the evaluator's
boundaries; they do not claim to benchmark arbitrary model quality.
