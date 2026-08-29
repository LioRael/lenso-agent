# ADR-0078: Project Plugin control as observable operations

Status: Accepted

## Context

Web Plugin mutations change Desired State synchronously, but the immutable
Generation is prepared and switched asynchronously. A response that only says
"accepted" cannot tell an operator whether the candidate is preparing,
active, rejected, or rolled back. Destructively draining reconciliation events
also makes multiple Console consumers race with each other.

Named Profiles make a single active Plugin list insufficient. The visible
Plugin Root can resolve to one Desired Profile selection while the current
Generation remains active and another candidate is preparing.

## Decision

The Web Surface exposes the following versioned projections:

- `GET /api/console/v1/agent/plugins?after=<cursor>` returns
  `lenso.agent.plugin-inventory.v2`, including complete `desired`, `active`,
  and nullable `preparing` selections plus non-destructive lifecycle events;
- every Plugin Root mutation returns `lenso.agent.plugin-operation.v1` with
  one UUID operation receipt and the Profile-aware Desired selection when it
  resolves; accepted work uses HTTP 202 while deterministic authoring
  rejection uses HTTP 409 with the same receipt envelope; and
- `GET /api/console/v1/agent/control/plugin-operations/{id}` returns the
  receipt's latest `accepted`, `preparing`, `switched`, `rejected`, or
  `rolled_back` state.

Event cursors, routing epochs, and operation cursors serialize as unsigned
decimal strings so JavaScript consumers do not lose `u64` precision. Event
reads are bounded and non-destructive. A stale cursor receives `truncated:
true`; legacy in-process consumers retain a private cursor over the same log.
Every cursor-bearing inventory, mutation, operation, and configuration
publication envelope also carries one required process `streamId`. Consumers
discard cached events and local operations when that identity changes rather
than merging cursors from different Host processes.
Every control write carries the inventory identity back as required
`expectedStreamId`. The Host compares it before entering any filesystem or
configuration-authority write path and returns HTTP 409 without side effects
when a request from an older process reaches a restarted Host.

Configuration publication uses the same top-level operation schema. Its
embedded publication payload is identified separately by
`publicationSchema`, and `publicationStatus` cannot be confused with the
operation lifecycle. The Desired `configurationStatus` is derived from that
same receipt: switched is applied, rejected or rolled back is rejected, and
accepted or preparing is pending.

`switched` is a successful mutation outcome for a waiting consumer. The Host
continues observing that receipt during the rollback window so a later
Generation failure can project `rolled_back`. Rejected and rolled-back
receipts no longer change. Receipts and events are bounded process-local
operational state; after restart or expiry an unknown receipt returns 404.
Preparing, switched, and rejected events match a receipt only by the complete
Plugin Root revision, Desired State digest, and Plan digest. A later different
activation closes the old receipt's rollback window, so reactivating an
identical deterministic Generation cannot rewrite an earlier receipt.
The runtime snapshot retains the complete identity of the latest failed or
rolled-back Desired selection independently of the bounded event and receipt
windows. A newer full-identity receipt or Preparing snapshot takes precedence
over older same-root partial rejection evidence.
That exact rejection is paired with the event cursor that observed it. A
receipt consumes it only when the cursor is newer than the receipt's
pre-publication observation baseline. An older rejection of the same
deterministic identity blocks the active-state fallback but leaves a retry
pending until a newer Preparing, Switched, or terminal event arrives. Durable
Controller resynchronization uses its journaled degradation cursor for the
same ordering and clears retained rejection evidence when Controller state no
longer contains a failed Generation.

The runtime also advances a process-local Desired observation epoch for every
valid selection and every rejected authoring attempt. An accepted receipt is
fenced at the post-materialization epoch: a later different selection
supersedes it, while a partial rejection after acceptance terminalizes it with
the retained latest Desired rejection observation. A same-identity recovery
keeps its exact failure fence until Preparing or a durably healthy active
Generation confirms progress. That observation and its cursor outlive the
bounded event window. When an older exact-identity failure and a newer partial
Desired rejection both exist, the newer Desired observation wins. The epoch
is intentionally not part of the wire contract.

Plugin Root control accepts named Profiles. After each atomic authoring
mutation, the actor re-resolves the selected Profile. If it no longer resolves,
the mutation still has an honest rejected receipt with `desired: null`; it is
never reported as an accepted candidate.

The complete staged snapshot, Profile-aware validation, filesystem commit,
post-commit event cursor, and receipt registration hold the same cross-process
authority transition fence used by CLI authoring. The online Host continues
serving the active Generation until the fence drops and then reconciles one
canonical Desired State.

The serialized contract is frozen by
`apps/lenso-agent-web/tests/fixtures/plugin-control-contract.json` and a Rust
golden test. Consumer fixtures should copy that file with provenance instead
of reconstructing the shape from prose.

Management inspection returns an exact-state `ETag` covering the serialized
authoring view, including raw configuration bytes and source digests.
`If-None-Match` returns 304 so periodic freshness checks do not repeatedly
transfer and parse unchanged configuration TOML.

## Consequences

- operators can distinguish what was requested, what is preparing, and what
  currently serves new Turns;
- multiple consumers can poll independently without stealing events;
- Profile-specific control no longer requires falling back to the default App;
- operation history is intentionally bounded and not a durable audit log; and
- management inspection remains the authoring view while inventory is the
  Desired/Preparing/Active runtime view.

## Proof

Tests cover named Profile startup and mutation, successful and rejected
receipts, preparing-to-switched-to-rolled-back transitions, decimal cursors,
process stream changes, complete-identity receipt matching, cross-process
post-commit registration, stale-stream write rejection, terminal identity
retention beyond truncation, same-identity retry fencing, durable Controller
resynchronization, out-of-band same-generation and invalid-authoring
supersession, conditional management reads, non-destructive repeated reads,
truncation, and the shared serialization fixture.
