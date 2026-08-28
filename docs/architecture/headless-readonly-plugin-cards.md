# Headless read-only Plugin cards

Status: implementation baseline for the first executable slice.

## `lenso.agent.model.fixture`

- **Deletion boundary:** removes deterministic model completion used by local
  proof and tests; no Agent, Tool, Session, or Kernel behavior changes.
- **Owned facts:** fixture script and deterministic response policy.
- **Provides:** `lenso.agent.model@2` (`complete`, stream).
- **Requires:** none.
- **Configuration:** exact fixture model name.
- **Lifecycle/resources:** endpoint-only; no durable state or managed work.
- **First behavior:** deterministically proves direct answers, sequential
  workspace navigation/read calls, and resumed-turn context.

## `lenso.agent.workspace-read`

- **Deletion boundary:** removes read-only workspace Tool definitions and file
  access.
- **Owned facts:** workspace root, allowed Tool names, path containment and
  hidden-entry policy, traversal/search budgets, and response limits.
- **Provides:** `lenso.agent.tool-provider@2` (`catalog`, `execute`).
- **Requires:** none.
- **Configuration:** canonical workspace root; maximum list/search entries,
  scanned bytes, search matches, and output bytes.
- **Final authorization:** validates every requested path component, rejects
  traversal and every encountered symlink or special entry, omits hidden
  entries, and skips non-UTF-8 search inputs.
- **Lifecycle/resources:** `prepare` verifies that the root exists and is a
  directory; no background work.
- **First behavior:** lists one directory, recursively performs a bounded
  case-sensitive literal search, and reads one UTF-8 text file.

## `lenso.agent.tools`

- **Deletion boundary:** removes the App-facing aggregate Tool catalog and
  deterministic dispatch.
- **Owned facts:** aggregate name uniqueness and provider routing table.
- **Provides:** `lenso.agent.tools@2` (`catalog`, `execute`, `execute_stream`).
- **Requires:** `lenso.agent.tool-provider@2`, `lenso.agent.tool-progress@1`,
  and `lenso.agent.tool-hook@1`, each with `many` cardinality.
- **Configuration:** empty.
- **Lifecycle/resources:** `activate` obtains only explicitly bound Provider
  handles and builds the catalog; no discovery or global registry.
- **First behavior:** exposes and dispatches `list`,
  `search`, and `read`.

## `lenso.agent.prompt.static`

- **Deletion boundary:** removes one explicitly configured set of Prompt or
  Skill contributions; the aggregate and Agent remain runnable.
- **Owned facts:** contribution IDs, versions, kinds, content, and local order.
- **Provides:** `lenso.agent.prompt-provider@1` (`contribute`).
- **Requires:** none.
- **Configuration:** one bounded contribution list; malformed or duplicate IDs
  reject the Plugin generation.
- **Lifecycle/resources:** endpoint-only; no discovery, storage, or managed
  work.
- **First behavior:** contributes fixture instructions or the workspace summary
  Skill selected by Composition.

## `lenso.agent.prompt.filesystem`

- **Deletion boundary:** removes loading of explicitly selected filesystem
  Skills; static Prompt Providers and the aggregate remain unchanged.
- **Owned facts:** configured Skill root, selected names, contribution ID
  prefix, file/aggregate limits, containment policy, and startup snapshot.
- **Provides:** `lenso.agent.prompt-provider@1` (`contribute`).
- **Requires:** none.
- **Configuration:** one root such as `~/.agents/skills`, an ordered non-empty
  Skill-name list, ID prefix, and finite byte limits.
- **Final authorization:** canonicalizes each selected
  `<root>/<name>/SKILL.md`, rejects traversal and targets outside the root, and
  validates UTF-8 plus minimal Skill frontmatter before reading it into the
  snapshot.
- **Lifecycle/resources:** `prepare` loads one immutable generation snapshot;
  there is no directory watcher, script execution, or background work.
- **First behavior:** contributes only the explicitly selected Skill documents
  in Composition order with content-derived versions.

## `lenso.agent.prompt`

- **Deletion boundary:** removes deterministic Prompt aggregation and the
  Agent-facing Prompt endpoint.
- **Owned facts:** cross-provider ID uniqueness, explicit ordering, aggregate
  byte/count limits, and content digests.
- **Provides:** `lenso.agent.prompt@1` (`assemble`).
- **Requires:** `lenso.agent.prompt-provider@1` with `many` cardinality.
- **Configuration:** maximum contribution count and aggregate content bytes.
- **Lifecycle/resources:** `activate` obtains only explicitly bound Provider
  handles and snapshots their contributions; no global registry or file scan.
- **First behavior:** returns one system prompt and its ordered audit manifest.

## `lenso.agent.session.file`

- **Deletion boundary:** removes durable Session identity, events, revisions,
  recovery, and the Plugin-private file store.
- **Owned facts:** append-only event ordering, optimistic revision checks,
  idempotent event IDs, retention boundary, and file format.
- **Provides:** `lenso.agent.session@1` (`open`, `read`, `append`).
- **Requires:** none.
- **Configuration:** durable store directory.
- **Transaction boundary:** one Session append batch under an exclusive
  in-process lock, persisted through a temporary file and atomic rename.
- **Failure policy:** invalid or unavailable storage rejects startup or returns
  a Runtime Failure; there is no in-memory fallback.
- **First behavior:** survives a fresh Plugin generation and process restart.

## `lenso.agent.session.sqlite`

- **Deletion boundary:** removes the transactional SQLite Session Adapter.
- **Owned facts:** normalized SQLite schema, WAL setup, transaction isolation,
  uniqueness constraints, and database-path lifecycle.
- **Provides:** `lenso.agent.session@1` (`open`, `read`, `append`).
- **Requires:** none.
- **Configuration:** one durable SQLite database path.
- **Transaction boundary:** one immediate SQLite transaction per append batch;
  revision and event rows commit together.
- **Failure policy:** schema, constraint, corruption, and I/O failures are
  Runtime Failures; there is no file or in-memory fallback.
- **First behavior:** provides the default transactional Session store; a
  Profile can replace it with the file Adapter without changing the Agent Loop
  or provenance tooling.

## `lenso.agent.loop`

- **Deletion boundary:** removes Turn/Step coordination, budgets, sequencing,
  and terminal Agent outcomes.
- **Owned facts:** active Turn exclusion, maximum model steps/tool calls,
  message construction, and Session event intent.
- **Provides:** `lenso.agent@3` (`run_turn`, stream).
- **Requires:** exactly one `lenso.agent.model@2`, one
  `lenso.agent.prompt@1`, one `lenso.agent.tools@2`, and one
  `lenso.agent.session@1`.
- **Configuration:** model name, maximum steps, maximum Tool calls, bounded
  parallel Tool calls, aggregate model output-token budget, and bounded
  Session-history event count.
- **Lifecycle/resources:** `activate` materializes generated clients only from
  `PluginDependencies`; each generation owns its client set, active-Turn state,
  and Driver-managed turn tasks. Each Agent stream uses a one-item internal
  channel so a slow consumer backpressures the Loop.
- **First behavior:** reconstructs bounded completed-turn context, accepts a
  direct answer or bounded parallel-safe Tool waves with exclusive barriers
  until a finite budget is reached,
  prepends the assembled Prompt, records its contribution manifest, persists
  terminal facts, and forwards Model text deltas immediately.

## `lenso.agent.cli`

- **Deletion boundary:** removes terminal input/rendering and the external
  consumer edge; the Agent Capability remains invocable by another UI Plugin.
- **Owned facts:** selected Session ID, terminal presentation, and local Ctrl-C
  cancellation.
- **Provides:** none.
- **Requires:** exactly one `lenso.agent@3`.
- **Configuration:** empty.
- **Lifecycle/resources:** no endpoint; the Runner uses this Instance's
  explicitly resolved stream binding.
- **First behavior:** one-shot `run --workspace ... --prompt ...` execution.

## Composition deletion proof

The fixture Model, Prompt Providers, and workspace Tool Provider are
replaceable selections. A
fixture without the workspace Provider removes that package, Instance, binding,
and configuration, then resolves the remaining graph after rebinding the Tools
consumer to zero providers. Removing all Prompt Providers leaves an empty but
valid aggregate Prompt. Removing the Agent product requires removing the
CLI consumer and all Agent-owned Instances; Kernel, Driver, and Native Adapter
remain unchanged.
