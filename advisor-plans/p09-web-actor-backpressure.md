# P09 — Bounded Web actor backlog

Status: implemented and validated

## Outcome

Keep the Web runtime actor responsive to cancellation and shutdown during an
active Turn without draining its bounded ingress channel into an unbounded queue.

## Work

- Give deferred commands an explicit capacity.
- Reject overflow deterministically while continuing to service active-Turn control.
- Preserve request ordering for admitted deferred commands.
- Test overflow, cancellation responsiveness, and normal drain after Turn completion.

## Validation

- Web actor tests passed with 16 admitted deferred commands, deterministic
  overflow, and a pre-filled slow SSE consumer without actor blocking.
- Focused seams prove admitted turns drain in FIFO order, active cancellation
  cancels immediately, and queued cancellation is recorded before its Turn starts.
- All 74 Web library tests, three standalone tests, and existing Web integration
  tests passed; all-target Clippy with warnings denied passed.
