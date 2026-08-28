# ADR-0048: Compose provider-first Memory

Status: Accepted

## Context

Session persistence, Context Compaction, and Memory have different jobs. A
Session is the canonical event log. Compaction projects one long Session into
bounded model context. Memory curates useful knowledge across Sessions. Using
SQLite for more than one concern must not merge their Interfaces or authority.

The Harness also needs a useful offline default without reserving a private
Agent Loop path that a third-party hosted, MCP-backed, process, Wasm, or remote
Adapter cannot use.

## Decision

The Harness defines portable, replaceable `lenso.agent.memory@1` with four
request operations:

- `observe` lets the selected Adapter curate one completed Turn;
- `recall` returns bounded relevant items with source provenance and
  confidence;
- `remember` stores explicit user- or tool-selected knowledge; and
- `forget` explicitly deletes selected logical items.

The default `lenso.agent.memory.sqlite` Adapter owns one SQLite database, FTS5
index, configured scope, deduplication, provenance rows, capacity enforcement,
and soft-deletion state. Automatic observation stores one normalized completed
Turn with moderate confidence. Explicit memories retain the caller's bounded
confidence. Storage failure is a Runtime Failure and never falls back to an
ephemeral map.

Before each model request, the Agent Loop asks the bound Memory Adapter to
recall against the new user input. Returned items are validated against count,
character, identity, provenance, and confidence bounds, then inserted below
the installed System Instruction as visibly untrusted request context. A bad
or unavailable recall produces `memory_recall_failed`; it never becomes a
System Instruction.

After a successful model result, the Agent Loop asks Memory to observe the
complete Turn. It appends `memory_committed` or `memory_commit_failed` in the
same Session append as `turn_completed`. A transient Memory failure therefore
remains visible without changing an already successful answer into a failed
Turn. Repeated observation is idempotent through content identity and source
provenance.

The Host Catalog exposes one replaceable `memory` Slot. Plugin instance
configuration selects the database and scope, so two Profiles can use the same
Plugin code with isolated stores or policies.

The optional `lenso.agent.memory.command` Adapter provides a concrete
third-party/remote seam. It sends `memory.observe`, `memory.recall`,
`memory.remember`, or `memory.forget` through one bounded JSON stdin/stdout
exchange with an exact executable, cancellation, timeout, and response-size
limits. A deployment may point that executable at HTTP, MCP, an embedding
service, or another store without coupling those transports to the Agent Loop
or SQLite implementation.

## Consequences

- A fresh default Harness gains durable cross-Session recall without a hosted
  embedding dependency.
- Third-party Adapters implement one portable Interface and need no Agent Loop,
  Session table, or SQLite internals.
- Memory content is lower-authority context with explicit provenance; it cannot
  silently rewrite the Session-installed System Instruction.
- The default extractive Turn observation is intentionally modest. Semantic
  extraction, embeddings, consolidation jobs, organization stores, and MCP
  products remain replaceable Adapter behavior.
- `remember`, `recall`, and `forget` Tool projections can consume the same
  Capability later; they are not a second Memory store or Interface.
