# ADR 0005: Bootstrap turns through an App Generation

- Status: accepted
- Date: 2026-08-24
- Relates to: Lenso ADR 0065
- Refined by: ADR 0011

## Context

The Harness previously passed a reviewed `ResolvedAppPlan` directly to Kernel.
That preserved an immutable graph but provided no product authority for Plugin
artifacts, the exact Host build, execution-class policy, or route-pinned App
Generations. Adding any of those concerns to Kernel would make portable runtime
mechanics own product package and rollout policy.

## Decision

The CLI Host owns a bounded Plugin control-plane bootstrap above Kernel. It:

1. opens a content-addressed Plugin Store;
2. constructs a canonical Host Build Manifest from the executable digest and
   statically linked native Module factories;
3. applies a canonical Host Execution Policy that currently permits only the
   stable, trusted `lenso.native-rust@1` class;
4. resolves an empty Plugin lock and closes the already-resolved base Plan,
   including its explicit Provider order, into one exact initial App
   Generation;
5. stages that Generation behind its Ready Gate; and
6. requires one route-pinned Generation lease to remain alive for the complete
   Agent Turn.

The Host keeps the Generation-to-Agent route table private. A lease supplies
the only digest accepted by that table. Process shutdown drains Generation
resources after the Turn lease is released.

## Consequences

- Kernel remains unaware of Plugin identity, stores, policy, and Generations.
- The current Compositions behave identically after deterministic control-plane
  resolution.
- The initial passive-only admission policy is superseded by ADR 0007's one
  exact native Tool Provider profile.
- Wasm Component, QuickJS, and native-dylib packages are transitive preview
  dependencies but are absent from the Host Execution Policy and Adapter
  catalog.
- ADR 0011 records Generation provenance in Session events. Overlap
  replacement, rollback, durable cross-process fencing, provenance inspection,
  and retention require later acceptance slices.

## Rejected alternatives

### Start Kernel directly and record a synthetic Generation ID

That would create provenance without a Generation authority, Ready Gate,
fenced routing, or resource ownership.

### Put a mutable Plugin registry in Kernel

That would move product acquisition and rollout policy into the portable graph
runtime and create a second dependency authority beside the immutable Plan.
