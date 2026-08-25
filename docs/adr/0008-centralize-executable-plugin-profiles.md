# ADR 0008: Centralize executable Plugin profiles

- Status: accepted
- Date: 2026-08-24
- Refines: ADR 0007
- Refined by: ADR 0009
- Refined by: ADR 0018

## Context

ADR 0007 admitted one exact native Tool Provider, but its package, factory,
Capability, Host policy profile, and `tools` attachment checks were split
between Plugin admission and Generation bootstrap. Adding another reviewed
profile by copying those branches would create several product authorities and
make drift possible between admission, revalidation, Host policy, and binding
derivation.

A generic Plugin still must not select arbitrary consumers or replace existing
`one` bindings. Catalog extensibility is Host product policy, not runtime Plugin
discovery.

## Decision

The Harness owns one code-level Plugin Profile Catalog. Each executable profile
registration closes:

- one Catalog registration ID, adapter profile ID, package ID, built-in factory
  identity, entrypoint, and configuration Schema digest;
- an exact ordered set of provided Capability identities, Descriptor versions
  and digests, request Operations, and operation interaction kinds;
- execution class, target, support channel, and trust level; and
- one product-owned attachment consumer and Capability.

Admission and every startup revalidation require each executable Module
contribution to match exactly one Catalog entry. The Host Build and Execution
Policy profiles are projected from the same Catalog. Generation binding
derivation resolves the active Manifest contribution back to that entry and
attaches it only when the base Plan consumer declares the exact Capability with
`many` cardinality.

The first production Catalog contains only ADR 0007's
`agent-tool-provider-v1` registration for `lenso.agent.text-tools@0.1.0`.
Registration is deterministic and rejects duplicate registration IDs or
multiple attachments for the same factory. Multiple exact factories may share
one adapter profile and attachment policy. A Plugin Bundle cannot register a
profile, provide a binding template, or mutate the Catalog.

## Consequences

- Adding an executable profile becomes one explicit Host code and review
  change instead of parallel conditionals across admission and Generation.
- Passive releases remain valid without a Catalog entry because they contain
  no executable Module contribution.
- Unknown, partial, duplicate, or ambiguous executable profiles fail before
  active authority is written and fail again if persisted authority is
  tampered with.
- ADR 0008's initial attachment mode is append-to-`many`. ADR 0009 adds one
  restricted replace-`one` operation for the fixture Model; other `one` or
  `optional` transitions still need separate reviewed profiles rather than
  hidden rebinding.
- The Catalog is above Kernel and does not introduce runtime discovery, hot
  loading, or a global Capability registry.
