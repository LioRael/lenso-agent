# Workspace edit Module card

Status: implementation baseline for the opt-in coding profile.

## `lenso.agent.workspace-edit`

- **Deletion boundary:** removes all workspace mutation Tool definitions and
  file writes; workspace navigation, reads, Skills, the Agent Loop, and Kernel
  remain unchanged.
- **Owned facts:** workspace mutation root, allowed Tool names, exact-replace
  semantics, create-only semantics, path containment, symlink policy, byte
  limits, and atomic file replacement.
- **Provides:** `lenso.agent.tool-provider@1` (`catalog`, `execute`).
- **Requires:** none.
- **Configuration:** canonical workspace root, maximum existing/final file
  bytes, and maximum aggregate exact-replacement bytes.
- **Final authorization:** rejects absolute paths, traversal, every requested
  symlink component, special files, missing edit targets, existing create
  targets, ambiguous replacements, and configured byte-limit violations.
- **Lifecycle/resources:** `prepare` verifies that the root exists and is a
  directory; each invocation owns only its temporary file and cleans it up on
  failure. There is no background work or durable private state.
- **First behavior:** `workspace.write_text` atomically creates one new UTF-8
  file below an existing directory; `workspace.edit_text` atomically replaces
  one unique, non-empty UTF-8 substring in one existing UTF-8 file while
  preserving its permissions.

## Composition

The Module is absent from every existing readonly Composition. The opt-in
headless and direct ChatGPT coding Compositions add one `workspace-edit`
Instance and one explicit `many` Tool Provider binding. Removing the Instance,
binding, and package input restores the corresponding readonly graph without a
Tool Runtime, Agent Loop, Adapter, Driver, or Kernel branch.

Deletion and generic overwrite are deliberately unsupported in this slice.
Approval policy and process execution remain separate future Modules.
The Agent Loop records Tool arguments as durable Session trajectory facts, so
create and replacement text must not contain credentials or other secrets.
