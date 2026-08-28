# Agent Harness architecture

## User outcome

A developer can start a useful Agent with no App file, add or configure one
Plugin through ordinary files under `plugins/`, observe a streamed answer, and
resume a durable Session after restart. Invalid configuration, missing secrets,
provider failure, exhausted budgets, and unavailable storage remain explicit
failures.

## Runtime graph

```text
Host defaults + plugins/
  -> deterministic resolver
  -> immutable App Plan
  -> Agent
       |- Model Plugin
       |- Tool Runtime Plugin -> Tool Provider Plugins
       |- Prompt Plugin -> Prompt Provider Plugins
       |- Context Compaction Plugin
       |- Memory Plugin
       `- Session Plugin
```

Capabilities are typed contracts between Plugins. They are not independently
selectable product units. The Host Catalog owns default Instances, root Slots,
and private attachments; Plugin publishers cannot inject bindings or mutate a
running graph.

## First Turn

1. The Host snapshots `plugins/`, derives a Plan, and readies one Generation.
2. The surface leases that Generation and opens `lenso.agent@3/run_turn`.
3. Agent opens or resumes a Session, restores or refreshes its bounded context
   projection, and records `turn_started`.
4. Agent obtains the installed System Instruction and derived Tool catalog.
5. Model output streams to the surface; complete Tool calls are recorded before
   execution.
6. Tool Runtime validates and dispatches to the Plan-bound Provider Plugin.
7. Agent records the result and continues within finite step and call budgets.
8. A terminal event is recorded before the Generation lease is released.

## Ownership

- Session owns durable identity, revision, event order, recovery, and retention.
- Agent owns volatile Turn orchestration reconstructed from Session events.
- Context Compaction owns replaceable summary and retained-tail behavior; the
  Agent owns trigger, validation, and durable checkpoint transactions.
- Memory owns curated cross-Session knowledge, provenance, retrieval, and
  deletion; the Agent owns when bounded recall enters one Turn and records the
  observable outcome in Session.
- Tool Runtime owns catalog aggregation and routing, but no second Plugin list.
- Tool Providers own definitions, resource policy, execution, and final domain
  authorization.
- Model Plugins own provider protocol, egress, cancellation, and error mapping.
- Host owns Catalog generation, Plugin Root resolution, readiness, routing
  leases, and Generation drain.
- Kernel executes only the immutable Plan and owns no product composition.

## Trust

Workspace Plugins canonicalize roots and reject traversal, symlinks, special
files, and configured limits. Process Plugins own executable, cwd, environment,
argument, timeout, output, and cleanup policy. Model credentials resolve from
environment-backed secret providers and never enter configuration or Session
events. Adding a Plugin grants only the capabilities and resources present in
the derived Plan.

## Acceptance boundary

Tests prove default boot without App files; deterministic Plugin Root
resolution; external Wasm Plugin packaging and execution; invalid-candidate
rollback; Generation lease continuity; bounded Sessions and Turns; exact Tool
authorization; workspace containment; process cancellation; and removal of
optional Plugins without changing Kernel code.
