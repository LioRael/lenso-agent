# ADR-0088: Compose dynamic model selection per Turn

Status: Accepted

## Context

ADR-0083 admits a concrete model selection for one Turn without changing its
immutable Generation. An App may also want a model name such as `auto` or
`mixed` to mean a policy: inspect the current input, call a classifier, or draw
from a configured model pool. Putting these policies in the Host or Agent Loop
would make removable product behavior part of runtime infrastructure.

Dynamic selection must not expand authority. A policy cannot select another
Provider Instance, an unadmitted model, or inference controls that the active
Provider/Model Catalog rejected. The selected model must also remain stable for
the complete Turn, including compaction and retry behavior.

## Decision

Add the portable request Capability `lenso.agent.model-selection@1`. The Host
recognizes a non-concrete model name as a policy only when a Model Selection
Plugin is bound to the Agent. It resolves every eligible candidate through the
active Generation's Provider/Model Catalog and attaches those exact
`ResolvedTurnProfile` values as Turn-scoped selection authority.

Before `turn_started`, the Agent Loop invokes exactly one selected Model
Selection provider with the policy name, Turn identity, current user input,
and admitted model identities. The provider returns one model plus a strategy
and reason code. The Agent Loop maps the result back to the Host-issued
candidate profile, persists the decision with the resolved profile, and uses
that profile for the complete Turn.

The first native provider, `lenso.agent.model-selection.dynamic`, supports:

- deterministic rules based on input length and keywords;
- a stable weighted draw from a configured model pool; and
- an LLM classifier that returns `default` or `strong`, with an explicit
  fallback model when classification fails.

The weighted draw is derived from the Turn selection identity. Retries within
one Turn therefore cannot silently switch models. Each new Turn receives a new
draw.

## Consequences

- Apps define aliases such as `auto`, `review`, or `mixed` under the Plugin
  Root without changing Host code;
- removing the Plugin removes policy aliases while concrete per-Turn model
  selection continues to work;
- dynamic selection remains inside one already selected Provider Instance;
- Session provenance records the policy, strategy, reason code, and exact
  selected Provider/model profile; and
- cross-provider routing remains a future Generation-selection feature rather
  than an implicit expansion of Turn authority.

## Proof

Capability snapshot checks lock the Descriptor, Schemas, and generated Rust
bindings. Plugin tests cover configuration rejection and stable weighted
selection. Agent Loop tests cover the derived requirement and strict durable
provenance. A TUI integration test resolves `auto` to an auxiliary admitted
fixture model, completes the Turn, and verifies the persisted selection
evidence. The existing concrete-selection fixture proves that `auto` is
rejected when the Plugin is absent.
