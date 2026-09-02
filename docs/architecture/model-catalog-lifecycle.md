# Model Catalog lifecycle

## Target boundary

Model Catalog content is Provider data, not App Generation identity. A
Generation freezes executable code, configuration, permissions, resources, and
Plugin composition. Each admitted Turn separately freezes the exact Provider
and model facts it will use.

This target replaces catalog-driven Generation churn only after the
characterization tests and architecture review pass. Until then, ADR-0096
describes the implemented behavior.

## Owners

- The Provider Plugin owns authenticated acquisition, validation, bounded cache
  freshness, refresh fencing, atomic publication, and a content-derived catalog
  revision.
- The Host Provider Catalog projection owns the latest validated set of models
  selectable for a new Turn. It publishes one immutable snapshot at a time.
- App Generation owns the Provider implementation, configuration, permissions,
  resources, and Plugin composition. Ordinary catalog additions, removals, and
  metadata refreshes do not change its Spec digest.
- The Turn admission record owns an immutable `ResolvedTurnProfile`. The
  Session Plugin persists that profile in `turn_started` before execution.

## Turn profile ownership

| Field | Source owner | Persistence point |
|---|---|---|
| `provider_id`, `provider_instance` | leased Generation and Host catalog | `turn_started.resolved_turn_profile` |
| `model` | admitted Host catalog snapshot | `turn_started.resolved_turn_profile` |
| reasoning and service-tier controls | normalized Provider model facts | `turn_started.resolved_turn_profile` |
| `limits`, `capabilities`, `wire_protocol` | normalized Provider model facts | `turn_started.resolved_turn_profile` |
| `compaction_compatibility` | projected Provider model facts | `turn_started.resolved_turn_profile` |
| `catalog_provenance` | Provider acquisition and freshness policy | `turn_started.resolved_turn_profile` |
| `catalog_revision` | digest of normalized selectable facts and visibility inputs | `turn_started.resolved_turn_profile` |

Acquisition timestamps and HTTP ETags are cache metadata. They do not change
the content revision unless the normalized projected facts change.

## State transitions

- **A to A:** revalidation updates acquisition metadata only. The content
  revision, Host selectable view, and Generation stay unchanged.
- **A to B:** the Provider atomically publishes B with a new content revision.
  New Turns select from B; admitted Turns retain A; the Generation stays
  unchanged.
- **Model removal:** a new Turn cannot select the removed model. An admitted
  Turn retains its recorded profile. If the upstream Provider no longer serves
  it, completion returns the existing `UnsupportedModel` domain error.
- **Refresh failure:** the last valid snapshot remains selectable while its
  configured stale bound permits it. No partial catalog is published.
- **Restart:** the Provider restores only a valid, account-matching snapshot
  within the freshness policy, then revalidates normally. With no acceptable
  snapshot, startup remains fail-closed until authenticated acquisition
  succeeds.
- **Concurrent admission:** admission reads one immutable Host catalog snapshot
  and copies its revision and complete model profile. A concurrent refresh may
  affect the next admission, never the profile already copied.

## Invariants

The Provider instance in a Turn profile must belong to the leased Generation.
The catalog revision must be a valid content digest from the same admission
snapshot; callers cannot forge or mix profiles across Providers. Dynamic model
candidates share the leased Provider instance and one admission snapshot, while
each candidate carries the revision used to derive it.

Provider code, configuration, permission, resource, and Plugin Root changes
still cross the Generation Ready Gate. Catalog refresh does not add timers or
mutable graph behavior to the Kernel.
