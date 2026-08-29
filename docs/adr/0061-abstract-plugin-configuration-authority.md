# ADR 0061: Abstract Plugin configuration authority

## Status

Accepted.

## Context

The Console configuration workflow already separates read-only proposal from
compare-and-swap publication, but the Agent Web Host called local Plugin Root
authoring functions directly. That coupling made the current local storage
mechanism part of the control API implementation and left no explicit seam for
a future remote configuration service.

The running Host still has one stricter invariant: it resolves and stages only
the complete Plugin Root visible to that Host. A remote service response or
remote-authored Plan cannot become a second execution authority.

## Decision

`lenso-app-authoring` exposes a Host-side `PluginConfigurationAuthority` port
with four operations:

- report stable source provenance;
- inspect current desired configuration and semantic revision;
- build a read-only revision-fenced proposal; and
- publish one exact reviewed proposal with compare-and-swap fencing.

`LocalPluginRootAuthority` is the default implementation. It preserves the
existing visible Plugin Root, schema validation, semantic revisions,
cross-process authoring lock, and atomic file replacement.

An embedding Host may inject another trusted implementation into the Agent Web
Surface. A successful implementation must make its complete desired state
atomically observable through the managed Plugin Root before publication
returns. The existing Host reconciler then snapshots that root, resolves it
against the immutable Host Catalog, stages the candidate Generation behind the
Ready Gate, and switches routing only after readiness. The authority port does
not accept a remote Plan and does not stage or switch a Generation.

The Console contracts expose the authority `kind` and stable `reference` on
management, inventory, proposal, and publication responses. This provenance is
descriptive; authorization remains with the Host control seam.

Plugin installation, selection, and removal remain local Plugin Root operations
in this slice. Remote synchronization, distributed coordination, rollout
policy, history, and rollback are separate extensions.

## Consequences

- Console configuration no longer depends on local filesystem functions.
- The local workflow and Generation behavior remain unchanged.
- A future remote adapter has one explicit authoring port and one explicit
  materialization obligation.
- Kernel receives the same immutable resolved Plan and owns no configuration
  client, storage, watcher, or mutable graph.
- A remote service outage fails the configuration operation rather than falling
  back silently to local publication.

## Proof

Repository tests must prove that the local implementation remains read-only
during proposal, publishes only the reviewed revision, and rejects stale
publication. The Agent Web Host must dispatch through a substituted authority,
return its provenance, and retain the existing publication-to-Generation
integration proof for the local implementation. Console decoders must reject
malformed authority provenance and render the selected source.
