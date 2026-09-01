# ADR-0094: Model reasoning controls as typed Turn options

Status: Accepted

## Context

ADR-0092 moved model discovery to the selected Provider and froze the resulting
catalog at the Generation Ready Gate. The `lenso.agent.model@2.2` catalog can
describe reasoning only as a list of effort strings. That fits Codex and some
OpenAI-compatible Providers, but it cannot faithfully represent Providers that
offer a boolean reasoning switch or a bounded token budget.

A generic parameter map would preserve Provider wire shapes at the cost of
portable validation, stable Turn provenance, and consumer interoperability.
The Host needs a small normalized vocabulary while each Provider remains the
owner of its private protocol mapping.

## Decision

`lenso.agent.model@3` extends the existing reasoning control with an optional
`mode`:

- `effort` publishes named options with labels, descriptions, and a default;
- `toggle` publishes the stable `off` and `on` options and a default;
- `budget_tokens` publishes inclusive minimum, maximum, and default token
  counts as portable decimal strings.

A selectable control without `mode` retains the previous meaning of `effort`
inside the Host's migration boundary.
Unknown and unsupported controls carry no mode, options, default, or budget.
Budget controls use the bounded budget object and leave the option list and
legacy default empty.

The `complete` request adds `reasoning_enabled` and
`reasoning_budget_tokens`. A request may select at most one of
`reasoning_effort`, `reasoning_enabled`, and `reasoning_budget_tokens`. The Host
validates that selection against the frozen catalog, records the normalized
selection in the resolved Turn profile, and sends the same selection on retry
and compaction paths. The Provider maps it to its private wire protocol.

Service tier remains a separate control because it describes delivery policy,
not reasoning behavior.

## Compatibility

Adding fields to a strict Provider response changes the constraints observed by
existing consumers, so compatibility lint rejects this as a minor evolution.
The contract therefore advances from `lenso.agent.model@2.2` to
`lenso.agent.model@3.0`. All in-tree Providers and the Host migrate atomically.
Effort-only behavior remains semantically unchanged, but external Providers
must implement the new major before selection.

## Consequences

- Consumers can render Provider-authored reasoning controls without knowing a
  Provider protocol.
- The Host can reject unsupported or ambiguous selections before execution.
- Turn provenance identifies the effective reasoning behavior, rather than an
  opaque Provider parameter bag.
- Providers with richer, non-portable controls still need an explicit future
  contract extension instead of leaking arbitrary wire fields.

## Proof

The slice requires:

- 2.2-to-3.0 compatibility lint;
- source snapshot and generated Rust freshness checks;
- projection and validation tests for effort, toggle, and token budget modes;
- Provider-to-Host-to-Turn proof that the selected value reaches `complete`
  and durable provenance unchanged.
