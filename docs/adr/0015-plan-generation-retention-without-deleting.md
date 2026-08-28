# ADR 0015: Plan Generation retention without deleting

## Status

Accepted.

## Context

Generation Specs are immutable provenance. A Spec can remain reachable through
the current or retained Plugin Sets, or through any durable Session Turn. An
operator needs to see which Specs have no such reference before any deletion
mechanism is considered.

## Decision

The Harness Host provides a read-only command:

```text
generations gc-plan [--root <plugin-root>]
  [--session-database <sqlite-path> | --sessions <session-directory>]
```

The default Session database is `.lenso/sessions.sqlite3`; `--sessions` selects
an explicit file Adapter directory.

The public CLI now calls this `generations gc-preview` so the read-only report
is not confused with an executable Resolved App Plan. `gc-plan` remains a
compatibility alias; the semantics and authority described by this decision do
not change.

It validates and enumerates Generation Specs, retained Plugin Set locks, and
all durable `turn_started` events. A Generation is `protected` when its Plugin
Set is current or retained, when a Session Turn records its digest, or both. A
Generation with neither reference is reported as a `candidate`.

Unknown files, malformed authorities, corrupt Session records, invalid Turn
provenance, and missing referenced Generation Specs fail the command. The plan
does not modify any file and does not cover Plugin Store objects.

## Consequences

- Output is deterministic and exposes only two reasons: `plugin-set` and
  `session`.
- A candidate is an observation, not deletion authorization. Concurrent startup
  or Session writes can change reachability after inspection.
- Deletion, retention windows, background work, Plugin Store collection, and a
  stronger apply-time consistency protocol were deferred by this decision.
  ADR-0035 later adds an explicitly applied, process-fenced deletion path while
  preserving this command as read-only preview.
- This stays in the Host control plane. It adds no Kernel contract, Runtime
  Driver, Capability, policy DSL, or generic graph engine.

## Rejected alternatives

A general retention framework would add policy before there is an apply path.
Deleting candidates in the same command would require stronger coordination
with Generation creation and Session persistence. Moving the planner into the
Kernel would make portable lifecycle machinery own product provenance policy.
