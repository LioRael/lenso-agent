# ADR 0018: Admit isolated third-party Wasm Tool Providers

## Status

Accepted.

## Context

The Harness can build and execute reviewed Wasm Component Agent replacements, but every executable
Plugin profile still fixes one package identity in Host code. A third party therefore cannot add a
Tool without first changing and rebuilding the Harness, even when the contribution has no Host
imports, permissions, state, configuration, or replacement authority.

The product needs one honest third-party extension path without turning a publisher-controlled
Manifest into binding policy or claiming a marketplace trust system.

## Decision

The Plugin Profile Catalog admits one package-independent executable shape. A contribution may use
any non-empty package identity only when it:

- provides exactly `lenso.agent.tool-provider@1` Descriptor `1.0.0`, operations `catalog` and
  `execute`, and the exact reviewed Descriptor digest;
- uses one Artifact-backed `lenso.wasm-component@1` implementation with entrypoint `plugin`,
  profile `agent-tool-provider-v1`, experimental support, and isolated trust;
- uses the empty configuration Schema and Host-owned canonical `{}` configuration;
- declares no Capability requirement, permission request, state, Data mount, or binding template;
  and
- attaches only by appending to the existing `tools` Instance's exact `many` Tool Provider
  requirement.

Artifact-backed and experimental execution continues to require explicit bounded review evidence.
The Host registers the generated Tool Provider JSON codec and validates the guest Descriptor before
readiness. The Bundle cannot select its consumer, configuration, permissions, or execution policy.

The checked standalone example has no Agent Harness path dependency. Product tests copy it outside
the repository workspace, build its Wasm core, use the production Bundle builder, and prove install,
Agent invocation, upgrade, rollback, invocation after rollback, removal, and loss of Tool authority.

## Consequences

- A third party can independently build and install a pure Wasm Tool without registering its package
  identity or recompiling the Host.
- This is a local reviewed extension path, not automatic trust, publisher identity verification,
  marketplace distribution, permission grants, or arbitrary Plugin topology.
- Wasm resource limits and lack of Host imports contain the accepted shape. Filesystem, process,
  network, secret, or other Host access requires a later permission and Capability-import slice.
- Native packages, Model replacement, Agent replacement, configuration, and every other attachment
  shape remain exact product-owned Catalog entries.
- Kernel still receives one immutable Resolved App Plan and owns no Plugin discovery or policy.

## Rejected alternatives

Allowing publishers to select arbitrary bindings or required Host Capabilities would delegate
product authority to the Bundle. Registering every third-party package in Host source would preserve
the existing technical mechanism but would not provide independent extension.
