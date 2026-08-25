# Filesystem Skills Module card

## `lenso.agent.skills.filesystem`

- **Deletion boundary:** removes the bounded Model-visible Skill metadata
  catalog and all `skills.*` Tools. Prompt aggregation, Tool Runtime, Agent
  Loop, Session, Runner, and Kernel remain unchanged.
- **Owned facts:** configured Skill root, startup catalog membership, parsed
  names/descriptions, immutable Skill and resource contents, content versions,
  omitted resource counts, and Prompt catalog omissions.
- **Provides:** `lenso.agent.prompt-provider@1` (`contribute`, request) and
  `lenso.agent.tool-provider@1` (`catalog` and `execute`, request).
- **Requires:** none. App Composition binds the same keyed Instance explicitly
  to the Prompt and Tool Runtime aggregates.
- **Configuration:** rooted Skill directory; catalog contribution ID; Skill,
  document, resource, aggregate, manifest, and Prompt catalog limits. No Skill
  content or local absolute resolved path enters the Resolved App Plan.
- **Lifecycle/resources:** `prepare` canonicalizes the configured root and
  creates one immutable snapshot. It validates every visible Skill document,
  rejects symlinks and special resource entries, and snapshots bounded UTF-8
  resources. It starts no work and executes no script.
- **Authorization:** every Tool invocation uses an exact snapshotted Skill name
  and, for resources, a normalized relative path. The Module owns the final
  containment and size decision.
- **Failure policy:** invalid configuration is an Invalid Resolved Plan;
  unavailable/malformed roots, Skills, symlinks, and aggregate-limit failures
  reject startup; invalid Tool arguments, missing resources, and output limits
  remain classified Domain Errors.
- **First behavior:** Prompt assembly receives only ordered names and
  descriptions. A matching task calls `skill` directly, then optionally
  lists and reads one referenced resource. `skill_list` is retained for
  diagnostics and Prompt catalog overflow.

## Removal proof

Removing the `skills` Instance, both of its bindings, and its package input
leaves `openai-codex-direct` valid with workspace-read Tools and static Prompt
contributions. No Kernel, Driver, Native Adapter, Agent Loop, Prompt Runtime, or
Tool Runtime branch remains.
