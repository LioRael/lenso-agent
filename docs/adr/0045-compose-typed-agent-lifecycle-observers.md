# ADR-0045: Compose typed Agent lifecycle observers

Status: Accepted

## Context

Plugins need to react to Session and Turn transitions for audit, telemetry, and
workflow integration. Putting callbacks directly in the TUI or Agent Loop
would make third-party integration surface-specific. Reusing Tool Hooks would
also misrepresent lifecycle facts as Tool authorization.

## Decision

The Harness defines portable `lenso.agent.lifecycle@1` with one `observe`
operation. The first version exposes only transitions with established call
sites: `session_started`, `session_resumed`, and `turn_started`.

The Agent Loop invokes every observer in resolved Plan order. A Session-start
observation happens after the Session and required System Instruction are
durable but before the first user Turn is admitted. A failed initial delivery
can be retried with the same Session meaning; observers therefore receive a
stable event ID and must treat delivery as at least once. Turn-start
observation gates the durable `turn_started` event.

Two ordinary Adapters establish the seam:

- `lenso.agent.lifecycle.audit` appends typed events to a synchronized local
  JSONL audit file and is the default Adapter.
- `lenso.agent.lifecycle.command` sends one event as JSON on stdin to an exact
  absolute executable, with no shell lookup, discarded output, cancellation,
  and a bounded timeout.

The Capability does not expose Agent Loop state or permit observers to rewrite
Session facts. New lifecycle transitions are added only with a real producer,
ordering rule, and failure policy.

## Consequences

- TUI, headless, channels, and third-party Agent surfaces share the same Hook
  behavior.
- Removing all lifecycle observer instances restores the previous Agent Loop
  behavior without a special disabled implementation.
- Command Hooks are explicit trusted local configuration, not arbitrary text
  interpreted by a shell.
- Completion and failure Hooks remain future work until their terminal
  delivery semantics can avoid contradicting durable Session outcomes.
