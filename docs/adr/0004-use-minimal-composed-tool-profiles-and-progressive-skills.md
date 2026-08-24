# ADR-0004: Use minimal composed Tool profiles and progressive Skills

Status: Accepted

## Context

A coding Agent can expose only generic file and process primitives, while a
business Agent benefits from typed domain Tools. Making every concern a
specialized built-in Tool bloats Model context and couples the Agent Loop to one
product surface. Treating a shell as universal authority weakens permissions,
portability, replay evidence, and final authorization.

Skill discovery has a related context problem. Requiring `skills.list` before
every selection wastes a Tool step, but injecting every full `SKILL.md` defeats
progressive disclosure.

## Decision

Tool profiles are App Composition authoring recipes. They are not Kernel modes,
Tool Runtime configuration, hidden registries, or live permission switches.
Each recipe expands before resolution to ordinary Tool Provider Module
Instances and explicit `lenso.agent.tool-provider@1` bindings.

The initial profile vocabulary is:

- `readonly`: rooted observation Providers, including bounded workspace
  listing, literal text search, file reads, and Skill documents/resources;
- `coding`: `readonly` plus the separately removable create-only/exact-edit
  workspace mutation Provider; process execution remains a later Provider; and
- `automation`: explicitly selected typed domain Providers, without raw
  workspace or process authority by default.

The Agent Loop and Tool Runtime remain profile-agnostic. The immutable Resolved
App Plan is the authority for the selected Tool surface. `tools.search` is
deferred until real Compositions contain enough Tools to justify dynamic Tool
schema discovery.

The filesystem Skills Module provides both
`lenso.agent.prompt-provider@1` and `lenso.agent.tool-provider@1`. During
`prepare`, it creates one immutable, bounded snapshot of Skill metadata,
documents, and readable resources. Its Prompt contribution contains only
ordered names and descriptions. The Model calls `skills.read` for the selected
full document and uses resource Tools only when that document refers to them.
`skills.list` remains available for diagnostics and deterministic Prompt
catalog overflow.

The Prompt catalog has an App-configured contribution ID and byte limit. When
all entries do not fit, the Module deterministically includes the sorted prefix,
reports the omitted count, and directs the Model to `skills.list`. Prompt and
Session manifests carry the catalog content digest; Skill bodies, resources,
and local absolute paths are excluded.

## Consequences

- The common Model-visible surface stays small without making shell execution a
  universal Harness capability.
- Adding or removing a Tool profile changes ordinary Composition bindings and
  requires a new Resolved App Plan and App generation.
- Skills are procedural knowledge, not authority. A Skill cannot read, write,
  execute, or access a network unless the Composition selected the necessary
  Tool Provider.
- Removing the filesystem Skills Module removes both its Prompt catalog and all
  `skills.*` Tools without a Kernel or Agent Loop branch.
