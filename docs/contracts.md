# Capability contract semantics

The JSON Schemas and Capability Descriptors are the portable source of truth.
Generated projections must not be edited by hand. This native App owns its Rust
projections; `@lenso/bun` owns the TypeScript projections generated from locked
snapshots of the same sources.

## Stream conventions

- `lenso.agent@3/run_turn` is a server-output stream. The caller sends exactly
  one open request and then closes its send half.
- `lenso.agent.model@2/complete` follows the same half-close convention.
- Every emitted stream message has a monotonically increasing `sequence`,
  starting at `1` for each invocation.
- A normal stream close means completion. A typed stream error means the turn
  or model request failed. Cancellation is carried by the Kernel invocation
  context rather than modeled as an ordinary message.

For `complete`, fields not selected by `kind` retain their schema defaults:

- `reasoning_summary_delta` uses `text` and contains only Provider-designated,
  display-safe summary content.
- `text_delta` uses `text`.
- `tool_call` uses `tool_call_id`, `tool_name`, and `arguments_json`.
- `usage` uses `input_tokens` and `output_tokens`.

Model Descriptor `1.1.0` added optional `tool_name` and `arguments_json` fields
to input messages. This preserves the complete assistant Tool call when an
Agent sends a Tool result in a later completion request. The change is
additive within the existing Capability major.

Model `lenso.agent.model@2`, Descriptor `2.0.0`, adds the closed
`reasoning_summary_delta` kind. Agent `lenso.agent@3`, Descriptor `3.0.0`,
projects it as ordered `reasoning_delta` and `reasoning_completed` messages
with one Turn-step `reasoning_id`. Reasoning progress is volatile terminal
presentation data, not durable Session evidence or raw private chain-of-thought.

## Tool boundary

`lenso.agent.tools@2` is the application-facing aggregate catalog. It fans out
to one or more `lenso.agent.tool-provider@2` providers selected by composition.
Tool names must be unique in the aggregate catalog. Duplicate names fail
composition or catalog construction; runtime order never decides a winner.

Each Tool definition declares `parallel_safe` or `exclusive` execution.
Consecutive safe calls may overlap within the App's explicit binding admission
and Agent Loop bound. Exclusive calls are ordering barriers. Results are
persisted and returned to the Model in request order.

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

`lenso.agent.prompt@1` is the Agent-facing aggregate. The official aggregate
starts with the required `harness.base` instruction, then fans out to zero or
more `lenso.agent.prompt-provider@1` providers in the exact order selected by
App Composition. Provider-local order is also preserved. Duplicate contribution
IDs or configured size-limit violations prevent the aggregate from activating;
runtime order never chooses a winner. A replacement aggregate must also return
non-empty content; the Agent Loop enforces this public boundary invariant.

Every contribution declares a stable ID, provider-owned version, `instruction`
or `skill` kind, and bounded content. The aggregate returns one joined system
prompt plus an ordered manifest whose SHA-256 digests identify the exact input.
The Agent Loop calls this aggregate once when it installs a Session's System
Instruction, not once per Turn.

## Session log

Session events are append-only. `expected_revision` provides optimistic
concurrency: an append either stores the entire batch exactly once or returns a
`revision_conflict` without storing any event. Reusing an `event_id` within one
session is idempotent only when the event is byte-for-byte identical; otherwise
it is `invalid_event`.

`read.after_revision` is exclusive. Returned events are ordered by revision and
the response `revision` is the latest durable revision observed by that read.
Timestamps are evidence supplied by the Agent Plugin; event order is defined by
revision, not wall-clock time.

Every new Session installs one non-empty System Instruction before its first
`turn_started` fact. `system_instruction_installed` stores the complete content,
its `sha256:` digest, the ordered Prompt manifest, and the installing App
Generation Spec digest. A resumed Session reuses that event and fails closed on
malformed or multiple installations. A legacy Session without the event is
migrated once on first resume. Descriptor `1.2.0` adds this event kind.

Descriptor `1.3.0` adds `context_compaction_started`,
`context_compaction_committed`, and `context_compaction_failed`. These are
append-only projection facts; they never replace or delete the source events.
A committed checkpoint identifies the exact source revision, bounded summary,
retained complete-turn suffix, and summary digest.

Descriptor `1.4.0` adds `memory_recalled`, `memory_recall_failed`,
`memory_committed`, and `memory_commit_failed`. These events record the
portable Memory interaction outcome and logical IDs, not recalled content or
private storage details.

Supplying `session_id` to `open` means resume-only: an absent Session returns
`not_found`. Omitting it creates a new Session. This additive Domain Error was
introduced in Descriptor `1.1.0`; `lenso.agent.session@1` consumers preserve
unknown Domain Error codes for forward compatibility.

Agent Descriptor `1.1.0` adds optional `session_id` to `run_turn` messages so a
consumer can persist the identity created by the Agent Loop. Older consumers
may ignore it and older providers remain representable.

Agent Descriptor `1.2.0` adds an optional `kind` plus Tool call, result, and
duration fields to the same bounded `run_turn` stream. A missing `kind` remains
a `text_delta`, preserving messages from older providers. `tool_started`,
`tool_completed`, and `tool_failed` expose the Agent Loop's live Step progress;
they do not replace the durable Session events that own trajectory evidence.

## Lifecycle observers

`lenso.agent.lifecycle@1` delivers typed `session_started`, `session_resumed`,
and `turn_started` transitions to zero or more observers in resolved Plan
order. Session start runs only after the required System Instruction is
durable and before the first user Turn. Delivery is at least once and carries
a stable event ID; observers must be idempotent. Observer failure rejects the
pending transition and never widens Agent authority.

## User interaction

`lenso.agent.user-interaction@2` is the portable, replaceable seam between an
Agent Tool and an interactive surface. `ask` waits for one to eight structured
question answers; questions support single-select, multi-select, an automatic
Other path, and optional previews on single-select choices.
`pending` and `answer` let the selected surface present and complete questions
without exposing its event loop or widget state. A Host-issued typed Invocation
Context marker is required for `ask`, so a non-interactive surface receives
`unavailable` before any pending state or timeout is created.

## Context Compaction

`lenso.agent.context-compaction@1` receives a bounded previous summary and
complete user/assistant message pairs. It returns one non-empty bounded summary
and zero or more retained complete pairs. The Agent Loop accepts retained
messages only when they are an exact suffix of the request, writes the Session
transaction facts, and composes the result below the installed System
Instruction. Compactors do not read or mutate Session storage directly.

## Memory

`lenso.agent.memory@1` is a portable, replaceable curated-knowledge seam.
`observe` receives one complete successful Turn with source Session/Turn
provenance. `recall` returns bounded items with stable logical IDs, content,
source provenance, and confidence in thousandths. `remember` creates explicit
knowledge and `forget` deletes selected logical IDs. Durable-storage failures,
deadlines, cancellation, and provider unavailability remain Runtime Failures.

Memory is cross-Session request context, not canonical Session history and not
a System Instruction. The Agent Loop validates all returned bounds and labels
recalled text as untrusted context. Providers keep ranking, embeddings, FTS,
database schemas, consolidation, and transport private.
