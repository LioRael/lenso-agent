# ADR 0016: Derive local Plugin governance from risk

## Status

Accepted.

## Context

The first executable Plugin slices required every local install and upgrade to
carry manually supplied review evidence. Upgrade also required operators to
copy the currently active Manifest digest even though the Host already held an
exclusive authority fence and validated that exact Active Set. This exposed
control-plane implementation details in the ordinary author workflow without
adding authority for the lowest-risk contributions.

## Decision

The Harness derives one bounded automatic local-admission path. It applies only
when every selected executable contribution matches an existing product-owned
Profile and is all of the following:

- stable and trusted;
- stateless and permission-free;
- free of Capability dependencies and Artifact-backed implementations; and
- attached only by appending a provider to an existing `many` requirement.

Passive Releases with no selected executable contribution use the same local
fast path. The Admission Receipt records the derived decision evidence rather
than omitting evidence.

Provider replacement, intra-Plugin dependencies, state, permissions,
Artifact-backed execution, preview or experimental support, and any other
Profile topology continue to require explicit `--evidence`.

During upgrade, `--expected-manifest` remains an optional explicit CAS guard.
When it is omitted, the Host uses the active Manifest already validated while
holding the exclusive authority fence. `--plan` likewise defaults through
`LENSO_RESOLVED_PLAN` and then the product's ordinary default Plan path.

All paths still validate the complete Bundle and Profile topology, write an
Admission Receipt, stage the candidate Generation behind the Ready Gate, and
atomically commit only after readiness. Kernel receives only the resulting
immutable Plan.

## Consequences

- A local stateless Tool Provider installs and upgrades without copying review
  tickets or content digests.
- The CLI reports whether admission was automatic or explicitly reviewed.
- Automatic admission cannot replace a `one` provider or introduce new
  authority.
- CAS, provenance, rollback, and immutable Store records remain control-plane
  guarantees rather than author-supplied bookkeeping.
