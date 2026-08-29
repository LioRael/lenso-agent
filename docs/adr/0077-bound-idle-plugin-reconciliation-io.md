# ADR-0077: Bound idle Plugin reconciliation I/O

Status: Accepted

## Context

ADR-0036 makes filesystem events the normal wake-up path for online Plugin
reconciliation, with a two-second consistency tick as a recovery path. The
tick currently rebuilds the complete canonical Plugin Root snapshot even when
nothing changed. That repeatedly reads every configured Instance and resource
directory while an idle Agent is doing no work.

Skipping consistency checks entirely would make a dropped filesystem event
permanent. Treating file metadata as authority would be worse: timestamps and
sizes do not prove the bytes that become one immutable Generation.

## Decision

Each consistency tick computes a bounded metadata probe over the selected
Profile and visible Plugin Root. An unchanged probe suppresses the canonical
snapshot and resource reads. A changed probe, a probe error, or every thirtieth
tick runs the complete existing reconciliation path under the Host authority
fence. With the two-second tick, the bounded canonical audit remains once per
minute even when filesystem notifications are lost.

If an authoring process owns the exclusive transition fence, the reconciler
keeps the active Generation routable and forces another canonical attempt on
the next tick. A metadata refresh can never turn a transient busy fence into a
minute-long delay.

The probe is only a wake-up hint. It never supplies Plan bytes, resource bytes,
Desired State identity, or admission evidence. Every candidate still comes
from one validated canonical snapshot and passes the Ready Gate before new
Turns route to it. Resource bytes captured by that snapshot are reused during
Generation resolution so the same candidate does not read them twice.

Process-local counters expose metadata probes, canonical snapshots, full
reconcile attempts, and resource-directory reads. They are diagnostic
measurements, not runtime authority or a stable telemetry protocol.

## Consequences

- idle two-second checks perform metadata reads but no repeated full Plugin
  Root or resource reads;
- ordinary changes retain filesystem-event latency;
- metadata-preserving changes missed by the watcher are still discovered by
  the bounded canonical audit;
- probe errors fail closed into the canonical path rather than suppressing it;
  and
- one canonical snapshot is the sole source of candidate Plan and resource
  bytes.

## Proof

Deterministic tests verify that repeated unchanged probes cause no full
reconcile, a changed probe requires one, and Generation resolution reuses the
canonical resource read. The ignored `reconcile_benchmark` smoke reports cold
start, idle read counts, Plugin Root change-to-switch latency, and RSS using
the fixed `lenso.agent.reconcile-benchmark.v1` JSON shape.
