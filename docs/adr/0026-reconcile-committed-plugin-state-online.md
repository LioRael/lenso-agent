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

Every running Agent Host owns one bounded online Plugin reconciler beside its existing
`GenerationController`.

- Native filesystem events wake the reconciler after a 200-millisecond quiet period, bounded by a
  two-second settling limit for continuous writes. A two-second consistency scan remains as a
  fallback for missed, coalesced, or unavailable events. Each wakeup snapshots the canonical
  authority; event paths and kinds are never treated as authority.
- For source-backed Apps, the same wakeup snapshots bundled local selection, explicitly installed
  Releases, and the visible `plugins/` directory. The directory is Desired State rather than a
  committed Active Set. Its deterministic fingerprint includes both admitted authority and blocked
  entries. Creation or removal of the directory refreshes the recursive watch.
- Hidden immediate children of `plugins/` are an inert staging namespace. A publisher can assemble
  a complete Bundle there and expose it through one same-filesystem rename; hidden entries never
  contribute authority or quarantine events.
- A non-hidden `.lenso-plugin` file is a packaged Bundle read without extraction. It shares the
  directory Bundle's exact admission and Generation-switch path.
- The reconciler attempts the shared authority fence without blocking. While a Plugin command owns
  the exclusive transition fence, the Host keeps routing its current Generation and retries on a
  later event or consistency scan.
- Long-lived source-backed surfaces use file-backed Supervisors. Before initial activation or an
  online switch, the Host records the exact Generation Spec and resolved Plugin authority. TUI,
  Telegram, Discord, and combined channels use separate Controller directories and process-lifetime
  Host leases, so one surface cannot recover another surface's lineage.
- One changed Active Set is validated and resolved against the exact startup Plan and the Host
  executable identity hashed at startup. Resolution never executes Plugin code to discover
  metadata or bindings.
- The Host records the resolved Generation Spec and its exact recovery authority, then submits one
  `Overlap` transition to the existing Controller. The candidate must pass the ordinary complete
  Ready Gate before the routing epoch advances.
- Existing Turns retain their old Generation Lease. New Turns route to the candidate after the
  atomic switch. The previous Generation has a bounded five-minute drain deadline and a one-second
  rollback window for immediate terminal startup failure. It remains the exact rollback predecessor until that window expires, unless a
  terminal candidate failure restores it first.
- The Controller, not the reconciler, observes Runner terminal Generation health. A terminal active
  failure within the authorized window atomically restores the healthy predecessor and emits one
  operator event. Without an exact healthy predecessor, routing is fenced. Ordinary Turn, Tool, or
  provider request errors are not terminal Generation failures and do not trigger rollback.
- A rejected Active Set, resolution failure, failed Ready Gate, or transition rejection produces one
  bounded operator event and leaves the previous Generation routable. The same rejected Active Set
  is not retried continuously; a later committed Active Set creates a new attempt.
- Malformed, governed, duplicate, or conflicting discovered Bundles are quarantined individually.
  They produce a bounded rejection event but contribute no authority. Valid independent Bundles may
  still stage and switch while quarantined entries remain visible.
- The TUI reports switches, rejections, automatic rollback, and terminal failure fencing. It
  refreshes semantic panel snapshots after a successful switch or rollback. Watcher setup or
  runtime errors are reported as degraded operation while consistency scans continue. The headless
  one-turn CLI uses the same Host path but normally exits before an external authority change is
  useful.

Online overlap transitions authorize one bounded automatic rollback edge. Plugin commands retain
their existing offline maintenance preview before committing Desired State. Online reconciliation
consumes only that already-committed authority.
Source discovery instead rebuilds transient Desired State on each changed fingerprint; it never
writes discovered authority to `active-set.json`.

## Consequences

- A separately running `plugins enable`, `disable`, `upgrade`, `rollback`, or `remove` command can
  change the next Turn of an active TUI without restarting the process.
- Copying or removing an automatically admissible Bundle under a source App's `plugins/` directory
  changes the next Turn of a long-lived Host without restarting it.
- A Turn never migrates between Generations. Session provenance continues to record the exact
  Generation Spec digest leased for that Turn.
- Kernel still receives one immutable Resolved App Plan and owns no Plugin registry, watcher,
  resolver, or mutable binding graph.
- Native Rust availability remains limited to factories linked into the exact Host build. Reviewed
  Wasm Component and QuickJS artifacts may be introduced through their admitted execution shapes.
- Stateful identity changes still require exact State Compatibility Receipts. The Harness supplies
  none in this slice, so those changes fail before routing switches.
- Source-backed long-lived surfaces recover retained Active and Standby Generations after graceful
  restart or an unclean Host exit, then reconcile them with current App files. The one-turn headless
  CLI intentionally remains process-local. `plugins status --verbose` exposes Controller revision,
  suspension, active Generation digest, retained lifecycle, health, activation direction, rollback
  deadline, and retirement reason.
- Policy configuration beyond the fixed one-second source rollback window, a distributed Desired
  State service, distributed coordination, user-facing history pruning, and cross-Host rollout
  remain deferred.

## Rejected alternatives

Rebinding a running Kernel creates a second graph authority and makes Turn provenance ambiguous.
Blocking on the cross-process authority fence can stall active streams while a candidate is being
validated. Switching before the candidate Ready Gate risks replacing a healthy Generation with an
unstartable graph.
