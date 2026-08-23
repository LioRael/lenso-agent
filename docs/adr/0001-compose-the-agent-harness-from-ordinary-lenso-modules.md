# ADR 0001: Compose the Agent Harness from ordinary Lenso Modules

- Status: accepted
- Date: 2026-08-24
- Relates to: Lenso ADRs 0030, 0031, 0033, 0034, 0041, 0045, 0046,
  0047, 0051, 0052, 0055, 0056, 0057, and 0064

## Context

DeepSeek Harness demonstrates a useful product experience in which model,
Tool, Session, loop, storage, UI, and other behavior can be selected from
packages and configuration. Its Cordis implementation uses a mutable in-process
service graph, typed events, Fibers, and reversible effects.

Lenso already provides a different runtime contract: explicitly bound
Capabilities, immutable Resolved App Plans, staged activation, stable handles,
bounded invocation, cancellation, supervision, and Execution Adapters. Adding
a second mutable plugin graph inside the Agent product would weaken those
guarantees and create two dependency authorities.

## Decision

The Agent Harness is an ordinary Lenso App Composition. A user-facing Agent
plugin is an ecosystem package containing one or more ordinary Modules that
provide declared Agent Capabilities. Package acquisition, trust decisions,
configuration, and bindings happen before boot. The Kernel executes the exact
Resolved App Plan and never discovers Agent plugins.

V1 standardizes five product roles in this repository:

- `lenso.agent@1`
- `lenso.agent.model@1`
- `lenso.agent.tools@1`
- `lenso.agent.tool-provider@1`
- `lenso.agent.session@1`

The Agent Loop binds one Model, one Tool Runtime, and one Session. The Tool
Runtime binds many Tool Providers and exposes one validated catalog and
execution Interface to the Agent Loop. Model and Agent results stream through
bounded Lenso Stream Operations. Session and Tool interactions use Requests.

The V1 profile treats selected Rust and Bun packages as trusted code. The
read-only Workspace Tool owns final filesystem authorization under one
configured root. The remote Model Module owns provider egress and resolves its
credential through the existing `lenso.secrets@1` Capability.

## Consequences

- Replacing a Model or Tool provider changes package inputs and App Composition,
  not the Agent Loop or Kernel.
- The Tool Runtime earns its seam by hiding catalog aggregation, collision
  validation, argument validation, deterministic routing, and provider-error
  translation.
- Session durability, revision conflicts, recovery, and retention belong to
  the Session Module and its private persistence Adapter.
- Live streams are bounded delivery paths, not durable trajectory evidence.
- Lenso Events are not used to imitate middleware waterfall semantics. Ordered
  Hook interception requires a later explicit Request Capability with real
  independent implementations.
- V1 configuration changes produce a new Plan and restart the App. Seamless
  code replacement requires a future App Generation design above the Kernel.
- Untrusted or model-generated code remains unsupported until a reviewed Wasm
  or isolated-process Adapter supplies genuine resource confinement.

## Rejected alternatives

### Add a global Agent plugin registry to Kernel

This would make Kernel own product discovery, package identity, and mutable
Agent policy. It would also bypass explicit Capability bindings.

### Let every Tool plugin register directly into Agent Loop memory

This creates an undeclared second graph and leaves collision, cleanup, schema,
and routing policy distributed across providers. The Tool Runtime instead
consumes explicit `many` bindings.

### Treat Bun or `node:vm` as a hostile-code sandbox

Process and JavaScript realm separation can contain some failures but does not
provide a security boundary. V1 packages are trusted.
