# P05 — Incremental TUI transcript layout

Status: implemented and validated

## Outcome

Remove full-prefix row rescans from transcript rendering so work scales with
entries plus visible output rather than the square of transcript length.

## Work

- Compute each rendered entry's row contribution once per frame.
- Reuse the measured rows for viewport selection and final rendering.
- Keep wrapping, links, Tool state, reasoning state, and scroll behavior unchanged.
- Add a scale regression test that counts layout work at large transcript sizes.

## Validation

- All 66 TUI unit tests and both integration tests passed.
- The 2,000-entry scale case produced 4,000 rows with exactly 2,000 measured
  line visits, confirming linear rather than prefix-rescan work.
- The five affected-package test command and all-target Clippy with warnings
  denied passed.
