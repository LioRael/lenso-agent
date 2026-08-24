# ADR 0009: Admit one fixture Model replacement profile

- Status: accepted
- Date: 2026-08-24
- Refines: ADR 0008

## Context

The centralized Catalog can safely append Tool Providers to a `many`
requirement, but the Agent's Model requirement has `one` cardinality. Adding a
second binding would make the Plan invalid, while changing a live Kernel graph
would violate the immutable App Generation boundary.

Model completion is a stream operation. Runtime revision
`a42c7f7e160513968aaef33af087d76cff8adc99` preserves Manifest operation kinds
when resolving Plugin Module Instances, so a reviewed Model contribution can
now project the same stream endpoint contract as a base Plan Module.

A general Model switch also needs coupled Agent configuration, credentials,
provider policy, and migration rules. This slice needs a smaller deletion proof
before authorizing those choices.

## Decision

The Harness Catalog registers `agent-model-provider-v1` for exactly the linked
`lenso.agent.model.fixture@0.1.0` built-in factory. Its Manifest must close the
exact Model Descriptor, the `complete` stream operation, the fixture
configuration Schema, canonical configuration
`{"model":"fixture/readme-summary-v1"}`, native execution policy, supported
target, and trusted stable channel.

The profile attaches only to the base `agent` Instance's
`lenso.agent.model@1` requirement when all of these conditions hold:

- the requirement has `one` cardinality and Descriptor version `1.1.0`;
- the base Plan has exactly one matching binding to Instance `model`;
- that displaced Instance is package `lenso.agent.model.fixture`; and
- no other base binding consumes a Capability from the displaced Instance.

Before Generation resolution, Composition removes the displaced Module
Instance and every binding where it is consumer or provider, then adds the
Plugin Instance and replacement binding. Two active replacements for the same
base Instance fail closed. The next startup resolves one immutable Plan; no
running Kernel graph is changed.

Removing the Plugin removes its active Release and Instance authority, so the
next Generation is the exact approved base Plan again.

## Consequences

- Install, startup revalidation, replacement, execution, and removal have one
  real Bundle and CLI test path.
- Stream interaction kind is part of the executable profile match rather than
  inferred by the Harness.
- The profile deliberately rejects OpenAI-compatible and direct Codex base
  Plans. Those providers need separate profiles that close compatible Agent
  model configuration and credential policy.
- This is neither runtime hot replacement nor general Plugin-authored binding
  selection. `optional`, arbitrary `one`, upgrades, overlap, and rollback remain
  deferred product slices.
