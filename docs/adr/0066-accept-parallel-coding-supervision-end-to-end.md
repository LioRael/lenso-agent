# ADR-0066: Accept parallel coding supervision end to end

Status: Accepted

## Context

ADRs 0059 through 0064 compose named children, bounded task facts, isolated Git
worktrees, background process handles, and read-only TUI/Web projections. Unit
and narrow integration tests prove each boundary, but they do not prove that a
surface can supervise two real mutation lanes through an explicit reviewed
integration workflow.

A root Turn Run Scope is also not valid child authority. Copying its selected
Tool names into a child can request root-only Tools outside the child's
Plan-bound catalog and fail the child before its first progress event.

## Decision

The Task Supervisor contract major 2 adds optional bounded progress. The
subagent owner updates it after each child Agent message with monotonic message,
text-delta, and Tool-call counters plus at most 4 KiB of text. Surfaces remain
read-only consumers.

Detached child contexts retain Generation and Workspace provenance but omit the
parent Tool-call owner and root Run Scope. Each child is constrained again by
its own immutable Agent/Tools bindings and the composed approval Hook; no
authority is inferred from the parent surface's Tool selection.

`review_worktree` includes its locked commit and diff SHA-256 in model-visible
content. `integrate_worktree` continues to fail closed unless those exact values
match the retained clean review.

## Consequences

- root-only Tool selection no longer prevents a valid child Agent from opening;
- reconnecting clients can observe bounded progress without owning scheduling;
- the parent Workspace remains unchanged until an explicit approval and exact
  review; and
- the acceptance flow exercises existing Plugin authority rather than adding a
  Host or Kernel integration shortcut.

## Proof

The Web end-to-end test starts two mutation children concurrently, waits until
both expose terminal bounded progress, and reads the identical snapshot through
a new client while the parent Turn is blocked on approval. It proves the two
Workspace paths are distinct and the parent has neither file. After the test
answers the approval, Session evidence places review calls before integration
calls, contains both locked review values, and the parent contains both merged
files. Unit coverage proves detached child contexts strip root Run Scope while
preserving Generation provenance and independent cancellation.
