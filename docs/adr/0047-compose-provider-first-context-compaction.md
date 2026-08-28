# ADR-0047: Compose provider-first Context Compaction

Status: Accepted

## Context

The Agent Loop previously rebuilt model context from only the newest
`max_history_events` Session events. That bounded the request, but it silently
discarded older completed turns and gave a third-party summarizer no stable
place to participate.

Session persistence, Memory, and Context Compaction have different authority.
The Session is the canonical append-only fact log. Compaction derives a
bounded model-context projection for one Session. Memory may later curate
knowledge across Sessions. Sharing a database must not merge those contracts.

## Decision

The Harness defines portable, replaceable
`lenso.agent.context-compaction@1`. Its single `compact` operation receives a
previous summary, complete user/assistant turn pairs, and a target summary
bound. It returns one bounded summary plus an exact retained suffix.

The default `lenso.agent.context-compaction` Adapter is deterministic and
extractive. It needs no network or hidden model request, keeps a configured
number of recent complete turns, normalizes older turns into a bounded
summary, and incorporates the previous checkpoint. A semantic model-backed,
domain-specific, process, Wasm, or remote Adapter can replace it through the
same Capability.

The Agent Loop owns when compaction is required because it owns model-context
assembly. It invokes the selected Adapter after the configured number of new
Session events. It rejects empty or oversized summaries and any retained
messages that are not an exact suffix of the request. An Adapter may summarize
content but cannot replace the recent canonical tail.

Each attempt appends `context_compaction_started` followed by either
`context_compaction_committed` or `context_compaction_failed`. A commit stores
the summary, exact retained tail, source boundary, count, and digest. Original
Session events are never rewritten or deleted. Resume reconstructs context
from the latest valid commit plus later completed turns.

The base System Instruction remains the first, Session-installed instrument.
The compacted summary is lower-authority request context and never edits that
instruction.

## Consequences

- Long Sessions retain useful earlier context instead of relying on a silent
  tail cut.
- File and SQLite Session Adapters persist identical compaction facts.
- Third-party compaction is a real replacement seam without access to Session
  storage internals or Agent Loop state.
- The default is predictable and offline, but intentionally extractive;
  semantic quality is a replaceable Adapter concern.
- The first slice compacts automatically. A manual surface command may invoke
  the same transaction later without adding another contract.
