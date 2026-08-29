# ADR-0075: Export Session-grounded OTLP and evaluate replays

Status: Accepted

## Context

Lifecycle observers provide useful live delivery but are bounded and may fail.
The durable Session event log and its stable Trajectory projection already own
the complete model, Tool, timing, token, status, and Generation evidence needed
for post-run diagnostics. Re-executing a prior model or Tool call and calling it
replay would introduce new side effects and nondeterministic results.

OTLP export and evaluation must therefore share the durable source of truth,
remain backend-neutral across SQLite and file Session Plugins, and fail closed
on corrupt facts or unmet criteria.

## Decision

- `sessions replay --session <id>` reprojects and prints the stable
  `lenso.agent.trajectory@1` document. Replay means deterministic presentation
  replay; it never invokes a Model, Tool, Hook, or Plugin.
- `sessions evaluate --session <id> [--criteria <json>]` evaluates that same
  Trajectory. The default requires a completed Session with zero failed
  operations. Optional criteria bound duration and Tool-call count and require
  explicit Tool names. The command emits `lenso.agent.evaluation@1` and exits
  unsuccessfully when any check fails.
- `sessions otlp --session <id>` projects a deterministic OTLP/HTTP JSON trace.
  It creates one Session root span plus semantic record spans, deterministic
  trace/span IDs, stable Lenso attributes, model and Tool attributes, and exact
  nanosecond timestamps from Session facts.
- OTLP can be written with create-only file semantics, posted to an explicit
  HTTPS or loopback HTTP collector, or both. Redirects, URL credentials, query,
  and fragment are rejected; a non-success collector response fails export.
- OTLP transport credentials remain outside the command in Host-owned network
  configuration. The exporter does not read Agent Model or Plugin Secrets.

## Consequences

- CI can gate durable Agent outcomes without rerunning side effects.
- The exported trace remains reproducible from archived Sessions and comparable
  across Session backends and entrypoints.
- Live streaming telemetry, sampling, collector authentication, metrics, and
  logs can be added as separate adapters without changing Session ownership.

## Proof

Projection tests verify deterministic valid OTLP identifiers and fail-closed
evaluation. A real headless fixture creates one durable Session, replays its
Trajectory, passes the default evaluation, writes an OTLP document, and posts
the same envelope to a loopback collector.
