# ADR 0014: Fence Plugin authority across processes

## Status

Accepted.

## Context

Plugin install, remove, upgrade, and rollback already serialized their writes
through `active-set.lock`, but normal App startup did not participate in that
coordination. A second process could therefore begin reading the active
authority while a Ready-before-commit transition was in progress. Atomic file
replacement prevented partial JSON, but it did not close one consistent
Active Set and immutable Store snapshot for startup.

This is Host control-plane coordination. It is not portable Kernel state, a
Module Capability, running-graph mutation, or hostile-code isolation.

## Decision

The Harness Host owns one private `AuthorityCoordinator` over the Plugin
authority root:

- startup and validated Active Set inspection acquire a shared snapshot fence;
- install, remove, upgrade, and rollback acquire an exclusive transition
  fence before reading current authority and retain it through validation,
  Ready Gate work, history recording, and atomic commit;
- a startup snapshot validates and copies its complete Active Set and immutable
  Store closure while the shared fence is held, then resolves and starts that
  immutable snapshot after releasing the file lock; and
- the stable lock file lives at `.lenso/plugins/active-set.lock`. It is opened
  as a regular non-symlink file and uses OS-owned advisory file locking, so
  process exit releases ownership without a stale lock record.

The coordinator has only two operational interfaces: a shared snapshot fence
and an exclusive transition fence. Storage parsing, Plugin policy, App
Generation resolution, and Kernel lifecycle remain with their existing
owners.

## Consequences

- Concurrent startup observes either the complete pre-transition authority or
  the complete committed authority; it cannot observe a mixed snapshot.
- A transition waits for active snapshot readers, and a startup waits for the
  complete Ready-before-commit transaction.
- Crashing while holding the fence leaves atomic authority files unchanged or
  previously committed and lets the OS release the process-owned lock.
- Running Generations are not hot-rebound when authority changes. Their
  immutable resources and Turn leases remain unchanged.
- The fence is local-host coordination over the configured filesystem. It is
  not a distributed lease, network-filesystem portability claim, automatic
  rollback, or permission boundary.
- Retention planning and garbage collection remain separate because they must
  also close Session-to-Generation reachability before deleting immutable
  provenance.

## Rejected alternatives

Locking only writers leaves startup outside the authority transaction.
Holding the fence for the lifetime of a running Generation would serialize
ordinary operation with offline maintenance even though the Generation already
owns an immutable validated snapshot. Moving this state into Kernel would make
portable runtime mechanics own product rollout policy.
