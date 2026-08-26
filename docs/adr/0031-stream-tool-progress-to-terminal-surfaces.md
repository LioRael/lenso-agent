# ADR 0031: Stream Tool progress to terminal surfaces

- Status: accepted
- Date: 2026-08-26
- Relates to: ADR 0001, ADR 0004, ADR 0020, ADR 0023, ADR 0024

## Context

The Agent Turn Stream already reports Tool start and terminal success or failure,
but the Tool Runtime, Tool Provider, and private Process Capabilities return one
terminal Request result. A long-running command therefore produces no observable
stdout or stderr until it exits. The TUI can animate a running card, but it cannot
truthfully reproduce live command output.

Changing the existing Tool Provider `execute` Request into a Stream would break
every native and guest Provider. Process callbacks or a TUI-owned registry would
bypass explicit Capability bindings and make UI details execution authority.

## Decision

Add optional portable Capability `lenso.agent.tool-progress@1`, Descriptor
`1.0.0`. A Provider advertises progressive Tool names through
`progress_catalog`, then opens `execute_progress` as one bounded Stream. Messages
are ordered `stdout`, `stderr`, or one `completed` result. The Stream ends in
exactly one success, Tool Domain Error, or Runtime Failure. Progress is volatile
delivery and is not durable Session evidence.

Evolve `lenso.agent.tools@2` compatibly to Descriptor `2.1.0` by retaining the
existing `execute` Request and adding `execute_stream`. The Tool Runtime consumes
zero or more explicitly bound Tool Progress providers. It rejects progress names
outside the resolved Tool catalog and duplicate progress routes. With no progress
provider, it executes the existing Request and emits one `completed` message, so
existing third-party Tool Providers remain valid.

Evolve private native `lenso.agent.process@1` to Descriptor `1.1.0` by retaining
`run` and adding `run_stream`. The native Process Module reads bounded stdout and
stderr incrementally while preserving cancellation, timeout, process-group
termination, and the combined output limit. The Process Tools Module provides
both Tool roles from one source-first factory.

Introduce `lenso.agent@2`, Descriptor `2.0.0`, because adding `tool_progress` to
the closed `kind` enum is a breaking wire change. Its Turn Stream adds
`tool_progress` with a typed `stdout` or `stderr` channel and Tool call identity.
The Agent Loop forwards progress while keeping `tool_requested` and final
`tool_result` as durable Session events. The TUI appends chunks to the active
Tool card and replaces them with the canonical completed result at termination.

All Streams use Runtime backpressure, half-close, cancellation, and one explicit
terminal outcome. App Composition and the Host Profile Catalog own optional
progress bindings; Kernel and Execution Adapters gain no Agent-specific registry.

## Consequences

- Long-running commands become observable before exit without making progress a
  durable replay contract.
- Existing Tool Providers continue through the aggregate Stream's completion
  fallback.
- A Provider cannot claim progress for an unknown Tool or collide with another
  progress route.
- Removing the Process Modules and both Tool Runtime bindings removes local
  command execution and progress without changing the Agent Loop or TUI.
- Guest progress Providers still require Adapter Stream conformance evidence
  before product admission.

## Rejected alternatives

Changing Tool Provider `execute` from Request to Stream is a breaking interaction
change. A Lenso Event is wrong because execution is one ordered, cancellable
session with a terminal result, not independently admitted fan-out. An Invocation
Context callback would create ambient UI coupling outside the Resolved App Plan.
