# ADR-0092: Discover selected Provider models at Generation readiness

Status: Accepted

The Generation-frozen refresh behavior in this ADR was superseded by the
[Model Catalog lifecycle](../architecture/model-catalog-lifecycle.md).

## Context

The Host Catalog can identify configured Model Provider Instances, but model
IDs, reasoning choices, service tiers, and token limits are Provider facts that
change independently of a Host release. Encoding those facts in Host model-name
branches makes a Provider update require a new Host build and can advertise
controls that the selected account no longer admits.

Discovery is not a reason to activate every installed Provider. Doing so would
request credentials and network authority for Plugins that the active Profile
did not select. A mutable catalog also cannot silently change the semantics of
an already-routable immutable Generation.

## Decision

`lenso.agent.model@2.2` adds a portable `catalog` request beside `complete`.
Each Model Provider owns acquisition and normalization of its model metadata.
During lifecycle activation, a selected remote Provider fetches its catalog,
validates configured selections and controls, and freezes the normalized result
for that candidate Generation. The Ready Gate rejects the candidate when this
required snapshot cannot be acquired or validated; the previous active
Generation remains routable.

The Host invokes only the selected Model Provider's frozen `catalog` operation
and projects that result into its Provider/Model Catalog and per-Turn resolved
profile. Unselected Providers remain unactivated. They may expose configured
model IDs for inspection, but their remote controls and limits stay `unknown`.

The direct Codex Provider fetches the authenticated
`/codex/models?client_version=...` resource during activation. An absent
`allowed_models` value admits all discovered models; when present, it restricts
the snapshot to the configured primary model plus that allowlist. Provider
metadata does not add Tool, filesystem, process, or network authority.

Catalog refresh is Generation reconciliation, not mutable per-Turn state. This
slice refreshes when a candidate Generation is built; periodic or ETag-driven
reconciliation may be added separately without changing Model Capability
ownership.

## Consequences

- adding or retiring a Codex model or reasoning level no longer requires a Host
  model-name branch;
- every Turn is validated against one catalog snapshot from its leased
  Generation;
- catalog acquisition failure is visible before the candidate becomes active;
- inspecting unselected Providers does not spend their credentials or network
  authority; and
- local and OpenAI-compatible Providers implement the same operation from their
  configured facts when no richer discovery endpoint is available.

## Proof

Capability generation and compatibility lint cover the additive request.
Provider tests cover discovery projection, filtering, controls, limits, and
duplicate rejection. The Codex integration test requires the authenticated
model request before Responses traffic. Host and web tests cover Generation-
bound projection and per-Turn validation without exposing credentials.
