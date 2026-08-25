# Terminal surface Module cards

## TUI Shell Module

- **Owner:** `lenso-agent-tui-module` owns the interactive terminal product
  surface; `lenso-agent-cli` supplies its native Host integration.
- **Deletion boundary:** removing the Shell and its `tui` Instance removes raw
  terminal mode, layout, input, streamed rendering, cancellation UX, Session
  selection, and panel aggregation. Agent, Model, Tool, Prompt, and Session
  behavior remain valid.
- **Required Capabilities:** one `lenso.agent@1`; many
  `lenso.agent.tui-contribution@1`.
- **Provided Capabilities:** none. It is a user-facing consumer and does not
  invent a Provider solely for source-first metadata generation.
- **Configuration and state:** empty immutable configuration. Input,
  transcript rendering, selected panel, and active stream are volatile. The
  durable Session remains owned by the Session Module.
- **Lifecycle:** the Host opens terminal raw mode only after App readiness and
  successful Contribution snapshot validation. Drop restores terminal state.
- **Final authorization:** none. Turn Tool authority is only narrowed through
  the existing invocation scope; target Tool Modules retain final authority.
- **First observable behavior:** running `lenso-agent` without arguments opens
  the conversation, accepts one prompt, streams the Agent response, and shows
  selected semantic panels.

## Static TUI Contribution Module

- **Owner:** `lenso-agent-tui-static-module` owns configured read-only panel
  content and provider-local uniqueness checks.
- **Deletion boundary:** removing one Instance removes only its panels; the
  TUI Shell and Agent continue running.
- **Provided Capability:** `lenso.agent.tui-contribution@1` snapshot.
- **Required Capabilities:** none.
- **Configuration:** bounded panel IDs, titles, and bodies. Runtime validation
  rejects empty/oversized values and duplicate provider-local IDs.
- **Lifecycle and state:** stateless; no managed work or persistence.
- **Final authorization:** not applicable because v1 panels expose no action.
- **First observable behavior:** the selected `tui-help` Instance appears as a
  Help panel; removing it resolves a valid nine-Instance App with no panel
  binding.
