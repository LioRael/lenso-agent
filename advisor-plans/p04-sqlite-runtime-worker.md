# P04 — Non-blocking SQLite Session runtime

Status: implemented and validated

## Outcome

Move Session SQLite work off current-thread executors through one bounded,
serialized, Generation-owned worker that preserves all existing transactional
and fail-closed semantics.

## Work

- Start one bounded worker during Plugin prepare and stop it during shutdown.
- Dispatch open/read/list/append/rename requests through bounded messages.
- Preserve offline inspector/importer synchronous paths.
- Prove backpressure, cancellation safety, shutdown, and no worker reuse across Generations.

## Validation

- All 11 SQLite Plugin tests passed. A blocked write left the current-thread
  executor responsive for the 25 ms probe; stopped workers failed closed.
- A worker held behind a test gate admitted exactly 32 queued operations,
  rejected the 33rd, kept an unadmitted operation cancellable, and made shutdown
  wait until the admitted backlog drained.
- The Plugin-state lifecycle test rejected a second prepare, removed Capability
  availability before join, and prepared/deactivated a fresh generation.
- The admission boundary documents that dropping an invocation may discard its
  reply while an already-admitted transaction completes atomically.
- The five affected-package test command and all-target Clippy with warnings
  denied passed.
