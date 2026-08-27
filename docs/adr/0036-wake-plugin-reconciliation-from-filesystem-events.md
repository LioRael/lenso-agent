# ADR 0036: Wake Plugin reconciliation from filesystem events

## Status

Accepted.

## Context

The source-backed Plugin path already treats a visible `plugins/` directory as
Desired State and safely switches immutable App Generations. Its fixed
250-millisecond polling loop made an idle Host repeatedly read and hash the same
files, while a newly copied Bundle could still wait for the next tick. It also
made the user-visible drop-in workflow feel like a background scanner rather
than an ordinary plugin system.

Filesystem notifications cannot replace authority validation. Editors use
different write patterns, operating systems coalesce events, queues can
overflow, and some network or emulated filesystems may not deliver native
events at all.

## Decision

Each long-lived Harness reconciler owns a platform-recommended filesystem
watcher at the Runner and App Generation control boundary.

- Committed Plugin state watches its authority root non-recursively.
- A source-backed App watches the App Definition parent and acquired-Release
  root non-recursively, plus the visible `plugins/` directory recursively when
  it exists. Creating or removing `plugins/` refreshes that recursive watch.
- Any filesystem event is only a wakeup. The Host waits until events have been
  quiet for 200 milliseconds, with a two-second upper bound for continuous
  writes, then rebuilds the complete canonical Desired State snapshot and
  compares its deterministic digest. It never interprets an event path or kind
  as Plugin authority.
- Hidden immediate children of `plugins/` are an inert staging namespace. A
  publisher may copy or extract a complete Bundle there, then expose it through
  one same-filesystem rename. Hidden entries contribute neither authority nor
  quarantine output.
- A delayed two-second consistency scan runs through the same snapshot path.
  It covers event loss, watcher setup failure, unsupported filesystems, and
  external changes that do not produce a usable notification.
- Watcher setup and runtime errors do not stop the active Generation. They emit
  a bounded, deduplicated degraded event; the TUI explains that consistency
  scans remain active.
- Admission, quarantine, exact authority recording, Ready gating, overlap
  switching, Turn leases, rollback, and recovery remain unchanged.

The watcher remains Host control-plane machinery. Kernel still executes one
immutable Resolved App Plan and owns no filesystem watcher or mutable Plugin
registry.

## Consequences

- Copying, editing, or removing a safe drop-in Bundle normally wakes
  reconciliation immediately without idle high-frequency polling.
- Correctness does not depend on notification completeness. The worst expected
  detection delay in degraded local operation is the consistency interval.
- One noisy editor save may emit many events, but the debounce and Desired
  State digest collapse them into one meaningful attempt.
- Ordinary short direct-copy bursts settle before discovery. Arbitrarily slow
  or paused copies are not transactional; callers that require that guarantee
  use the hidden staging namespace and an atomic rename.
- Recursive watching is limited to the explicit discovery directory rather
  than the entire App tree.

## Rejected alternatives

Treating notification paths as exact changes creates a second, lossy authority
model. Removing periodic consistency checks would make network filesystems,
queue overflow, and watcher failure silently stale. Recursively watching the
whole App tree would increase noise and resource use without adding authority.
