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

Model Descriptor `1.1.0` adds optional `tool_name` and `arguments_json` fields
to input messages. This preserves the complete assistant Tool call when an
Agent sends a Tool result in a later completion request. The change is
additive within the existing Capability major.

## Tool boundary

`lenso.agent.tools@1` is the application-facing aggregate catalog. It fans out
to one or more `lenso.agent.tool-provider@1` providers selected by composition.
Tool names must be unique in the aggregate catalog. Duplicate names fail
composition or catalog construction; runtime order never decides a winner.

All JSON-bearing string fields contain one complete JSON value. Providers must
reject malformed or schema-invalid `arguments_json` as `invalid_arguments`.
The V1 workspace provider is read-only and must reject paths that resolve
outside its configured workspace root.

## Process boundary

`lenso.agent.process@1` is a private native request Capability between the
Agent-facing Process Tool projection and one explicitly bound process Provider.
`catalog` returns the provider-authorized program names. `run` accepts one
program name, an argument array, workspace-relative cwd, and timeout, then
returns exit code plus bounded stdout/stderr. Nonzero exit is a successful
process observation; policy rejection, timeout, output overflow, and signal
termination are Domain Errors. Caller cancellation remains Kernel
`RuntimeFailure::Cancelled` and triggers process-group cleanup.

## Prompt boundary

`lenso.agent.prompt@1` is the Agent-facing aggregate. It fans out to zero or
more `lenso.agent.prompt-provider@1` providers in the exact order selected by
App Composition. Provider-local order is also preserved. Duplicate contribution
IDs or configured size-limit violations prevent the aggregate from activating;
runtime order never chooses a winner.

Every contribution declares a stable ID, provider-owned version, `instruction`
or `skill` kind, and bounded content. The aggregate returns one joined system
prompt plus an ordered manifest whose SHA-256 digests identify the exact input
without copying Prompt content into the Session log.

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
