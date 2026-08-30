# ADR-0083: Separate Turn selection from Generation authority

Status: Accepted

## Context

The TUI previously received one fixed model string and one startup Tool scope.
It had no safe control path for changing either. Treating model, reasoning,
service tier, Profile mode, and permissions as interchangeable flags would
hide materially different authority transitions.

## Decision

A model may be selected per Turn only from the model identities explicitly
admitted by the Model Provider Instance already bound in the immutable
Generation. `TurnGeneration` resolves the selected identity back through the
Generation-revisioned Provider/Model Catalog and attaches the resulting
`ResolvedTurnProfile`. The Agent Loop uses that resolved model for requests and
durable provenance. The TUI exposes this operation as `/model`.

Tool permissions are a Turn-scoped narrowing overlay. `/permissions composed`
uses the Generation's Tool authority, `/permissions none` denies every Tool,
and `/permissions allow ...` supplies an exact allowlist. This control can
never add a Tool absent from the composed catalog.

Manual context compaction is a Session mutation, not a model prompt. The new
portable `lenso.agent.session-control@1` Capability owns `compact_session`.
The Agent Loop provides it, the TUI consumes it, and `/compact` invokes the same
durable compaction transaction as automatic policy. Active Turns and concurrent
Session mutations fail closed.

Profile modes such as `plan` and `code` change Plugin selection and Tool
authority. They therefore use Ready-Gated Generation transitions between Turns,
as implemented by ADR-0084. They are not a prompt label or mutable Agent Loop
boolean. Reasoning effort and fast/service tier remain model capability axes
and are added only when the selected model and Adapter both advertise and carry
them.

## Consequences

- the TUI can switch admitted models without rebuilding an otherwise identical
  Generation;
- permission controls only narrow existing authority;
- manual compaction is portable across surfaces and Session implementations;
- unsupported model controls stay hidden or rejected rather than being sent
  optimistically; and
- in-TUI Profile mode switching reuses the online Generation selection control
  and preserves existing Turn leases.

## Proof

The generated Session Control contract has a freshness-gated Descriptor,
Schemas, Rust client, and provider. Integration tests prove manual compaction,
model selection through an auxiliary admitted identity, durable selected-model
provenance, and preservation of all existing Host and TUI behavior. Parser
tests prove exact model and permission command forms.
