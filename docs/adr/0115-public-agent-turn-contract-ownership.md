# ADR 0115: Own public Agent turn contracts in Capability crates

Status: Accepted

## Context

The default Agent Loop previously owned public types that the Host, model
Providers, selection policies, surfaces, and diagnostics needed to exchange.
That made a default implementation the source of generic contract identity and
forced an alternate Provider to inherit its protocol vocabulary.

The blank foundation must inspect a selected composition without depending on a
default Loop. Public turn facts also need a stable home that a future alternate
Loop can consume without importing the legacy implementation.

## Decision

Public turn contracts move to the smallest existing Capability owners:

- model profile, limits, capabilities, and wire identity belong to
  `lenso-capability-agent-model`;
- dynamic model selection belongs to
  `lenso-capability-agent-model-selection`;
- narrowed Tool authority belongs to `lenso-capability-agent-tools`;
- presentation-only input belongs to `lenso-capability-agent-turn-input`; and
- session profile plus durable Agent provenance belong to
  `lenso-capability-agent`.

The default Loop consumes these contracts and retains compatibility re-exports
for existing callers. It does not own their canonical definitions. Existing
built-in wire values retain their serialized representation. An external
Provider may declare a bounded versioned `ModelWireProtocol::Other` identity;
this adds no Host branch and does not turn protocol identity into unbounded
metadata.

The Foundation catalog remains neutral. It does not choose a Loop, Provider,
or protocol merely because a public type exists.

## Consequences

Host-facing consumers can depend on public Capability crates instead of the
default Loop. The migration preserves source compatibility through re-exports
while making dependency direction visible for future removal.

This decision does not itself prove an alternate Loop, a third-party Provider,
or released-package compatibility. Those require separate external fixtures
and V10 package-boundary evidence.
