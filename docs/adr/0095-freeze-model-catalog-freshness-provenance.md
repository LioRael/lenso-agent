# ADR-0095: Freeze Model catalog freshness provenance

Status: Accepted

The freshness policy remains current; its Generation-owned catalog revision
was superseded by the
[Model Catalog lifecycle](../architecture/model-catalog-lifecycle.md).

## Context

ADR-0092 lets the selected Model Provider acquire and freeze its catalog at the
Generation Ready Gate. A remote acquisition currently has no retained prior
snapshot, conditional validator, or typed indication of whether the catalog was
read live, revalidated from cache, or admitted under a bounded stale policy.
Consequently a transient catalog outage rejects every candidate even when the
same Provider previously validated usable facts, while a future cache fallback
could not be made visible to catalog or Turn consumers.

The cache is Provider acquisition state. It must not become a mutable Kernel
registry, activate unselected Providers, or change a routable Generation.

## Decision

`lenso.agent.model@4` adds required catalog provenance with these normalized
facts:

- acquisition source: `live`, `cache`, or `configured`;
- freshness: `fresh`, `revalidated`, or `stale`;
- optional fetch and validation Unix timestamps;
- an optional bounded Provider revision such as an HTTP ETag; and
- the optional maximum stale age governing cache admission.

The Host validates the combinations, freezes the result with the selected
Provider catalog, projects it through `lenso.agent.provider-model-catalog.v3`,
and copies it into every resolved Turn profile. The Generation Spec digest
remains the catalog revision and execution authority; Provider provenance
explains how the model facts for that Generation were acquired.

The direct Codex Provider owns one Host-configured cache file. A successful
live response is fully parsed and validated before atomic publication. A later
request sends `If-None-Match` when the retained snapshot has an ETag. HTTP 304
revalidates the cached body. A transport error or transient HTTP status may use
the prior validated snapshot only while its age is within the configured
maximum. Authentication failures and invalid live responses fail readiness.
Mismatched cache identity and over-age snapshots are never admitted, so they
also fail readiness whenever a fresh acquisition is unavailable. Stale
admission is always explicit.

Configured and fixture Providers publish `configured/fresh` provenance without
invented fetch times or remote revisions. Unselected Providers remain
unactivated and retain `unchecked` readiness.

## Compatibility

Adding required facts to the strict Provider response changes what existing
Providers and consumers must exchange. Compatibility lint must therefore reject
`@3` to `@4` as compatible; all in-tree Providers and consumers migrate
atomically.

## Consequences

- transient catalog outages can preserve service only through a bounded,
  previously validated snapshot;
- operators and durable Turns can distinguish live, revalidated, stale, and
  configured facts;
- an old or superseded acquisition cannot mutate an active Generation; and
- periodic refresh remains Generation reconciliation rather than mutable
  per-Turn state.

## Proof

Tests cover live cache publication, ETag revalidation, bounded transient
fallback, rejection of expired and identity-mismatched cache entries, public
catalog projection, Turn provenance, generated freshness, and the required
Capability major-version incompatibility.
