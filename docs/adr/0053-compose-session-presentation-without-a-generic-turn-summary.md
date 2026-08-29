# ADR-0053: Compose Session presentation without a generic Turn summary

Status: Accepted

## Context

Session surfaces need a stable title and a short latest-turn preview. The Web
surface previously listed Sessions, read each complete Session again, and used
the first user input as its title. That made listing an N+1 history scan and
left every other surface to invent the same projection.

The Harness already has two semantic summarization boundaries. Context
Compaction produces bounded model context for one Session, while Memory curates
knowledge across Sessions. A generic `turn-summary` role would overlap both
without naming a distinct consumer or authority.

## Decision

The Harness defines portable, replaceable
`lenso.agent.session-presentation@1`. Its single `project` operation receives
one completed Turn plus the current Session title. It returns a bounded title
and latest preview. It receives no Session store, complete transcript, Tool
internals, or mutable Agent Loop state.

The default `lenso.agent.session-presentation` Adapter is deterministic and
local. It normalizes whitespace, derives the first title from the user input,
preserves an existing title exactly, and bounds title and preview lengths. A
model-backed, process, Wasm, or remote Adapter can replace it through the same
Capability. The optional linked `lenso.agent.session-presentation.model`
Adapter uses its Plan-bound `lenso.agent.model@2` provider and independently
configures the model request, instruction, temperature, token bound, and output
bounds. It never opens provider credentials or transports directly.

The Agent Loop invokes at most one selected presentation Adapter while closing
a successful Turn. A missing Adapter, rejected projection, or Runtime Failure
is ignored and cannot change the Turn outcome. The Host also rejects any
response that changes an existing title, retaining final title authority
outside the provider.

Each successful projection is stored as an optional `presentation` value on
the canonical `turn_completed` event, so the Turn outcome and its display
projection commit atomically without expanding the Session event vocabulary.
File and SQLite Session Adapters project those fields directly through
`session.list`; surfaces do not reread Session histories. The presentation Slot
is optional and replaceable, and the Agent Loop consumes it through a
zero-or-more binding so deleting the selected Plugin leaves the Agent runnable
without automatic presentation metadata.

A user rename is authoritative Session metadata, not another presentation
projection and not a synthetic Turn event. `lenso.agent.session@1` therefore
exposes a `rename` request with an independent title revision fence. Session
Adapters persist the normalized manual title outside the event log, prefer it
over projected titles when listing, and keep the conversation revision
unchanged. This lets surfaces reject stale concurrent edits without confusing
title changes with conversation history.

No generic `turn-summary` Capability is introduced. Model-context summaries
remain owned by Context Compaction, cross-Session extraction remains owned by
Memory, and presentation previews remain display metadata.

## Consequences

- TUI, Web, headless, and channel surfaces can consume the same title and
  preview projection.
- Session listing stays proportional to the list query rather than triggering
  one complete Session read per result.
- Profiles may select different presentation Adapters or configurations.
- A Profile may select the model-backed Adapter and its configured model ID;
  the selected Model provider must admit that primary or explicitly allowlisted
  request model.
- Projection is best-effort and cannot invalidate a durable answer.
- User-edited titles survive automatic projections and backend-neutral Session
  export/import.
- Removing the Plugin removes automatic title/preview generation without a
  Kernel feature branch or mandatory fallback implementation.
