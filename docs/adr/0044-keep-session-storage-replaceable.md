# ADR-0044: Keep Session storage replaceable and inspection neutral

Status: Accepted

## Context

The Agent Loop already depends only on `lenso.agent.session@1`, but Host
provenance and Generation GC imported file-Plugin functions and interpreted its
private JSON store. That made the storage seam hypothetical: replacing runtime
persistence still broke operational tooling.

Engineering-grade local persistence also needs transactional multi-event
appends, indexed reads, and safe concurrent process access. A file Adapter is
still useful for transparent fixtures, export, and small installations.

## Decision

File and SQLite are ordinary Session Adapters providing the same
`lenso.agent.session@1` `open`, `read`, and `append` interface. Both preserve:

- append-only contiguous revisions;
- optimistic `expected_revision` checks;
- atomic event batches;
- byte-identical event-ID idempotency;
- ordered bounded reads; and
- fail-closed durable storage errors.

The SQLite Adapter uses a normalized strict schema, foreign keys, WAL, full
synchronous commits, a five-second busy timeout, unique per-Session event IDs,
and one immediate transaction per append batch.

Offline operational tooling uses the small `SessionInspector` interface. Each
Adapter converts its private format into complete normalized Session facts;
shared validation and Turn provenance projection happen above that interface.
The Host selects a file directory or SQLite database without importing either
private representation.

SQLite is selectable through normal Profile and `plugins/` configuration. The
existing file Adapter remains the default until an explicit import/export
workflow can migrate existing stores without silently hiding Sessions.

## Consequences

- Adding a remote or hosted Session Adapter no longer requires teaching
  provenance and Generation GC its storage format.
- SQLite and file stores can be tested against the same externally observable
  Session semantics.
- Offline inspection is intentionally read-only and validates complete Session
  histories before projecting provenance.
- Store migration remains explicit future work rather than an implicit default
  switch.
