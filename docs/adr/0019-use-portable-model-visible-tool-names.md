# ADR 0019: Use portable model-visible Tool names

- Status: accepted
- Date: 2026-08-26
- Refines: ADR 0004 and ADR 0007

## Context

The first executable slices exposed package-shaped names such as
`workspace.read_text`, `process.exec`, and `skills.read`. Those names made the
owning implementation visible to the Model, consumed more prompt space, and
were not portable across Model providers. The Codex Direct adapter already
normalizes unsupported punctuation to underscores and must reject collisions
created by that normalization.

The existing names also mixed behavior with representation. `write_text` is
create-only, `search` performs bounded case-sensitive literal matching, and
`process.exec` accepts one program plus an argument array rather than shell
grammar. A shorter name must not claim broader behavior than the Provider
implements.

## Decision

Model-visible Tool names use lowercase ASCII snake case and match
`^[a-z][a-z0-9_]{0,63}$`. The Tool Runtime validates that rule while building
the aggregate catalog, before the App generation becomes ready. Duplicate
names continue to fail closed; registration order never selects a winner.

The built-in model-visible names are:

- workspace observation: `list`, `search`, and `read`;
- workspace mutation: `create_file` and `edit`;
- structured process execution: `run_process`;
- progressive Skills: `skill_list`, `skill`, `skill_resources`, and
  `skill_resource`; and
- the removable fixture Plugin: `uppercase`.

No built-in model-visible Tool name contains `text`. Package IDs, crate names,
Capability identities, App Instance keys, and implementation function names
remain owner-facing provenance and are not renamed by this decision.

Names continue to describe exact behavior:

- `search` remains bounded literal search and is not renamed to `grep`;
- `create_file` remains create-only and is not renamed to `write`; and
- `run_process` remains program-plus-arguments execution and is not renamed to
  `shell` or `bash`.

The affected Tool Provider Modules, aggregate Tool Runtime, and deterministic
Fixture Model advance to package revision `0.2.0`. Resolved App Plans must be
regenerated from their `composition/*.app.json` definitions so a fresh App
Generation records the changed executable package revisions. Old and new
names are not exposed together in one catalog.

Stable ecosystem Tool IDs, permission actions, output schemas, and
per-invocation concurrency classification remain future contract work. This
decision changes the current model-facing catalog without adding a registry or
graph mutation to Kernel.

## Consequences

- Model adapters receive names that require no punctuation normalization.
- Tool transcripts become easier for Models to select and cheaper to repeat.
- Existing durable Sessions may contain earlier Tool names; resuming them
  across the new package revisions remains observable through the recorded App
  Generation rather than pretending the catalog did not change.
- A Plugin exposing an invalid model name prevents the candidate Generation
  from becoming ready.
- Adding `glob`, `grep`, `write`, or `shell` later requires the corresponding
  behavior and a separately reviewed semantic change.
