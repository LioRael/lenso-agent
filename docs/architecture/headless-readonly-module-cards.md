# Headless read-only Module cards

Status: implementation baseline for the first executable slice.

## `lenso.agent.model.fixture`

- **Deletion boundary:** removes deterministic model completion used by local
  proof and tests; no Agent, Tool, Session, or Kernel behavior changes.
- **Owned facts:** fixture script and deterministic response policy.
- **Provides:** `lenso.agent.model@1` (`complete`, stream).
- **Requires:** none.
- **Configuration:** exact fixture model name.
- **Lifecycle/resources:** endpoint-only; no durable state or managed work.
- **First behavior:** deterministically proves direct answers, one or two
  sequential `workspace.read_text` calls, and resumed-turn context.

## `lenso.agent.workspace-read`

- **Deletion boundary:** removes read-only workspace Tool definitions and file
  access.
- **Owned facts:** workspace root, allowed Tool names, path containment policy,
  file and response limits.
- **Provides:** `lenso.agent.tool-provider@1` (`catalog`, `execute`).
- **Requires:** none.
- **Configuration:** canonical workspace root and maximum output bytes.
- **Final authorization:** resolves and canonicalizes every requested path,
  rejects traversal/symlink escape, directories, and oversized output.
- **Lifecycle/resources:** `prepare` verifies that the root exists and is a
  directory; no background work.
- **First behavior:** reads `README.md` as UTF-8 text.

## `lenso.agent.tools`

- **Deletion boundary:** removes the App-facing aggregate Tool catalog and
  deterministic dispatch.
- **Owned facts:** aggregate name uniqueness and provider routing table.
- **Provides:** `lenso.agent.tools@1` (`catalog`, `execute`).
- **Requires:** `lenso.agent.tool-provider@1` with `many` cardinality.
- **Configuration:** empty.
- **Lifecycle/resources:** `activate` obtains only explicitly bound Provider
  handles and builds the catalog; no discovery or global registry.
- **First behavior:** exposes and dispatches `workspace.read_text`.

## `lenso.agent.session.file`

- **Deletion boundary:** removes durable Session identity, events, revisions,
  recovery, and the Module-private file store.
- **Owned facts:** append-only event ordering, optimistic revision checks,
  idempotent event IDs, retention boundary, and file format.
- **Provides:** `lenso.agent.session@1` (`open`, `read`, `append`).
- **Requires:** none.
- **Configuration:** durable store directory.
- **Transaction boundary:** one Session append batch under an exclusive
  in-process lock, persisted through a temporary file and atomic rename.
- **Failure policy:** invalid or unavailable storage rejects startup or returns
  a Runtime Failure; there is no in-memory fallback.
- **First behavior:** survives a fresh Module generation and process restart.

## `lenso.agent.loop`

- **Deletion boundary:** removes Turn/Step coordination, budgets, sequencing,
  and terminal Agent outcomes.
- **Owned facts:** active Turn exclusion, maximum model steps/tool calls,
  message construction, and Session event intent.
- **Provides:** `lenso.agent@1` (`run_turn`, stream).
- **Requires:** exactly one `lenso.agent.model@1`, one
  `lenso.agent.tools@1`, and one `lenso.agent.session@1`.
- **Configuration:** model name, maximum steps, maximum Tool calls, aggregate
  model output-token budget, and bounded Session-history event count.
- **Lifecycle/resources:** `activate` materializes generated clients only from
  `ModuleDependencies`; each generation owns its client set, active-Turn state,
  and Driver-managed turn tasks. Each Agent stream uses a one-item internal
  channel so a slow consumer backpressures the Loop.
- **First behavior:** reconstructs bounded completed-turn context, accepts a
  direct answer or sequential Tool calls until a finite budget is reached,
  persists terminal facts, and forwards Model text deltas immediately.

## `lenso.agent.cli`

- **Deletion boundary:** removes terminal input/rendering and the external
  consumer edge; the Agent Capability remains invocable by another UI Module.
- **Owned facts:** selected Session ID, terminal presentation, and local Ctrl-C
  cancellation.
- **Provides:** none.
- **Requires:** exactly one `lenso.agent@1`.
- **Configuration:** empty.
- **Lifecycle/resources:** no endpoint; the Runner uses this Instance's
  explicitly resolved stream binding.
- **First behavior:** one-shot `run --workspace ... --prompt ...` execution.

## Composition deletion proof

The fixture Model and workspace Tool Provider are replaceable selections. A
fixture without the workspace Provider removes that package, Instance, binding,
and configuration, then resolves the remaining graph after rebinding the Tools
consumer to zero providers. Removing the Agent product requires removing the
CLI consumer and all Agent-owned Instances; Kernel, Driver, and Native Adapter
remain unchanged.
