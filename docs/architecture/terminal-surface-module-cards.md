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
  Help panel; removing it resolves a valid App with no panel binding.

## Telegram surface Module

- **Owner:** `lenso-agent-telegram-module` owns the source-derived Telegram
  consumer identity and Agent binding. The `lenso-agent-telegram` Host surface
  owns Telegram Bot API transport and delivery policy.
- **Deletion boundary:** removing the binary, Module package, and `telegram`
  Instance removes Telegram polling, chat authorization, reply delivery,
  update cursor, and conversation mapping. Agent, Session, terminal, and
  Plugin behavior remain valid.
- **Required Capability:** one `lenso.agent@1`.
- **Provided Capabilities:** none. Telegram is an external Agent consumer and
  does not invent an application Provider role.
- **Configuration and secrets:** the immutable Module configuration is empty.
  The Host surface reads the Bot token from `TELEGRAM_BOT_TOKEN` or an
  explicitly named environment variable, requires an explicit chat allowlist,
  and never writes the token into Plan, state, Session, or diagnostics.
- **Durable facts:** `.lenso/telegram/state.json` contains the next Telegram
  update ID and a bounded `bot + chat + topic -> Session ID` mapping. The
  Telegram surface owns the mapping; the Session Module creates and owns each
  Session and its events. Missing or corrupt state never falls back silently to
  an in-memory mapping.
- **Lifecycle:** the Host validates `getMe`, long-polls after App readiness,
  processes updates sequentially, leases the current App Generation per
  accepted message, and persists the next cursor only after delivery. Shutdown
  cancels polling through Ctrl-C and drains the App normally.
- **Authorization:** exact chat IDs are required unless `*` is deliberately
  selected. Groups require a mention or reply by default. Telegram Turns expose
  no Tools unless each model-visible Tool name is explicitly allowed; Tool
  Modules retain final authorization.
- **Delivery:** v1 accepts text and sends bounded plain-text replies split on
  Unicode boundaries. Persist-after-delivery provides at-least-once processing;
  a crash between Telegram accepting a reply and cursor commit can repeat that
  update. Durable exactly-once inbox/outbox semantics remain a later slice.
- **First observable behavior:** two private messages from one allowed chat run
  two Generation-pinned Turns, receive Telegram replies, and resume the same
  durable Session.

## Discord surface Module

- **Owner:** `lenso-agent-discord-module` owns the source-derived Discord
  consumer identity and Agent binding. The `lenso-agent-discord` Host surface
  owns Gateway v10 transport, REST replies, and delivery policy.
- **Deletion boundary:** removing the binary, Module package, and `discord`
  Instance removes Gateway connections, channel authorization, replies,
  Gateway resume state, and conversation mapping. Agent, Session, Telegram,
  terminal, and Plugin behavior remain valid.
- **Required Capability:** one `lenso.agent@1`.
- **Provided Capabilities:** none. Discord is an external Agent consumer.
- **Configuration and secrets:** immutable Module configuration is empty. The
  Host reads `DISCORD_BOT_TOKEN` or an explicitly named environment variable,
  requires a channel allowlist, and keeps the token out of Plan, state,
  Session, and diagnostics.
- **Durable facts:** `.lenso/discord/state.json` contains Gateway resume data
  and a bounded `bot + channel -> Session ID` mapping. The Discord surface owns
  that mapping; the Session Module creates and owns Session IDs and events.
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
