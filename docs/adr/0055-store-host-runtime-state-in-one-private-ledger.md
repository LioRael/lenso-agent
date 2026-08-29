# ADR-0055: Store Host runtime state in one private ledger

Status: Accepted

Supersedes the physical Controller and Generation-record layouts in ADR-0017
and ADR-0035. Their recovery, fencing, and reachability invariants remain in
force.

## Context

The Host previously projected each Controller lineage, immutable Generation
record, and cross-process fence directly into the Agent Home as directories and
lock files. Those paths were implementation details, but their visibility made
them look like user-owned configuration and accumulated one filesystem object
per internal concept. Each surface also had to choose a physical control
directory and concurrency suffix.

The durable facts remain necessary. A restarted Host must recover a safe active
Generation, an admitted Turn must retain its exact Generation provenance, and
collection must not race a live Host. Removing those facts would trade visible
clutter for incorrect recovery.

## Decision

The Agent Host owns one private `runtime/.state/runtime.sqlite3` ledger for:

- revision-fenced Controller state, keyed by a logical surface lineage;
- canonical immutable Generation Specs, keyed by digest; and
- completed automatic-maintenance records.

Surface adapters identify only their semantic kind. The Host allocates any
concurrent lineage slot internally, so callers no longer author directories,
suffixes, or instance limits. SQLite immediate transactions implement the
existing compare-and-swap revision contract.

Process-lifetime and transition fences remain operating-system file locks
because a database row cannot prove that a process has exited. They live under
the private `runtime/.leases/` directory and are not durable authority. The
Runtime State boundary owns both the ledger and these leases. A future
Supervisor may put the same boundary behind IPC without changing surface or
Kernel contracts; this decision does not introduce a daemon.

On first mutating open, the Host validates and imports the previous Controller
directories and Generation files in one database transaction. It then moves
the imported paths to a private recovery staging directory. The staging copy
and old root lock files are removed only after the new Host has completed
recovery successfully. There is no dual write and older binaries are not
supported against a migrated Agent Home.

Read-only inspection opens only an existing ledger. It never creates or
migrates state.

After a clean Host suspension releases its process leases, the Host attempts a
non-blocking collection pass against a fresh Session provenance snapshot. If
another Host is live, maintenance is deferred. Expert preview and explicit
collection commands remain available for diagnosis and repair, while ordinary
use does not require them. `runtime status` reports ledger health and semantic
counts without exposing the physical schema.

Session storage remains owned by its replaceable Session Plugin. Plugin
configuration and artifacts remain owned by the Plugin Root. Resolution
authority remains Host-owned evidence. Kernel receives the same immutable Plan
and does not know about this ledger.

## Consequences

- A normal Agent Home contains one compact runtime ledger and one hidden lease
  directory instead of a directory tree for every runtime concept.
- Controller lineage isolation, revision fencing, exact Generation recovery,
  and provenance-safe collection are preserved.
- Migration is fail-closed for malformed files, symbolic links, conflicting
  records, or a legacy process that still owns a fence.
- Automatic maintenance is safe but opportunistic; a continuously busy set of
  Hosts may defer collection until a later shutdown or explicit operator run.
- SQLite schema changes now require explicit versioned migration.
- A remote Supervisor remains an adapter choice at the Runtime State boundary,
  not a reason to distribute storage knowledge across surfaces.
