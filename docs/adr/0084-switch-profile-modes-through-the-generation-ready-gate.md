# ADR-0084: Switch Profile modes through the Generation Ready Gate

Status: Accepted

## Context

Normal, Plan, and Auto are not cosmetic prompt modes. A Profile can change the
selected Agent, Plugins, Tool authority, and configuration. The TUI nevertheless
needs the immediate, legible mode control established by coding harnesses such
as Grok Build.

## Decision

The online Generation reconciler accepts an explicit Profile selection request.
It resolves the requested Profile through the normal immutable Plan derivation
and Ready Gate, retaining the previous selection when reconciliation is busy,
rejected, or fails. Existing Turn leases remain pinned; a successful selection
affects only subsequent leases.

The TUI maps Normal to the default Profile, Plan to `plan`, and Auto to `code`.
When the prompt owns focus, Shift+Tab cycles Normal, Plan, Auto. `/mode` exposes
the same control. The composer bottom border renders the selected model,
reasoning/fast flags, and a right-aligned mode indicator; a pending transition
uses an ellipsis until the Ready Gate settles.

## Consequences

- mode switching cannot bypass Plugin or Tool authority;
- failed transitions visibly preserve the previous mode;
- running Turns remain reproducible under their original Generation; and
- model, thinking, fast tier, permissions, and Profile mode remain separate
  control axes.

## Proof

Host tests cover Profile selection and Generation reconciliation. TUI state and
render tests cover the Normal/Plan/Auto cycle, command parsing, and the pending
bottom-right indicator. Workspace checks prove all surfaces compile against the
same Host control path.
