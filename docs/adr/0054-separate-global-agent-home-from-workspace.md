# ADR-0054: Separate the global Agent Home from the Workspace

Status: Accepted

## Context

The Harness previously treated the process current directory as three different
things: the user's App configuration root, the durable runtime state root, and
the Workspace exposed to Agent Tools. Starting the same installed Agent from a
different repository therefore selected a different `plugins/`, `profiles/`,
Session store, and Generation lineage. It also encouraged Agent-owned files to
appear inside every Workspace.

The App must remain reproducible from one visible Plugin Root, and the resolved
Plan must continue to contain complete storage paths. Moving user-owned files
must not introduce ambient path lookup inside Kernel or individual Plugins.

## Decision

Every Harness surface resolves one global Agent Home before App resolution.
`LENSO_AGENT_HOME` may select an explicit absolute UTF-8 path; otherwise the
Home is `~/.lenso/agent`.

The Agent Home owns:

- `plugins/` and `profiles/` authoring input;
- the generated Host Catalog and durable Generation control state;
- Session, Memory, lifecycle, approval, channel, and authentication state; and
- other Host-owned product files.

The process current directory remains the Workspace. Workspace, Process, Git,
and suggestion Plugins keep their explicit Plan-bound Workspace roots and do
not infer them from the Agent Home.

The Host resolves all of its persistence defaults to absolute Agent Home paths
before producing the immutable Host Catalog and Resolved Plan. Plugins receive
those exact paths through ordinary validated configuration; neither Kernel nor
Plugins read `LENSO_AGENT_HOME` as hidden execution authority.

An explicit resolved Plan retains its encoded paths. Exact replay never
rewrites the Plan to the current Agent Home.

## Consequences

- Changing the shell current directory changes the Workspace without changing
  Agent identity, configuration, Session history, or Generation recovery.
- TUI, CLI, Web, Telegram, and Discord surfaces share one App configuration and
  durable state by default.
- Tests and launchers can select an isolated absolute Home without changing the
  Workspace or process current directory.
- Generic `lenso plugins` and `lenso app` commands still operate on their
  current App directory; operators run them from the Agent Home until the
  generic CLI gains an explicit App-root option.
- Relative paths explicitly authored inside Plugin configuration retain that
  Plugin's documented semantics. Host-owned persistence defaults are absolute.

