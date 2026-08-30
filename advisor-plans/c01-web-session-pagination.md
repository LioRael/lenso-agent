# C01 — Complete Web Session reads

Status: implemented and validated

## Outcome

Return complete Session and trajectory histories instead of silently truncating
them at the first 1,000 events.

## Work

- Page through the Session Capability using revision cursors.
- Validate stable Session metadata and contiguous progress across pages.
- Bound each page while allowing the durable revision to define completion.
- Add a history larger than one page and verify both Session and trajectory reads.

## Validation

- The 1,001-event regression read cursors 0 and 1,000 and returned every event;
  both Session and trajectory handlers share this collector.
- Negative regressions reject metadata changes between pages and an event-
  revision gap spanning two pages.
- All Web tests, the five affected-package test command, and all-target Clippy
  with warnings denied passed.
