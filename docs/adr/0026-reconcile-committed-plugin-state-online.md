# ADR 0026: Reconcile committed Plugin state online

## Status

Accepted.

## Context

Plugin enable, disable, upgrade, rollback, and removal already validate a candidate Generation
behind a maintenance Ready Gate before atomically committing the Active Set. A running Harness did
not observe that committed authority. Users therefore had to stop and restart the Host even though
the Runtime already supported overlap staging, fenced route switching, Generation Leases, drain,
and durable recovery.

Mutating the active Kernel graph would abandon the immutable Plan authority and could move a Turn
between incompatible Module graphs. Blocking the async Host on another process's Plugin authority
lock would also interrupt an otherwise healthy active Generation.

## Decision

Every running Agent Host owns one bounded online Plugin reconciler beside its existing durable
`GenerationController`.

- The reconciler polls the canonical Active Set every 250 milliseconds. It attempts the shared
  authority fence without blocking; while a Plugin command owns the exclusive transition fence,
  the Host keeps routing its current Generation and retries on the next tick.
- One changed Active Set is validated and resolved against the exact startup Plan and the Host
  executable identity hashed at startup. Resolution never executes Plugin code to discover
  metadata or bindings.
- The Host records the resolved Generation Spec and its exact recovery authority, then submits one
  `Overlap` transition to the existing Controller. The candidate must pass the ordinary complete
  Ready Gate before the routing epoch advances.
- Existing Turns retain their old Generation Lease. New Turns route to the candidate after the
  atomic switch. The previous Generation has a bounded five-minute drain deadline and is retired
  as soon as its final Lease is released.
- A rejected Active Set, resolution failure, failed Ready Gate, or transition rejection produces one
  bounded operator event and leaves the previous Generation routable. The same rejected Active Set
  is not retried continuously; a later committed Active Set creates a new attempt.
- The TUI reports switches and rejections and refreshes semantic panel snapshots after a successful
  switch. The headless one-turn CLI uses the same Host path but normally exits before an external
  authority change is useful.

The transition has no rollback window and does not enable automatic rollback in this slice. Plugin
commands retain their existing offline maintenance preview before committing Desired State. Online
reconciliation consumes only that already-committed authority.

## Consequences

- A separately running `plugins enable`, `disable`, `upgrade`, `rollback`, or `remove` command can
  change the next Turn of an active TUI without restarting the process.
- A Turn never migrates between Generations. Session provenance continues to record the exact
  Generation Spec digest leased for that Turn.
- Kernel still receives one immutable Resolved App Plan and owns no Plugin registry, watcher,
  resolver, or mutable binding graph.
- Native Rust availability remains limited to factories linked into the exact Host build. Reviewed
  Wasm Component and QuickJS artifacts may be introduced through their admitted execution shapes.
- Stateful identity changes still require exact State Compatibility Receipts. The Harness supplies
  none in this slice, so those changes fail before routing switches.
- Automatic rollback, a non-polling Desired State service, distributed coordination, retained
  standby policy, and cross-Host rollout remain deferred.

## Rejected alternatives

Rebinding a running Kernel creates a second graph authority and makes Turn provenance ambiguous.
Blocking on the cross-process authority fence can stall active streams while a candidate is being
validated. Switching before the candidate Ready Gate risks replacing a healthy Generation with an
unstartable graph.
