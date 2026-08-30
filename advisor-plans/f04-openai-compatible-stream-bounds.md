# F04 — OpenAI-compatible stream bounds

Status: implemented and validated

## Outcome

Bound every provider-controlled accumulation in the OpenAI-compatible SSE
decoder without changing the portable Model contract.

## Work

- Add a validated per-event byte bound following the Direct Codex pattern.
- Bound cumulative stream Tool-call count and accumulated ID, name, and argument
  bytes without resetting the budget after a `tool_calls` finish.
- Reject later Tool deltas after the first Tool batch, while accepting usage and
  `[DONE]`, and bound items and payload bytes returned by each decoder push.
- Fail closed on every non-empty provider frame after the terminal marker.
- Fail the stream with a stable protocol error when any bound is exceeded.
- Add fragmented-frame and cumulative Tool-call regression tests.

## Validation

- All 20 model Plugin tests passed, including a 4 MiB single chunk, an
  oversized tail after a valid frame, 128-call pressure, per-call bounds, and
  the 4 MiB aggregate Tool-call budget.
- Regressions reject a second valid Tool batch across pushes and a single push
  containing more than 1,024 output items or 8 MiB of decoded payloads.
- Same-push and later-push regressions reject content following `[DONE]`.
- The five affected-package test command and all-target Clippy with warnings
  denied passed.
