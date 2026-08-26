# ADR 0030: Compose unified Tool Hooks and one-shot approval

## Status

Accepted.

## Context

Direct Tool calls, Code Mode nested calls, and child-Agent calls currently reach two explicitly
composed Tools providers. Adding approval separately to Agent Loop, Code Mode, or each Tool would
create divergent policy and let a new call path bypass enforcement. Putting product approval in the
Kernel would make an Agent workflow part of the portable runtime.

## Decision

The Harness defines portable `lenso.agent.tool-hook@1` with ordered `before_execute` and
`after_execute` request Operations.

- Both the root Tool Runtime and the restricted read Tools Module consume explicitly bound `many`
  Hook providers. With no Hook bound, existing Tool behavior is unchanged.
- Every pre Hook sees one Runtime-owned execution ID, exact Tool name, and normalized JSON
  arguments. Hooks run in Plan order. `deny` dominates `ask`, which dominates `allow`; a later Hook
  cannot widen an earlier decision. Hook Domain or Runtime failure stops the pre phase and fails
  closed; Hooks that already returned receive a terminal failure observation.
- Provider execution starts only after all pre Hooks allow. Tool Providers still parse their full
  schema, canonicalize resources, and make final authorization decisions; a Hook is not a
  replacement for provider authority or an OS sandbox.
- Every Hook that successfully participated in pre-execution receives one terminal post observation, including
  success, Tool Domain Error, or Runtime Failure. Post Hooks cannot undo effects or create a second
  terminal Tool outcome.
- The reviewed `approval` Plugin binds one durable Approval Hook to both Tools consumers. Its
  policy is exact-name allow/ask/deny with a fail-closed default. `ask` creates an exact,
  Generation-bound pending action. An operator approves or rejects the ID through the product CLI.
  Approval is consumed once on a later exact retry and never becomes an ambient or indefinite
  capability grant.
- Approval storage is owned by the Module and fails closed when unavailable or corrupt. Records
  retain exact Tool arguments to identify the action, so credentials must not be passed through
  Tool calls.

## Consequences

Enabling or disabling approval stages and switches an immutable App Generation; no running Kernel
graph is mutated. Root mutation/process calls, outer `delegate` and `run_code` calls, Code Mode
nested reads, and child reads traverse the same Hook Capability. The first product slice uses an
explicit approve-then-retry workflow rather than pausing a live Turn. Interactive surface prompts,
argument-aware permission actions, provider-side canonical resource keys, result replacement, and
OS sandbox escalation are later compatible additions.

## Rejected alternatives

Agent-Loop-only approval misses Code Mode and non-Agent Tools consumers. A global Hook registry
bypasses Plan bindings. Treating Plugin admission review as per-call approval conflates installation
authority with effects. Waiting indefinitely inside a Tool call pins a Turn and App Generation and
does not survive process loss truthfully.
