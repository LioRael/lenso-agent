# ADR 0040: Select Session Profiles before Generation resolution

- Status: accepted
- Date: 2026-08-28
- Extends: ADR 0011, ADR 0026, and ADR 0039

## Context

One installed Agent Harness should support focused coding, game-building, and
other Agent experiences without copying an App manifest or mutating a running
Kernel. Each experience may need a different subset of installed Plugins, a
configured Model Instance, and occasionally a different Plugin that provides
the root Agent Loop.

Putting configuration inside a Profile would create a second configuration
authority beside `plugins/<plugin-id>/<instance>.toml`. Treating a Profile as a
runtime mode would also let Sessions escape the immutable Generation and Ready
Gate rules.

## Decision

A Session Profile is a small authoring-time selector stored at
`profiles/<name>.toml`. It contains:

- `instances`: an allowlist of exact configured Plugin Instance identities;
- `agent`: the exact Agent provider bound to CLI, TUI, and channel surfaces;
  and
- an optional human-readable `description`.

The selected Instance configuration must already exist at
`plugins/<plugin-id>/<instance>.toml`. Installed release descriptors remain
available for resolution, but unselected Plugin Root Instances do not enter the
candidate App. Host defaults remain the stable Harness base. An explicit Model
Instance replaces the default through the existing replaceable `model` slot.
An explicit Agent provider is bound through the existing Host binding seam.

`lenso-agent --profile <name>` and `lenso-agent-cli --profile <name>` apply the
selector before resolving an immutable Plan. The resulting Generation—not the
Profile name—is execution authority and is recorded in Session Turn provenance.
Resuming a Session uses the selected Profile for the new Host invocation; a
Session may deliberately move to another Generation on a later Turn.

The online reconciler retains the Profile name, watches both `plugins/` and
`profiles/`, reapplies the selector, and sends every candidate through the same
Ready Gate and overlap transition used by ordinary Plugin changes.

## Consequences

- Plugin configuration has one owner and one filesystem location.
- A Profile can select Tools, Skills, a Model configuration, and a compatible
  Agent Loop without exposing Host Slots or handwritten Capability bindings.
- Deleting the Profile feature would push filtering and Agent binding choices
  back into every surface, so the selector earns a distinct interface.
- Exact Plan replay conflicts with `--profile`; replay already supplies the
  complete runtime authority.
- The first interface selects a Profile when starting or resuming a terminal
  Session. In-TUI Profile switching and automatic name recall are deferred; the
  Session's Generation provenance remains sufficient to audit what ran.

## Rejected alternatives

A second `lenso.app.toml`-style composition document duplicates Plugin Root
authority. Embedding arbitrary configuration tables in Profiles creates two
owners for the same Plugin Instance. Changing a Session's active Profile inside
one Turn would violate Generation pinning. Hard-coding code-agent and game-agent
variants in the TUI would duplicate resolution logic across surfaces.
