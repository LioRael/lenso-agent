# ADR 0010: Admit an atomic Codex Direct Plugin

- Status: accepted
- Date: 2026-08-25
- Refines: ADR 0009

## Context

Codex Direct is not a standalone Model Instance. The Model requires exactly one
private `lenso.agent.auth.openai-codex@1` provider, and the Agent Loop must send
the same model identity configured for the provider. Installing only the Model,
omitting Auth through a Feature selection, or leaving the fixture Agent model
configuration unchanged would produce an incomplete or unusable Plan.

The generic Runtime already projects declared Module requirements and verifies
that an intra-Plugin binding has a matching Manifest template. Product policy
must still decide which exact contributions, configurations, topology, and base
replacement are acceptable.

## Decision

The Harness Catalog registers two experimental trusted native profiles:

- `lenso.agent.model.openai-codex-direct@0.1.0` provides the Model stream,
  requires exactly one Codex Auth Capability, and replaces the fixture base
  `model` Instance; and
- `lenso.agent.auth.openai-codex@0.1.0` provides the private Auth request
  Capability and has no external attachment.

The admitted Bundle must select both contributions and declare exactly one
`codex-model -> codex-auth` binding template. Admission matches exact package,
factory, Descriptor digest, operation table and kind, requirement cardinality,
configuration Schema, execution profile, target, support channel, and trust.
The Host derives canonical production configurations; the publisher does not
select base URLs, OAuth issuer/profile, model, or reasoning effort.

The Model replacement is valid only over the fixture base `agent -> model`
edge. Composition removes the fixture Model, binds the Plugin Model to `agent`,
binds that Model to the Plugin Auth Instance, and atomically replaces the exact
fixture Agent configuration with the same bounded configuration using
`gpt-5.6-luna`. The direct Model configuration fixes medium reasoning and the
official ChatGPT backend; Auth fixes the official issuer and `default` profile.

All changes produce one new immutable Plan before Kernel boot. OAuth credentials
remain in `~/.lenso/agent/auth.json` and never enter the Manifest, Plugin lock,
Plan, Session events, or diagnostics. Removing the Plugin restores the exact
base Plan on the next startup.

## Consequences

- One reviewed install activates the complete Model/Auth dependency closure;
  incomplete Feature selections and incompatible templates fail before active
  authority is written.
- Missing login fails the first turn before any Model HTTP request. Browser PKCE
  remains the normal login path; device auth remains explicitly opt-in.
- The profile is deliberately fixed to `gpt-5.6-luna` with medium reasoning.
  Publisher-selected models, endpoints, Auth profiles, arbitrary templates,
  cross-Plugin dependencies, upgrades, overlap, and rollback remain separate
  product decisions.
- No Kernel, Driver, or Execution Adapter change is introduced. Runtime only
  consumes the exact Plan and verifies the declared intra-Plugin closure.
