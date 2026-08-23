# Capability contract semantics

The JSON Schemas and Capability Descriptors are the portable source of truth.
The generated Rust and TypeScript files are checked-in projections and must not
be edited by hand.

## Stream conventions

- `lenso.agent@1/run_turn` is a server-output stream. The caller sends exactly
  one open request and then closes its send half.
- `lenso.agent.model@1/complete` follows the same half-close convention.
- Every emitted stream message has a monotonically increasing `sequence`,
  starting at `1` for each invocation.
- A normal stream close means completion. A typed stream error means the turn
  or model request failed. Cancellation is carried by the Kernel invocation
  context rather than modeled as an ordinary message.

For `complete`, fields not selected by `kind` retain their schema defaults:

- `text_delta` uses `text`.
- `tool_call` uses `tool_call_id`, `tool_name`, and `arguments_json`.
- `usage` uses `input_tokens` and `output_tokens`.

## Tool boundary

`lenso.agent.tools@1` is the application-facing aggregate catalog. It fans out
to one or more `lenso.agent.tool-provider@1` providers selected by composition.
Tool names must be unique in the aggregate catalog. Duplicate names fail
composition or catalog construction; runtime order never decides a winner.

All JSON-bearing string fields contain one complete JSON value. Providers must
reject malformed or schema-invalid `arguments_json` as `invalid_arguments`.
The V1 workspace provider is read-only and must reject paths that resolve
outside its configured workspace root.

## Session log

Session events are append-only. `expected_revision` provides optimistic
concurrency: an append either stores the entire batch exactly once or returns a
`revision_conflict` without storing any event. Reusing an `event_id` within one
session is idempotent only when the event is byte-for-byte identical; otherwise
it is `invalid_event`.

`read.after_revision` is exclusive. Returned events are ordered by revision and
the response `revision` is the latest durable revision observed by that read.
Timestamps are evidence supplied by the Agent Module; event order is defined by
revision, not wall-clock time.

Supplying `session_id` to `open` means resume-only: an absent Session returns
`not_found`. Omitting it creates a new Session. This additive Domain Error was
introduced in Descriptor `1.1.0`; `lenso.agent.session@1` consumers preserve
unknown Domain Error codes for forward compatibility.

Agent Descriptor `1.1.0` adds optional `session_id` to `run_turn` messages so a
consumer can persist the identity created by the Agent Loop. Older consumers
may ignore it and older providers remain representable.
