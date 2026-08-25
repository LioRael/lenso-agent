# ADR 0017: Run the Host through durable Generation control

## Status

Accepted.

## Context

ADR 0005 introduced route-pinned Agent Turns, but the product Host implemented
its own in-memory `GenerationSupervisor`, route table, Kernel startup, and
cleanup. ADR 0012 reused that implementation only as an offline Ready preview.
The generic Runtime now provides durable Generation authority, a bounded
Controller, fenced routes, terminal-failure maintenance, and recovery. Keeping
the product copy would bypass those guarantees and make later overlap or
automatic rollback require a second implementation.

A normal CLI exit also differs from retiring a Generation. The committed
Plugin authority still selects the same reproducible Generation on the next
invocation. Marking it permanently retired would either prevent restart or
force the Host to invent a new digest without changed authority.

## Decision

The Agent Host uses the Runtime-owned `KernelGenerationRuntime`,
`DurableGenerationSupervisor`, and `GenerationController` as its only live
Generation path.

- A stable product-surface namespace stores each fsync'd compare-and-set control
  record separately from immutable Generation Specs and Plugin authority. The
  companion headless CLI uses `.lenso/plugins/generation-control`; the
  independently composed `lenso-agent` TUI uses
  `.lenso/plugins/tui-generation-control`. They share Plugin authority but do
  not recover each other's Controller lineage.
- Startup resolves and records the exact committed Generation and retains a
  content-addressed recovery copy of its Plugin authority, separate from the
  user-visible rollback history and Generation GC roots. It opens a new durable
  control state when none is live, or recovers every durable Active or Standby
  digest from retained exact Plugin authority. One shared Plugin authority
  fence covers resolve, recovery, Ready, and switch. When committed authority
  changed while the CLI was stopped, startup performs a standard maintenance
  transition from the recovered Active Generation to the newly resolved
  Generation before routing.
- Every Turn acquires a Controller route. That route owns the durable Generation
  Lease and supplies both the Agent handle and the Generation Spec digest
  recorded in Invocation Context.
- Controller maintenance continuously reconciles terminal Kernel failure,
  routing epochs, drain deadlines, standby expiry, and authorized rollback.
- Normal CLI exit uses Host suspension: it refuses active Turn Leases, shuts
  down process-local Kernel resources, and preserves the durable Active and
  Standby authority for exact recovery. Explicit Generation shutdown remains
  the separate retirement operation.
- Offline upgrade and rollback validation use the same standard Controller over
  an in-memory control store, then retire the preview after the Ready Gate. They
  still do not mutate a different running CLI process.

The Host selects native Rust, Wasm Component, and QuickJS execution classes.
The generated Agent Capability projection supplies the shared typed Stream
codec, and product-owned Artifact profiles allow reviewed Wasm or QuickJS
contributions to replace the exact native Agent Loop. The Runtime-owned
multi-execution catalog consumes the Generation's digest-verified Artifact
catalog and enforces the exact pre-Ready Guest Descriptor and bounded Stream
limits.

## Consequences

- Product code no longer owns a parallel Generation state machine or route
  table.
- A graceful stop and next invocation recover the same immutable Generation
  without changing its digest; crash recovery fences the older Supervisor
  epoch before admitting routes.
- A durable authority mismatch fails startup instead of silently selecting the
  newest Store files or rebuilding a different graph.
- Overlap rollout UX, guest imports for required Host Capabilities, durable
  Session fencing, and distributed leader election remain separate product
  work. None is added to Kernel.

## Rejected alternatives

### Delete control state after every CLI invocation

That discards recovery and fencing evidence and creates an unsafe window around
process failure.

### Add a nonce to every Generation Spec

That makes identical authority resolve to different identities and breaks
content-addressed provenance.

### Treat Host exit as Generation retirement

Retirement is a terminal rollout decision. A process-local stop should release
resources while preserving the committed Generation selection.
