# ADR-0096: Reconcile periodic Model catalog refresh

Status: Accepted

## Context

ADR-0092 acquires the selected Model Provider catalog at the Generation Ready
Gate, and ADR-0095 retains a validated cache with explicit freshness
provenance. The catalog nevertheless changes only when some unrelated Plugin
Root or Profile edit creates a candidate Generation. A long-lived Host cannot
discover a newly admitted model until it restarts or its authoring state
changes.

Refreshing the catalog inside the active Plugin would violate the stronger
Generation boundary: two Turns leased from the same Generation could observe
different model facts. Adding a portable `refresh` request to the Model
Capability would also expose Provider acquisition machinery as product
behavior. Finally, creating a new Generation on every timer tick would give
identical model facts different identities and create unnecessary drain work.

## Decision

The selected direct Codex Provider owns a generation-managed periodic refresh
task. The Host default interval is one hour. Unselected Providers are not
activated and therefore do not refresh.

Every successful acquisition is fully parsed and validated through the same
Provider path used by initial readiness. The Provider atomically publishes one
effective snapshot only when the normalized upstream model facts differ from
the current snapshot. HTTP 304, an equivalent successful response, and any
failed refresh preserve the published snapshot. A process-local publication
generation prevents a task retained by a draining Generation from overwriting
the snapshot published by its successor.

The Host watches the effective-snapshot directory alongside author-owned
Desired State. During canonical reconciliation it injects the selected direct
Provider snapshot as a reserved immutable Instance resource. Those exact bytes
therefore contribute to the resolved artifact-set digest and the Generation
Spec. The candidate Provider validates account identity and projects its
catalog only from the injected snapshot at activation. Candidate failure leaves
the active Generation routable, while existing Turns retain their old
Generation lease.

On first startup no effective snapshot may exist. The initial Provider acquires
and publishes one while crossing its Ready Gate. Before the Host becomes
routable, startup resolves once more and performs a maintenance transition when
the new resource changes Generation identity. This prevents a Turn from being
leased from an identity that did not close over its catalog bytes.

The Provider cache remains separate acquisition state. Revalidation timestamps
and ETags may change without rewriting the effective snapshot or creating a
Generation. The normal bounded consistency audit still discovers a changed
snapshot if filesystem notification is degraded.

## Consequences

- long-lived Hosts discover selected-Provider model additions and removals
  without restart;
- unchanged catalogs and refresh failures create no Generation churn;
- the Kernel remains an immutable Plan executor and gains no model registry or
  timer;
- the active Provider catalog never mutates in place; and
- the current snapshot file participates in the same retained-state recovery
  limits as other authoring inputs. Content-addressed snapshot history and
  explicit garbage collection may be added when retained-generation recovery
  expands beyond those limits.

## Proof

Tests verify that equivalent acquisitions do not republish, a superseded
publisher cannot write, changed model facts do publish, and changed snapshot
bytes alter the selected Instance resource identity. Existing Generation
reconciliation tests continue to prove immutable old resource bytes, candidate
rejection, overlap switching, and retry behavior.
