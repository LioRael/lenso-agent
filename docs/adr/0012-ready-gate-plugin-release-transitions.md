# ADR 0012: Ready-gate Plugin Release transitions

## Status

Accepted.

## Context

`plugins install` intentionally refuses to replace an active immutable Release.
Writing a candidate Active Set before proving its Generation ready would make
the next App start depend on an unverified graph.

## Decision

The Harness admits an offline maintenance transaction:

1. `plugins upgrade` holds the authority lock and compare-and-swaps the active
   Manifest against explicit `--expected-manifest` authority;
2. admission stores the candidate without changing the Active Set;
3. the Host resolves current and candidate authorities against the reviewed
   base Plan;
4. `GenerationSupervisor` performs a `Maintenance` transition and requires the
   candidate Kernel and Agent route to become ready;
5. only after a clean Ready Gate and cleanup does the Host content-address both
   Active Sets and atomically commit the candidate; and
6. `plugins rollback --to <active-set-digest>` follows the same path for an
   exact retained authority.

The command process is an offline preview and transaction coordinator. It does
not keep the candidate Generation running after commit. Normal App startup
resolves and starts the committed authority again.

## Consequences

- CAS mismatch, invalid Plan, failed Ready Gate, dirty shutdown, missing
  rollback record, or tampered history leaves `active-set.json` unchanged.
- Admission may leave immutable Store objects after an unsuccessful upgrade;
  they have no activation authority.
- Active Set records live under `.lenso/plugins/active-sets`.
- Kernel still receives one immutable Plan and knows nothing about replacement.
- ADR 0014 closes local cross-process startup/transition fencing. Overlap,
  automatic rollback, hot loading, distributed coordination, history retention
  policy, and garbage collection remain deferred.

## Rejected alternatives

Committing before the Ready Gate abandons known-good authority too early.
Rebinding a running Kernel creates a second mutable graph authority.
