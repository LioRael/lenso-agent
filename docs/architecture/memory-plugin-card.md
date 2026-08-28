# Memory Plugin card

Status: implementation baseline for durable cross-Session Memory.

## `lenso.agent.memory.sqlite`

- **Deletion boundary:** removing this Adapter removes curated cross-Session
  knowledge, FTS search, explicit remember/forget behavior, and its database.
  Session persistence, compaction, System Instruction, Model, and Agent Loop
  remain separate Plugins. A valid composition selects another Memory Adapter.
- **Provides:** `lenso.agent.memory@1` (`observe`, `recall`, `remember`,
  `forget`).
- **Requires:** none.
- **Configuration:** SQLite database path, logical scope, active-record limit,
  item character bound, recall count bound, and recall character bound.
- **Owned durable facts:** normalized memory content, content identity,
  confidence, source Session/Turn provenance, timestamps, scope, and deletion
  state.
- **Owned behavior:** SQLite schema verification, FTS5 indexing, bounded
  retrieval, deduplication, soft deletion, and oldest-active capacity pruning.
- **Does not own:** canonical Session events, compaction checkpoints, System
  Instruction, model requests, Profile selection, or final prompt ordering.

## Agent Loop integration

1. Start the Turn and durably record its user input.
2. Invoke `recall` with the input and configured request bounds.
3. Validate the response and append `memory_recalled` or
   `memory_recall_failed`.
4. Place valid items below the System Instruction as untrusted assistant
   context.
5. After a complete answer, invoke `observe` with source Session/Turn
   provenance.
6. Append `memory_committed` or `memory_commit_failed` together with
   `turn_completed`.

## Third-party Adapter contract

A hosted vector store, MCP-backed product, organization knowledge system,
native Rust Plugin, Wasm Plugin, process Plugin, or remote Plugin can satisfy
the same portable contract. It owns ranking, extraction, consolidation, and
storage. It cannot edit Session history or raise recalled text to instruction
authority. The Capability package includes a provider conformance fake that
uses no Agent Loop or SQLite implementation types.
