# ADR-0080: Default Agent Loop cumulative quotas to unlimited

Status: Accepted

## Context

The Agent Loop originally required small step and Tool-call quotas, and user
resume boundaries defaulted to eight. Those values made a safe fixture easy to
bound, but they also made an ordinary coding Turn fail because of Harness
policy rather than a model, context, cancellation, or provider boundary.

A user answer can legitimately start another autonomous execution segment in
the same Turn. Resetting every counter at that boundary would let strict
deployments bypass a Turn-wide quota. Conversely, applying only one cumulative
quota prevents a deployment from bounding each unattended segment.

Concurrency is a different concern. The Host still needs a finite scheduler
capacity even when it does not impose an arbitrary cumulative Tool-call quota.

## Decision

The Agent Loop separates optional cumulative execution quotas from required
runtime capacities.

The following configuration fields are optional. Omitting one means that the
corresponding cumulative quota is unlimited:

- `max_steps` bounds model steps in one autonomous execution segment;
- `max_tool_calls` bounds Tool calls in one autonomous execution segment;
- `max_user_resumes` bounds accepted user-resume boundaries in one Turn;
- `max_total_steps` bounds model steps across the complete Turn; and
- `max_total_tool_calls` bounds Tool calls across the complete Turn.

An accepted user input resets only the segment step, Tool-call, and output
budgets. Turn-total step and Tool-call counters remain monotonic. A managed
Host or user configuration can set both segment and Turn-total quotas when it
requires strict enforcement.

`max_steps` and `max_total_steps` reject zero because a Turn could never issue
its first model request. `max_tool_calls = 0` and
`max_total_tool_calls = 0` are valid explicit ways to prohibit Tool execution.
`max_user_resumes = 0` permits the initial autonomous segment but rejects a
resume after user input. There is no numeric sentinel for unlimited; omission
is the only unlimited representation.

The default Host omits these five quotas. Existing explicit values retain
their strict meaning. `max_parallel_tool_calls` remains a required finite
scheduler capacity and does not count cumulative Tool usage. Output-token,
history, compaction, memory, provider, cancellation, approval, and Tool
authority boundaries remain independently enforced.

This decision refines ADR-0046: user interaction still renews an autonomous
segment, but repeated interaction is unlimited by default and is bounded only
when `max_user_resumes` is explicitly configured.

## Consequences

- ordinary Agent Turns no longer fail at an arbitrary default step, Tool-call,
  or resume count;
- strict and managed deployments can independently bound one autonomous
  segment and the complete Turn;
- answering a question cannot evade a Turn-total execution quota;
- unlimited cumulative work does not imply unlimited concurrency, context,
  output, authority, or wall-clock execution; and
- configuration schemas distinguish omitted unlimited quotas from explicit
  zero-denial policy.

## Proof

Tests prove that the default Host executes more than sixteen sequential Tool
calls, explicit Tool-call quotas reject the first excess call, user answers do
not reset Turn-total Tool-call usage, explicit zero user resumes terminate at
the first resume boundary, and omitted user-resume limits permit repeated
renewals.
