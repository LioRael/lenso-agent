# ADR-0060: Project bounded child-task supervision

Status: Accepted

## Context

The subagent Tool Plugin owns a bounded Generation-local task registry, but its
`list_subagents` output is a model-facing JSON value containing only task ID,
Agent, status, and an observed child Session. TUI and Web cannot consume one
typed surface-neutral projection, and operators cannot relate a task to its
parent Tool call, immutable Generation, Workspace, or terminal result.

Moving the registry or scheduling policy into Kernel would violate the Host
boundary. Letting each Surface reconstruct task facts from Session events would
create several inconsistent owners and would lose running tasks that have not
yet emitted a child Session fact.

## Decision

`lenso.agent.subagent-tools` remains the sole task-fact owner and additionally
provides the native request Capability `lenso.agent.task-supervisor@1` with one
`snapshot` operation. A snapshot returns at most 64 tasks in stable task-ID
order. Each task contains:

- its task ID and named child Agent Instance;
- the parent Session, Turn, and Tool-call owner;
- lifecycle status and observed child Session;
- immutable Generation Spec digest and absolute Workspace identity; and
- an optional terminal result with content bounded to 16 KiB, an explicit
  truncation flag, and a stable reason code when applicable.

The Host attaches the Workspace scope to each root Invocation Context. The
Agent Loop narrows the context for every Tool call with its parent Session,
Turn, and Tool-call identity. The subagent Plugin fails closed when spawning a
task without valid owner, Generation, or Workspace provenance. Detached child
contexts preserve those facts but do not gain authority from them.

`list_subagents` serializes the exact typed projection for the model. It does
not maintain a parallel JSON contract. Waiting remains the only operation that
consumes a terminal result and releases the task slot.

## Consequences

- later TUI and Web work can consume the same Capability without becoming task
  schedulers;
- reconnect does not change the task owner or provenance while the leased
  Generation remains alive;
- task supervision remains volatile across Host suspension and Generation
  switching; durable restart recovery is not implied by this projection; and
- the later Worktree Provider may replace the Workspace identity for
  mutation-capable children without granting filesystem authority to Kernel.

## Proof

Contract generation and freshness checks cover the Descriptor, Schemas, and
Rust runtime projection. Unit tests cover running, cancellation, terminal
projection, provenance validation, and result truncation. Headless integration
proves parent ownership, Generation, Workspace, child Session, and terminal
result flow through the real Agent Loop and Tool Provider path.
