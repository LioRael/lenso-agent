# Context Compaction Plugin card

Status: implementation baseline for replaceable model-context projection.

## `lenso.agent.context-compaction`

- **Deletion boundary:** removing this Adapter removes context projection;
  Session persistence, System Instruction, Agent Loop, Model, and Memory remain
  separate Plugins. A valid Agent composition selects another Adapter for the
  required seam.
- **Provides:** `lenso.agent.context-compaction@1` (`compact`).
- **Requires:** none for the default extractive Adapter.
- **Configuration:** maximum total input characters, maximum summary
  characters, and complete recent turns to retain.
- **Owned behavior:** deterministic normalization, bounded extractive summary,
  previous-summary incorporation, and complete-turn tail selection.
- **Does not own:** trigger policy, Session reads or writes, System
  Instruction, model requests, Memory extraction, or canonical history.

## Agent Loop integration

The Agent Loop owns the transaction boundary:

1. read the latest valid checkpoint and later completed turns;
2. append `context_compaction_started`;
3. invoke the Plan-bound Context Compaction Adapter;
4. validate the summary bound and exact retained suffix;
5. append `context_compaction_committed` or
   `context_compaction_failed`;
6. send the base System Instruction, compacted summary, recent tail, and new
   user input to the Model.

`max_history_events` is now the automatic compaction trigger. It no longer
means “silently discard everything older than this tail.”

## Third-party Adapter contract

A third-party Adapter can produce a semantic or domain-specific summary, call
a hosted service, or run as native Rust, Wasm, QuickJS, process, or remote
execution. It receives only bounded portable data. Returned retained messages
must be byte-for-byte equal to a suffix of the request and contain complete
user/assistant pairs. Session provenance and durable commit facts remain with
the Agent Loop.

The Host Catalog exposes one replaceable `context-compactor` Slot. Profile or
Plugin Root configuration can select a different provider without changing
the Agent Loop.
