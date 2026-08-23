# ADR-0003: Compose Prompt and Skill contributions as ordinary Modules

Status: Accepted

## Context

The Harness already replaces Model, Tool Provider, and Session behavior through
App Composition. Prompt instructions and reusable Skills must have the same
property without teaching the Agent Loop to scan directories, load code, or
consult a process-global plugin registry.

Prompt content affects every Model request and therefore needs finite size,
deterministic order, collision handling, and durable trajectory evidence.
Runtime hot-loading would conflict with the immutable Resolved App Plan and
would make a resumed Session unable to identify the instructions used earlier.

## Decision

Define two portable request Capabilities:

- `lenso.agent.prompt-provider@1` allows a removable Module to contribute an
  ordered list of versioned `instruction` or `skill` entries.
- `lenso.agent.prompt@1` exposes one assembled prompt and an ordered manifest
  containing each contribution ID, version, kind, and SHA-256 content digest.

The `lenso.agent.prompt` aggregate requires Prompt Providers with `many`
cardinality. It preserves the explicit binding order and provider-local order,
rejects duplicate IDs, and enforces configured contribution and byte limits.
The Agent Loop requires exactly one aggregate, prepends its content as one
system message, and records the manifest in each `model_requested` Session
event. Prompt contents remain outside Session events.

Static Prompt plugins are ordinary keyed Instances of
`lenso.agent.prompt.static`. App Composition owns their content, order, and
bindings. Removing every provider leaves the aggregate valid and produces an
empty prompt; no Kernel, Driver, or Adapter behavior changes.

An optional `lenso.agent.prompt.filesystem` Provider may read explicitly named
`SKILL.md` documents below one configured root, including
`~/.agents/skills`. It snapshots them during startup, enforces canonical path
containment and byte limits, and derives contribution versions from content.
It does not enumerate unselected Skills, execute assets, or watch for changes.

## Consequences

- Prompt and Skill behavior is packageable and explicitly reviewable in the
  same immutable graph as the rest of the Harness.
- A Session trajectory can identify prior Prompt inputs without duplicating
  their full contents.
- Changing a contribution requires resolving and restarting a new App
  generation; live mutation is deliberately unsupported.
- Dynamic Skill discovery, model-driven selection, recursive filesystem
  scanning, and marketplace installation remain separate product slices.
