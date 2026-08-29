# ADR-0076: Compare Agent behavior across surface-specific Generations

Status: Accepted

## Context

ADR-0042 intentionally gives CLI, TUI, Web, ACP, and channel executables
different Host Catalogs. Each executable links only its own surface Plugin, so
its immutable Plan and Generation Spec digest must differ. Requiring equal
Generation Spec digests across entrypoints would either make the digest
misrepresent the executable graph or restore a shared ambient surface graph.

Cross-entrypoint acceptance still needs a stable way to prove that one Profile
selects the same Agent behavior, Tool providers, Model, Session, Memory, Prompt,
Hook, and policy dependencies.

## Decision

- Keep the Generation Spec digest as the exact content identity of the whole
  Host-specific immutable Plan. It remains the authority for inspection,
  rollback, retention, and execution provenance.
- At Turn lease time, derive an Agent behavior digest from the selected Agent
  Instance and the transitive provider closure reachable from that Instance.
  Canonical selected Plugin Instance records and bindings inside the closure
  are hashed with SHA-256.
- Exclude consumers outside that provider closure. A CLI, TUI, Web, ACP,
  Telegram, or Discord surface Plugin therefore cannot alter the behavior
  digest merely by changing its own presentation configuration.
- Attach both identities through typed Invocation Context provenance and record
  both in the durable `turn_started` Session event. Existing Session events
  without a behavior digest remain readable.
- Cross-entrypoint acceptance compares the behavior digest and explicit
  RunScope policy. It never substitutes the behavior digest for the exact
  Generation Spec digest.

## Consequences

- Surface executables can remain independently linked and removable while
  proving equivalent Agent behavior.
- Changes to the selected Model, Tool providers, prompts, memory, hooks, or any
  other transitive Agent provider change the behavior digest.
- Surface-local rendering, transport, or configuration changes still change
  the exact Generation Spec digest but do not create false Agent-behavior
  drift.
- Historical Session inspection reports `unavailable` for the new identity
  when reading events written before this ADR.

## Proof

Host tests prove that a surface-only Plugin configuration change preserves the
behavior digest while an Agent configuration change does not. CLI, Web, ACP,
Telegram, and Discord integration tests prove that real Turns durably record a
canonical behavior digest; TUI uses the same Host Turn lease and Agent Loop
path. Agent Loop parsing tests retain backward compatibility for older Session
events.
