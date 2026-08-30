# ADR-0081: Resolve model metadata into each Turn

Status: Accepted

## Context

The v1 Provider/Model Catalog projected authentication and Provider-wide
feature booleans, but each model contained only `id` and `selected`. A surface
could display a model name but could not determine its context/output limits,
reasoning control, service tiers, modalities, Tool behavior, or wire protocol.

Changing only the model string is unsafe. Context policy, inference options,
and request adaptation must agree on one profile selected from the same
immutable Generation. Provider readiness also cannot be inferred merely
because a Plugin is linked; inspecting authentication may require activating
or calling that Provider.

## Decision

The Host publishes `lenso.agent.provider-model-catalog.v2`. Each model entry
owns:

- known context, input, and output token limits;
- input modalities and text-output support;
- Tool-call and parallel-Tool support;
- reasoning control as `unknown`, `unsupported`, or a selectable value set;
- service-tier control with the same explicit state distinction;
- the Provider wire protocol; and
- a compaction-compatibility identity.

An absent numeric limit means unknown, not unlimited. `unsupported` means the
current Provider/model path cannot carry that option. `unknown` means the Host
does not have enough authoritative metadata to expose the control. Surfaces
must not manufacture controls from either state.

Provider entries expose readiness separately from authentication method. The
read-only Host projection initially reports `unchecked`; it does not activate
an unselected Provider or claim that authentication succeeds.

The catalog revision is the exact active Generation Spec digest. The catalog
also returns one `ResolvedTurnProfile` for its selected Provider Instance and
model. On each Generation lease, the Host attaches that profile to the root
Invocation Context. The Agent Loop fails closed unless the profile revision
matches Generation provenance and its model matches the immutable Agent
configuration.

`turn_started` persists the complete resolved profile. Adapters serialize an
output bound only when their wire contract supports one, narrowed by any known
model maximum. The direct ChatGPT Codex endpoint rejects the public Responses
API `max_output_tokens` field, so that service retains its wire-level output
limit. This establishes the runtime seam for later token-aware compaction and
turn-scoped model/variant selection without making the catalog a mutable
registry.

The fixture model has deterministic limits for tests. Other limits remain
unknown until authoritative Provider/model metadata is configured or fetched.
The direct ChatGPT path advertises only inference controls implemented by its
current Adapter; service tier remains unsupported until its request contract is
implemented. No Grok subscription behavior is introduced.

## Consequences

- TUI and Web surfaces can distinguish selectable, unsupported, and unknown
  model controls;
- every Turn durably identifies the exact model profile it used;
- model limits can safely narrow local output and future context policy;
- catalog inspection remains read-only and cannot promise authentication
  readiness; and
- switching Provider, authority, Profile, or permission policy still requires a
  Ready-Gated Generation change rather than mutating an active Turn.

## Proof

Web integration proves the v2 schema, Generation-bound revision, per-model
fixture limits and capabilities, explicit readiness, and selected resolved
profile. Headless execution proves that the same profile is persisted in
`turn_started` while the default-unlimited Agent Loop continues through more
than sixteen Tool calls.
