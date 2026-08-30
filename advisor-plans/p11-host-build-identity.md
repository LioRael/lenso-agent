# P11 — Host build identity startup cost

Status: implemented and validated

## Outcome

Make executable identity cost observable and avoid whole-file allocation while
preserving once-per-startup exact Host-build authority.

## Work

- Instrument executable locate/open/read-hash phases with cumulative timings.
- Hash through bounded streaming I/O while preserving the existing one identity
  per Host-startup operation.
- Test digest equivalence, executable-open failure, and telemetry accounting;
  no synthetic mid-read fault seam is claimed or required.
- Record streaming time and the peak-allocation proxy for a real test executable.

## Validation

- Digest-equivalence and fail-closed executable-open tests passed; 91 normal Host tests
  passed and one manual measurement test remains ignored by default.
- The measurement streamed 93,055,312 bytes in 70,366 microseconds through a
  262,144-byte buffer, replacing a whole-file allocation of the same size.
- The five affected-package test command and all-target Clippy with warnings
  denied passed.
