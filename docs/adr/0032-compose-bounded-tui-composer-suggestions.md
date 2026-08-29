# ADR 0032: Compose bounded TUI composer suggestions

- Status: accepted
- Date: 2026-08-26
- Extends: ADR 0020 and ADR 0023

## Context

The TUI composer needs Grok Build-style slash-command and workspace-file
completion without teaching the Shell to discover Modules or granting a UI
component ambient filesystem authority. Querying the filesystem on every key
press would also make terminal responsiveness depend on workspace size.

## Decision

Define native-only request Capability `lenso.agent.tui-suggestion@1`. Its
`snapshot` Operation returns bounded semantic `command`, `skill`, `file`,
`prompt`, or `resource` items with a
stable ID, label, exact insertion text, and description. The TUI Shell consumes
explicitly bound providers with `many` cardinality, snapshots them before raw
terminal mode, rejects duplicate IDs and aggregate limit violations, and then
filters the immutable in-memory catalog at the active composer token.

The base App selects three independently removable providers. The command
provider owns the reviewed `/help`, `/clear`, `/new`, and `/rename` actions. The workspace
provider owns one startup filesystem snapshot below its configured root. It
skips symlinks and hidden entries, supports explicit directory exclusions, and
bounds both inspected entries and returned files. It does not watch the
workspace or perform I/O while the user types.

The filesystem Skills Plugin is also a suggestion provider. It projects only
the already bounded name and description metadata from its prepared Skill
snapshot. Selecting `/skill-name` inserts the explicit Skill invocation and a
space without submitting the Turn; the user can then write the task. The same
Plugin's Prompt contribution tells the Agent to read that exact Skill before
following it, so the TUI never reads or executes Skill contents itself.

The TUI also projects metadata from its explicitly bound Context Sources.
No-argument Prompts and text Resources appear under `/`; accepting one inserts
a semantic selection token and leaves the task composer open. The Shell resolves
that exact selection on submit, so MCP Prompt selection remains user-controlled
and Resource attachment remains application-controlled.

The Shell owns trigger parsing, ranking, keyboard selection, token replacement,
scrolling, and responsive rendering. `/` is recognized only at the start of the
current line; `@` completes the active file token. Enter or Tab accepts a
candidate, arrows select, and Esc dismisses the menu before it can exit the
TUI. Command selection submits immediately, while Skill and file selection
leave the composer open.

## Consequences

- Other Modules can contribute suggestions only through App Composition.
- Removing either provider removes only its candidate kind.
- Removing all providers leaves the composer operational without a dropdown.
- Startup cost is bounded and observable; per-keystroke filesystem work is
  absent.
- The first contract is native-only and does not claim Bun or Wasm portability.
