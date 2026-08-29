# ADR-0065: Render task supervision through read-only surfaces

Status: Accepted

## Context

ADR-0060 established `lenso.agent.task-supervisor@2` as the surface-neutral
child-task projection owned by `lenso.agent.subagent-tools`. The model can read
that projection through `list_subagents`, but operators using the TUI or Web
surface cannot observe the same running and terminal task facts. Rebuilding
those facts from Session events would omit running tasks and create competing
surface-owned lifecycle models.

## Decision

The TUI and Web consumer Plugins require many optional
`lenso.agent.task-supervisor@2` providers. The Host reads those bindings through
the active immutable Generation, rejects duplicate task IDs or aggregates over
64 tasks, and returns the source-first `SnapshotResponse` type. Surfaces do not
cache a second domain model and receive no scheduling operation.

The TUI polls the typed snapshot once per second and projects it into one compact
Tasks context panel. The panel keeps stable geometry, summarizes active versus
terminal work, shows at most eight stable task-ID-ordered rows, and reports
bounded progress counters and content, and reports unavailability without
stopping the current Generation. The Web surface exposes
`GET /api/console/v1/agent/tasks` with the exact snake-case Capability response
and services reads while an Agent Turn is running. Clients may reconnect and
read the same Generation-local Provider state. While a Turn is active, both
surfaces read through that Turn's lease so an online switch cannot hide its
still-running children; idle reads use the current Generation route.

No surface endpoint starts, waits for, cancels, integrates, or removes a child
task. Those actions remain model Tools owned by the subagent Plugin and the
explicit reviewed parent workflow.

## Consequences

- TUI and Web observe one typed task lifecycle instead of reconstructing it;
- surface reconnect preserves visible tasks while the Generation remains alive;
- polling is bounded and read-only, so presentation cannot become a scheduler;
- task state still does not survive Host suspension or Generation replacement;
  and
- a future streaming transport can reuse the same snapshot without changing
  ownership or authority.

## Proof

Descriptor tests prove both surface Plugins consume the typed Capability with
many cardinality. Host Plan tests prove the resolved surface bindings target the
selected Task Supervisor. TUI tests cover stable compact projection including
progress. Web integration tests cover capability discovery, the exact empty
snapshot shape, and a second client reconnecting to the same two completed
child-task progress records while their parent Turn waits for explicit
integration approval.
