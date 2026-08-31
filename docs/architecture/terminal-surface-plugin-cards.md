# Terminal surface Plugin cards

## Terminal Command Provider

- **Owner:** each feature Plugin owns the commands that project its domain
  behavior. `lenso.agent.session-terminal` is the first provider and owns
  `sessions list` plus `sessions show`.
- **Deletion boundary:** removing one provider removes only its command paths.
  The aggregate runtime and every terminal surface remain valid.
- **Provided Capability:** `lenso.terminal.command-provider@1`, with a bounded
  catalog request and cancellable execution stream.
- **Required Capabilities:** only the feature roles needed to perform the
  command. The Session projection requires one `lenso.agent.session@1`.
- **Configuration and state:** command IDs, paths, parameter metadata, and
  supported text/JSON formats are provider-owned. Business state stays behind
  the feature Capability; the command contract creates no second store.
- **Lifecycle:** the provider is queried while the aggregate activates.
  Provider-local and aggregate validators reject malformed or ambiguous
  catalogs before App readiness.
- **Final authorization:** the feature Plugin remains the final authority for
  its operation. Advertising a command grants neither Tool nor OS authority.

## Terminal Command Aggregate

- **Owner:** the `LioRael/lenso-terminal` repository and its
  `lenso-terminal-command-plugin` crate own deterministic validation and
  routing, not any feature command.
- **Deletion boundary:** removing its `commands` Instance removes terminal
  command discovery and execution. Agent Turns, Sessions, and feature
  Capabilities remain valid.
- **Provided Capability:** one validated `lenso.terminal.command@1` catalog and
  execution stream.
- **Required Capability:** many `lenso.terminal.command-provider@1` bindings.
- **Lifecycle and state:** activation snapshots providers in resolved order,
  rejects duplicate IDs, duplicate paths, path-prefix ambiguity, and more than
  256 commands, then stores one immutable route table. Deactivation clears it.
- **Failure boundary:** a provider catalog failure rejects readiness. Execution
  errors and cancellation are forwarded without inventing fallback providers.

## Generic CLI Surface

- **Owner:** the `LioRael/lenso-terminal` repository owns the
  `lenso.terminal.cli` consumer identity and `lenso-terminal-cli-surface`
  catalog-to-Clap translation; each product binary owns argv, stdout/stderr,
  exit codes, and maintenance commands.
- **Deletion boundary:** removing one CLI Instance removes only that command
  presentation. Providers and other CLI, TUI, or Console consumers remain.
- **Required Capability:** one `lenso.terminal.command@1`.
- **Lifecycle and state:** the process leases a catalog and command stream from
  one immutable Generation. Nested help, option parsing, shell quoting, and
  `--json` are derived from the catalog. Execution runs until completion,
  cancellation, or Generation shutdown rather than an arbitrary short timeout.
- **Multiplicity:** an App may select multiple `lenso.terminal.cli` Instances;
  an embedded Host leases the exact `plugin-id/instance-name`. The parser crate
  also accepts a caller-owned binary name, so separate products reuse the same
  contract without sharing process I/O.

## Generic TUI Surface

- **Owner:** `lenso.terminal.tui` owns terminal extension Ports;
  `lenso.agent.tui` owns the Agent Turn consumer; the optional
  `lenso-agent-tui` distribution owns Ratatui, Crossterm, and the event loop.
- **Deletion boundary:** removing the generic TUI consumer removes command,
  panel, and suggestion composition. Removing the Agent TUI consumer removes
  conversation Turns. Neither removal changes feature providers.
- **Required Capabilities:** one `lenso.terminal.command@1`; many
  `lenso.tui.panel@1`; and many `lenso.tui.suggestion@1`.
  `lenso.agent.tui` separately requires one `lenso.agent@3` and optional task
  supervision roles.
- **Configuration and state:** input, transcript rendering, selected panel,
  active streams, and task projection are volatile. A command catalog is kept
  with the exact `TerminalGeneration` lease that produced it. Online switches
  replace both atomically for subsequent commands; an active stream retains
  its old immutable lease until completion or cancellation.
- **Lifecycle:** snapshots are read before raw mode. Drop restores terminal
  state. Built-in Shell controls remain local; feature commands are parsed by
  the shared Clap-backed adapter and stream into the transcript.
- **First observable behavior:** `/sessions list` and `/sessions show ...`
  appear from the same Session provider used by the headless CLI.

## Generic Web Surface

- **Owner:** `lenso.terminal.web` owns only the Web consumer identity. The
  `lenso-agent-web` Host surface owns HTTP validation, SSE framing, volatile
  execution tracking, and cancellation transport.
- **Deletion boundary:** disabling or removing the `web` Instance removes Web
  command discovery and execution while the Agent Web surface, Agent Turns,
  Sessions, providers, CLI, and TUI remain valid.
- **Required Capability:** one `lenso.terminal.command@1`. The consumer neither
  provides commands nor receives ambient OS shell authority.
- **Configuration and state:** the Plugin has no configuration or durable
  state. Catalogs and active executions are Generation-pinned Host state;
  request IDs and cancellation tokens are volatile and bounded to one active
  Agent Turn or Terminal command at a time.
- **Lifecycle:** the Console first negotiates `terminalCommands`, then reads the
  active Generation catalog. Execution uses the shared Clap-backed parser and
  streams typed messages plus one terminal status over SSE. Shutdown and an
  explicit cancel endpoint cancel active commands.
- **First observable behavior:** `/sessions list` is suggested from the live
  catalog and produces streamed output from the same Session provider used by
  CLI and TUI surfaces.

## TUI Panel and Suggestion Providers

- **Owner:** each provider owns one concrete semantic role, not a generic
  `Contribution`. `lenso-agent-tui-static-plugin` owns configured panels;
  workspace and Skills Plugins own their own suggestion snapshots.
- **Deletion boundary:** removing one provider removes only its panels or
  candidates. The composer and terminal command catalog continue working.
- **Provided Capabilities:** `lenso.tui.panel@1` or
  `lenso.tui.suggestion@1`.
- **Configuration and state:** panels are bounded IDs, titles, and read-only
  bodies. Suggestions are bounded semantic command, file, prompt, resource, or
  Skill items. Aggregate duplicate IDs and byte limits are enforced by the
  Host surface.
- **Final authorization:** panels and suggestions carry no actions or ambient
  filesystem authority. Accepting a suggestion only edits the composer; the
  eventual feature or Tool Plugin still authorizes execution.

## Telegram surface Plugin

- **Owner:** `lenso-agent-telegram-plugin` owns the source-derived Telegram
  consumer identity and Agent binding. The `lenso-agent-telegram` Host surface
  owns Telegram Bot API transport and delivery policy.
- **Deletion boundary:** removing the binary, Plugin package, and `telegram`
  Instance removes Telegram polling, chat authorization, reply delivery,
  update cursor, and conversation mapping. Agent, Session, terminal, and
  Plugin behavior remain valid.
- **Required Capability:** one `lenso.agent@3`.
- **Provided Capabilities:** none. Telegram is an external Agent consumer and
  does not invent an application Provider role.
- **Configuration and secrets:** the immutable Plugin configuration is empty.
  The Host surface reads the Bot token from `TELEGRAM_BOT_TOKEN` or an
  explicitly named environment variable, requires an explicit chat allowlist,
  and never writes the token into Plan, state, Session, or diagnostics.
- **Durable facts:** `<agent-home>/telegram/state.json` contains the next Telegram
  update ID and a bounded `bot + chat + topic -> Session ID` mapping. The
  Telegram surface owns the mapping; the Session Plugin creates and owns each
  Session and its events. Missing or corrupt state never falls back silently to
  an in-memory mapping.
- **Lifecycle:** the Host validates `getMe`, long-polls after App readiness,
  processes updates sequentially, leases the current App Generation per
  accepted message, and persists the next cursor only after delivery. Shutdown
  cancels polling through Ctrl-C and drains the App normally.
- **Authorization:** exact chat IDs are required unless `*` is deliberately
  selected. Groups require a mention or reply by default. Telegram Turns expose
  no Tools unless each model-visible Tool name is explicitly allowed; Tool
  Plugins retain final authorization.
- **Delivery:** v1 accepts text and sends bounded plain-text replies split on
  Unicode boundaries. Persist-after-delivery provides at-least-once processing;
  a crash between Telegram accepting a reply and cursor commit can repeat that
  update. Durable exactly-once inbox/outbox semantics remain a later slice.
- **First observable behavior:** two private messages from one allowed chat run
  two Generation-pinned Turns, receive Telegram replies, and resume the same
  durable Session.

## Unified Channel Host

- **Owner:** `lenso-agent-channel` is a Host entrypoint over the independently
  removable Telegram and Discord consumer Plugins. It does not introduce a
  generic Channel Capability, Plugin type, Kernel registry, or mutable graph.
- **Authoring input:** `<agent-home>/channels.toml` selects external transports,
  allowlists, Tool scopes, state paths, and token environment-variable names.
  It is not App composition authority. The Host Catalog and current Plugin Root
  remain the reviewed App source; the resolved Plan is generated Host input.
- **Concurrency:** both transports share one Controller lineage and one Turn
  gate. One Turn is active while a configured, bounded number may wait; a
  message beyond that bound receives a busy response without entering the App.
- **Failure boundary:** invalid configuration, missing token variables, or an
  unavailable selected transport fails the unified Host closed. Each surface
  keeps its own durable cursor, resume data, and conversation mapping.
- **Deletion boundary:** removing this entrypoint and its TOML file removes only
  joint process orchestration. The focused Telegram and Discord binaries and
  their ordinary Plugin deletion boundaries remain intact.

## Discord surface Plugin

- **Owner:** `lenso-agent-discord-plugin` owns the source-derived Discord
  consumer identity and Agent binding. The `lenso-agent-discord` Host surface
  owns Gateway v10 transport, REST replies, and delivery policy.
- **Deletion boundary:** removing the binary, Plugin package, and `discord`
  Instance removes Gateway connections, channel authorization, replies,
  Gateway resume state, and conversation mapping. Agent, Session, Telegram,
  terminal, and Plugin behavior remain valid.
- **Required Capability:** one `lenso.agent@3`.
- **Provided Capabilities:** none. Discord is an external Agent consumer.
- **Configuration and secrets:** immutable Plugin configuration is empty. The
  Host reads `DISCORD_BOT_TOKEN` or an explicitly named environment variable,
  requires a channel allowlist, and keeps the token out of Plan, state,
  Session, and diagnostics.
- **Durable facts:** `<agent-home>/discord/state.json` contains Gateway resume data
  and a bounded `bot + channel -> Session ID` mapping. The Discord surface owns
  that mapping; the Session Plugin creates and owns Session IDs and events.
- **Lifecycle:** after App readiness the Host connects to Gateway v10,
  identifies or resumes, maintains heartbeats, processes message events
  sequentially, leases the current App Generation per accepted message, and
  closes the Gateway during normal shutdown.
- **Authorization:** exact channel IDs are required unless `*` is deliberately
  selected. Guild messages require a Bot mention or reply by default. Reading
  every guild message requires both `--message-content-intent` and the matching
  privileged Intent in Discord. No Tools are exposed unless explicitly named.
- **Delivery:** v1 accepts text and sends plain-text replies split at Discord's
  2,000-character limit with mentions disabled. Gateway Resume can replay
  messages after a disconnect, so delivery is at least once; if the Discord
  Gateway session expires while the process is stopped, events missed during
  that interval cannot be recovered by this surface.
- **First observable behavior:** an allowed DM or mentioned guild message runs
  a Generation-pinned Turn, receives a reply, and later messages in that
  channel resume the same durable Session.
