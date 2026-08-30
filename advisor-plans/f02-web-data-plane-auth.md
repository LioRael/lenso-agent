# F02 — Agent Web data-plane authorization

Status: implemented and validated

## Outcome

Keep loopback startup frictionless, but fail closed when the standalone Web
surface binds a non-loopback address without an explicit bearer secret. Apply
the same Host-owned authorization seam to every Agent data-plane route while
retaining the existing separately gated Plugin-control behavior.

## Work

- Distinguish data-plane authorization from mutation/control authorization.
- Reject non-loopback startup without a non-empty configured bearer token.
- Pre-hash the configured bearer once when the Web runtime starts, hash only the
  supplied value per request, and compare fixed-size digests in constant time.
- Redact bearer secrets and their digests from public config, runtime, and
  surface `Debug` output.
- Cover loopback compatibility, remote fail-closed startup, and authorized / unauthorized requests.

## Validation

- Web tests passed for missing, wrong, and correct bearer tokens plus Disabled,
  Local, and HostAuthorized access; fixed-size comparisons cover equal,
  same-length unequal, and unequal-length inputs.
- A route-level regression proves data-plane access cannot accidentally grant or
  gate the independently configured control route; three listener-policy tests passed.
- Direct enum, config, runtime, and surface formatting regressions contain no
  plaintext bearer secret.
- The five affected-package test command and all-target Clippy with warnings
  denied passed.
