# ADR-0082: Trigger Context Compaction from model input capacity

Status: Accepted

## Context

ADR-0047 triggered compaction after a fixed number of Session events. Event
count is deterministic, but it is not a useful approximation of model context:
one event may contain a few characters or a large Tool result. Models also have
different input windows, and some Provider catalogs cannot yet supply a trusted
limit.

The Harness must distinguish an unknown limit from an unlimited one. It must
also retain a strict fixed threshold for installations that need predictable
policy independent of Provider metadata.

## Decision

The Agent Loop supports three validated compaction policies:

- `model_default` uses 85 percent of the resolved safe model input window;
- `percent` uses an explicitly configured percentage from 1 through 99; and
- `tokens` uses an explicitly configured positive token threshold.

Configuration represents the tagged choice as
`compaction_trigger_mode` plus `compaction_trigger_value`, because the current
generated Plugin configuration projection supports scalar optional fields.
`compaction_fallback_percent` changes the `model_default` percentage without
changing its model-relative meaning.

The safe input window is the resolved `max_input_tokens`, or a known context
window minus a known maximum output allowance. A fixed token threshold is
capped to that safe window when it is known. An explicit percentage fails
closed when no safe input window is known. The default policy falls back to the
existing event-count threshold when model capacity is unknown, preserving old
configurations without pretending that unknown means unlimited.

Before each model request, the Loop estimates the complete projected request:
System Instruction, prior compacted summary, reconstructed Session messages,
and pending user input. The estimator is deliberately conservative and local;
Provider tokenizers may replace it later without changing policy semantics.

When the threshold is reached, the Loop uses the existing ADR-0047 transaction.
The started event records the trigger kind, estimate, and resolved threshold.
Committed checkpoints remain derived projections; canonical Session events are
never rewritten.

Manual compaction invokes the same transaction through
`lenso.agent.session-control@1`. It is not encoded as a user prompt and does
not bypass Generation or Session authority.

## Consequences

- larger-context models compact later by default and smaller-context models
  compact earlier;
- operators may choose percentage or fixed-token policy explicitly;
- unknown model limits remain observable and safe;
- existing event-count configurations continue to operate as the compatibility
  fallback; and
- automatic and manual compaction share one durable checkpoint protocol.

## Proof

Unit tests cover model-default percentage calculation, rejection of percentage
policy with unknown capacity, fixed-token capping, and monotonic context
estimation. A headless integration test forces the token threshold and proves a
durable checkpoint is committed without rewriting prior Session history.
TUI integration proves `/compact` reaches the generated Session Control
provider and commits the same started/committed event pair.
