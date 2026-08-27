# Workspace edit Plugin card

Status: implementation baseline for the opt-in coding profile and bundled
Plugin selection.

## `lenso.agent.workspace-edit`

- **Deletion boundary:** removes all workspace mutation Tool definitions and
  file writes; workspace navigation, reads, Skills, the Agent Loop, and Kernel
  remain unchanged.
- **Owned facts:** workspace mutation root, allowed Tool names, exact-replace
  semantics, create-only semantics, path containment, symlink policy, byte
  limits, and atomic file replacement.
- **Provides:** `lenso.agent.tool-provider@2` (`catalog`, `execute`).
- **Requires:** none.
- **Configuration:** canonical workspace root, maximum existing/final file
  bytes, and maximum aggregate exact-replacement bytes.
- **Final authorization:** rejects absolute paths, traversal, every requested
  symlink component, special files, missing edit targets, existing create
  targets, ambiguous replacements, and configured byte-limit violations.
- **Lifecycle/resources:** `prepare` verifies that the root exists and is a
  directory; each invocation owns only its temporary file and cleans it up on
  failure. There is no background work or durable private state.
- **First behavior:** `create_file` atomically creates one new UTF-8
  file below an existing directory; `edit` atomically replaces
  one unique, non-empty UTF-8 substring in one existing UTF-8 file while
  preserving its permissions.

## Selection

The Plugin is absent from the root read-only App. `lenso plugins configure
lenso.agent.workspace-edit` creates its Instance configuration under
`plugins/`. The Host derives one `many` Tool Provider binding to `tools` before
staging the candidate App Generation. `lenso plugins disable
lenso.agent.workspace-edit` removes that authority through the same Ready Gate.
Mutation authority exists only through this visible Plugin Root entry.

Deletion and generic overwrite are deliberately unsupported in this slice.
Approval policy and process execution remain separate Plugins.
The Agent Loop records Tool arguments as durable Session trajectory facts, so
create and replacement text must not contain credentials or other secrets.
