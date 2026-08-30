# ADR-0056: Compose the official coding and planning Profiles

Status: Accepted

## Context

The Harness already exposes bounded Workspace edit, Process, Git, Code Mode,
subagent, Tool Hook, Prompt, Profile, and User Interaction seams. A user still
has to understand their individual configurations before the Harness behaves
like a coding product. Workspace-owned instructions are also absent, and the
durable one-shot Approval Hook requires an external approve-and-retry loop that
does not fit an interactive terminal workflow.

Hard-coding a coding mode in Agent Loop or the TUI would duplicate Profile
selection and make removable behavior permanent. Calling the native Process
provider a sandbox would overstate its authority boundary.

## Decision

`lenso-agent-cli profiles install coding` installs two reviewed, inspectable
Session Profiles and their Plugin Instance configurations under Agent Home. It
is idempotent for exact official content and refuses to overwrite a customized
file. ADR 0079 later moves the official Prompt bytes into Host-owned
configuration defaults so new Sessions receive Prompt improvements with a Host
update while visible empty Instance files retain local override semantics.
Installed Profile-only Instances carry disabled markers, so installing the
Profiles does not change or invalidate the default App. The official Profiles
set `include_enabled = true`, enabling their declared Profile-only Instances
while retaining enabled App-wide Plugin Root differences such as model
configuration. Profiles without that explicit field keep exact-selection
semantics.

The `code` Profile selects Workspace edit, constrained native Process, semantic
Git, Code Mode, bounded subagents, hierarchical Workspace instructions, one
coding Prompt contribution, and an interactive Approval Hook. The Hook allows
an explicit read-only Tool set and asks through
`lenso.agent.user-interaction@2` for every other Tool call. Approval is scoped
to the exact blocked invocation and does not create ambient authority. A
non-interactive surface cannot satisfy the question and therefore remains
fail-closed.

The `plan` Profile selects only hierarchical Workspace instructions and a
read-only planning Prompt contribution. Host defaults still provide the Model,
Session, Memory, read-only Workspace Tools, and surface; the Profile does not
select edit, Process, Git, Code Mode, or subagent authority.

`lenso.agent.workspace-instructions` is a removable Prompt Provider Plugin. At
Generation preparation it finds the nearest Git boundary, reads regular
non-symlink `AGENTS.md` files from repository root to current working
directory, enforces per-file and aggregate bounds, and contributes them in
that order. Outside a Git repository it reads only the current working
directory. The installed System Instruction then retains the exact content for
the Session under ADR-0043.

The native Process provider remains trusted execution constrained by its
configured executable, environment, root, output, argument, timeout, and
cancellation policies. It is not an OS sandbox. A sandbox Profile requires a
separate isolated Process Adapter and is deferred.

## Consequences

- a new user reaches an inspectable coding or planning experience with one
  installation command and one Profile flag;
- nested Workspace instructions follow a deterministic broad-to-specific
  order without changing Agent Loop;
- the TUI can approve a Tool call inline through the existing portable User
  Interaction Capability;
- deleting either Profile or selected Plugin Instance removes that behavior;
  and
- worktree supervision, named Agent teams, ACP/IDE entrypoints, provider
  catalog UX, marketplace UX, GitHub/CI workflows, browser control, OTLP, and
  evaluation remain independent later slices.

## Proof

Plugin tests cover hierarchical order, non-repository containment, policy
precedence, and UTF-8-safe previews. CLI tests prove idempotent installation,
custom-file preservation, and successful resolution of both official
Profiles. Existing Host and headless suites continue to prove immutable Plan,
Ready Gate, Session, Tool, Process, Git, and subagent behavior.
