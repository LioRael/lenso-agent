# ADR-0097: Keep Model continuation an optional affinity hint

Status: Accepted

## Context

WebSocket connection reuse alone does not identify which previous response a
later completion continues. A global last-response ID would mix concurrent
tasks, while putting provider response IDs in the Agent Loop or Session store
would make provider transport state part of durable conversation semantics.

## Decision

Model Descriptor 4.1.0 retains `lenso.agent.model@4` and adds the optional
`continuation_scope` string to `complete` input. The caller chooses an opaque
scope unique to an isolated task; the Agent Loop supplies its random Turn ID.
The field is a hint only. Every request still contains complete messages and
controls, and Providers may ignore the hint without losing correctness.

Only the Provider knows upstream response IDs, connection affinity, and cache
content. The direct Codex Plugin keeps bounded, disposable checkpoints on its
Instance-local sockets. A matching scope is necessary but insufficient: the
full prior input, model, instructions, tools, and controls must match, and the
projected assistant text and ordered Tool call/result pairs must agree before
the Plugin sends only new input and `previous_response_id`.

Compaction, changed controls, other branches, missing output, credential
rotation, socket loss, or eviction discard the optimization. An explicit
`previous_response_not_found` as the first server event permits one full-input
retry on that socket. Unknown acceptance, partial output, and other failures
never trigger a transparent replay. Successful completed responses alone can
become checkpoints. No raw reasoning is persisted by the continuation cache.

The contract remains portable; generated Rust and cross-language wire shapes
come from the annotated source. The compatibility linter accepts the additive
minor change. Existing consumers omit the hint and receive the same behavior.
Non-Codex Providers and auxiliary callers need no transport-specific logic.

## Consequences

- Durable conversation facts remain owned by Session, not a connection cache.
- Agent Loop gains one optional hint, not a Codex protocol implementation.
- App composition, Host Generation routing, and Kernel do not change.
- Continuation is task-local in this slice; cross-Turn persistence, WS
  multiplexing, and mid-response steering are not introduced.

## Proof

Contract lint and generated-projection freshness checks cover 4.0.0 to 4.1.0.
Tests cover full requests without the hint, scope/context/control mismatch,
incremental Tool results, and bounded explicit cache-miss recovery. CLI tests
cross the real Agent/Model/Auth interfaces and execute a Tool between two
responses on one connection.
