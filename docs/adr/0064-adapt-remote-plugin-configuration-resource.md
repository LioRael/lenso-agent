# ADR 0064: Adapt one remote Plugin configuration resource

## Status

Accepted.

## Context

ADR 0061 defined the Host-side configuration authority port and ADR 0063
provided a durable SQLite implementation. Neither supplied a network contract
for a Host that delegates desired configuration to a remote service. Treating
an endpoint as an unscoped replacement for a local file would leave App and
environment identity ambiguous and could silently publish into the wrong
resource.

The remote service still cannot become a Plan or Generation authority. Each
Host must resolve and ready-gate the complete Plugin Root it actually runs.

## Decision

The Agent Web Host provides an opt-in HTTP adapter for exactly one resource:

```text
service / v1 / apps / {app} / environments / {environment}
```

Plugin configuration operations extend that identity with the Plugin ID and
Instance key. App and environment segments are explicit validated identities;
they are never inferred from the Workspace, process directory, or bearer
token.

The protocol provides inspection, read-only proposal, revision-fenced
publication, publication history, rollback proposal, and ordered desired-change
operations. Requests use a Host-owned bearer token. Non-loopback transport
requires HTTPS, redirects are rejected, responses are bounded, schemas are
exact, and every remote proposal is compared with the Host's independently
resolved proposal.

The service owns desired-state compare-and-swap. After a successful remote
publication, the adapter materializes the same reviewed TOML through
`LocalPluginRootAuthority` and verifies the resulting semantic revision before
returning. A remote outage, protocol mismatch, CAS conflict, or materialization
failure is terminal for that operation. The Host never falls back to local
publication.

Before the first App Generation, the Host publishes its exact Host Catalog and
then synchronizes an ordered transition batch from the visible Plugin Root
revision. Each change contains one complete Plugin Instance TOML document plus
its base and candidate Plugin Root revisions. The Host independently proposes
and materializes every transition. A gap, reorder, semantic mismatch, invalid
cursor, oversized batch, or unavailable service fails closed; the Host never
replaces the whole Plugin Root from an unaudited snapshot.

After startup, a Host-owned background actor long-polls the same change feed.
Successful materialization flows through the existing filesystem reconciler
and Ready Gate before a new immutable Generation is selected. Watch failures
are deduplicated into operator-visible degradation events and retried; shutdown
stops and joins the watcher. The cursor is an optimization held in memory. The
semantic Plugin Root revision is the durable recovery fence after restart.

Remote proposal digests identify remote CAS and audit records. Host proposal
digests identify the locally resolved proposal, which includes Host Catalog
evidence. They are deliberately distinct because different Hosts may have
different absolute catalog defaults while resolving the same Plugin TOML into
the same semantic Plugin Root revision.

## Consequences

- Remote identity is `App / environment / Plugin / Instance`, with one stable
  authority reference visible to Console.
- Existing Console proposal, publication, history, and rollback UI works from
  authority capabilities without storage-specific branching.
- Kernel still sees only immutable resolved Plans and owns no HTTP client.
- Plugin installation, enablement, and removal remain unavailable through a
  non-local configuration authority.
- A crash after remote CAS and before local materialization is recovered from
  the ordered transition feed before the next initial Generation.
- The service must retain a continuous transition chain from a Host's visible
  semantic revision. Retention gaps fail closed and require an explicit,
  separately designed baseline recovery operation.
- Multi-Host rollout policy, durable cursor storage, and tenant/user RBAC remain
  outside this adapter slice. ADR 0065 supplies a deployable single-resource
  service with scoped read/write credentials.

## Proof

Tests must cover identity and URL validation, bearer authorization, exact
proposal comparison, remote-first CAS followed by local materialization,
history and rollback transport, stale publication rejection, ordered startup
recovery, live watch materialization through a Ready Generation switch, and
protocol/schema failure without local fallback.
