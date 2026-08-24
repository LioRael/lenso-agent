# ADR 0007: Admit one native Tool Plugin profile

- Status: accepted
- Date: 2026-08-24
- Supersedes: ADR 0006 executable-Plugin deferral

## Context

Runtime revision `25812bcbaf3b488d1a03f1864eb0130b53cadd93` adds an exact
`NativeModuleFactory::factory_identity` contract. A control-plane
`built_in_factory` selection can therefore close through the generated Plan
into Native Adapter preparation without confusing factory identity with Cargo
package version.

The Agent Harness still needs a product-owned attachment policy. A generic
Plugin must not invent bindings to arbitrary existing Instances.

## Decision

The first executable profile is deliberately narrow:

- the linked factory is exactly `lenso.agent.text-tools@0.1.0`;
- the contribution is stateless, permission-free, zero-dependency, and uses
  empty canonical configuration;
- it provides exactly `lenso.agent.tool-provider@1` operations `catalog` and
  `execute` at Descriptor version `1.0.0`;
- it selects stable/trusted `lenso.native-rust@1` with profile
  `agent-tool-provider-v1`; and
- the Harness deterministically binds each active Plugin Tool Provider Instance
  to the existing `tools` aggregator.

`plugins install` creates exact locked Instances from selected contributions.
`plugins remove --plugin <id>` atomically removes the Release and all of its
Instances. Bindings are derived only from the validated active lock at startup,
so removal deletes the Tool from the next Generation without changing the base
Resolved App Plan.

## Consequences

- Merely linking the factory does not activate the Module.
- An unregistered factory, altered Descriptor digest, permission request,
  state declaration, dependency, Artifact-backed implementation, or other
  Capability fails admission.
- Removal leaves immutable Store objects unreferenced but removes all active
  authority.
- Additional Plugin types require new explicit product attachment profiles;
  this decision does not authorize arbitrary cross-Plugin or base-App bindings.
- Runtime hot replacement, upgrade, permission grants, and preview execution
  classes remain deferred.
