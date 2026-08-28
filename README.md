# Lenso Agent Harness

A terminal-first Agent that starts with a small base App and gains optional
behavior through Plugins.

## Run it

Authenticate once before the first Turn. The default App uses the direct Codex
Model with the `default` ChatGPT profile:

```sh
cargo run -p lenso-agent-cli -- auth login
```

Start the interactive TUI from the repository root:

```sh
cargo run -p lenso-agent-tui
```

Run one headless Turn:

```sh
cargo run -p lenso-agent-cli -- \
  "Summarize this workspace README."
```

Use `--session <id>` to resume a Session, `--no-tools` to remove Tool access
for one Turn, or repeat `--allow-tool <name>` to narrow the selected Tools.

Type `/` in the TUI to search both reviewed commands and the Skills currently
available below `~/.agents/skills`. Selecting `/skill-name` leaves the composer
open so the request can follow it on the same line.

The default Tool catalog also includes `ask_user`. In the TUI, an Agent can
pause a Turn, show one bounded question with optional choices, and resume after
the answer is entered in the normal composer. Headless and channel surfaces do
not pretend to be interactive: the same Tool returns
`interaction_unavailable` immediately unless that surface supplies a User
Interaction Adapter.

## Choose a Session Profile

A Profile selects an exact subset of configured Plugin Instances for one Agent
Session. It does not contain Plugin configuration or introduce another App
manifest. Keep every configuration beside its Plugin:

```text
plugins/
  example.code-tools/
    code.toml
  example.game-loop/
    game.toml
  lenso.agent.model.openai-compatible/
    game.toml
profiles/
  code.toml
  game.toml
```

For example, `profiles/game.toml` can select a different Agent Loop, Tool set,
and configured Model Instance:

```toml
description = "Game agent"
agent = "example.game-loop/game"
instances = [
  "example.game-loop/game",
  "example.game-tools/default",
  "lenso.agent.model.openai-compatible/game",
]
```

Start or resume a Session through that Profile:

```sh
lenso-agent --profile game
lenso-agent --profile game --session <id>
lenso-agent-cli --profile code "Review this workspace."
```

The Profile is an authoring-time selector. The resolved immutable Generation,
including exact Plugin configurations and bindings, remains the execution and
Session-provenance authority. Editing the selected Profile or its Plugin files
goes through the same online Ready Gate as any other Plugin change.

For a transactional local Session store, configure the SQLite Adapter through
the same Plugin directory:

```toml
# plugins/lenso.agent.session.sqlite/local.toml
database = ".lenso/sessions.sqlite3"
```

Then select it from a Profile; it replaces the default file Session slot:

```toml
# profiles/sqlite.toml
description = "SQLite-backed sessions"
instances = ["lenso.agent.session.sqlite/local"]
```

Inspect its Generation provenance with
`lenso-agent-cli sessions provenance --session <id> --database .lenso/sessions.sqlite3`.

Long Sessions use the replaceable Context Compaction seam instead of silently
dropping old history. The bundled offline Adapter stores a bounded extractive
summary plus complete recent turns while leaving the canonical Session log
untouched. Configure its default Instance like any other Plugin:

```toml
# plugins/lenso.agent.context-compaction/context-compactor.toml
max_input_characters = 1048576
max_summary_characters = 8192
retain_recent_turns = 8
```

A Profile can select a different native, Wasm, process, or remote Plugin for
`lenso.agent.context-compaction@1`; the Agent Loop still owns trigger policy,
checkpoint validation, and durable Session facts.

Lifecycle integrations use ordinary Plugin configuration. The default local
audit Adapter writes typed `session_started`, `session_resumed`, and
`turn_started` events to `.lenso/lifecycle/events.jsonl`. A trusted command
Adapter can be added without changing the Agent Loop:

```toml
# plugins/lenso.agent.lifecycle.command/webhook.toml
program = "/absolute/path/to/lifecycle-handler"
arguments = ["--format", "json"]
timeout_ms = 5000
```

It receives one event as JSON on stdin. Program output is discarded and a
timeout or non-zero exit rejects the transition.

## Choose Plugins

The Host boots its read-only defaults when `plugins/` is absent or empty. App
differences use one directory per Plugin and one TOML file per Instance:

```sh
lenso plugins list
lenso plugins configure lenso.agent.workspace-edit

cargo run -p lenso-agent-cli -- \
  "Create and edit a workspace note."

lenso plugins disable lenso.agent.workspace-edit
lenso plugins enable lenso.agent.workspace-edit
```

The visible state is ordinary files:

```text
plugins/
  lenso.agent.workspace-edit/
    default.toml
    default.disabled        # present only while disabled
  example.uppercase/
    plugin.lenso-plugin/    # immutable package, for external Plugins
    default.toml
    default/                # optional, immutable Instance resources
      prompts/system.md
```

The optional `<instance>/` directory is paired with `<instance>.toml`. The Host
snapshots its bounded regular files into the same immutable Generation, so a
Plugin reads stable bytes rather than a live filesystem path. A resource-only
edit goes through the same Ready Gate and existing Turns retain the old bytes.

There is no `lenso.app.json`, `lenso.app.toml`, `lenso.local.toml`, enabled
list, or user-authored binding document. `lenso app check` and `lenso app show`
derive the App from the Host Catalog plus this directory.

## Build a Plugin

Create one ordinary Rust Tool Plugin. The default project builds portable Wasm
and trusted Process implementations from the same authored source:

```sh
lenso plugin new uppercase
cd uppercase
lenso plugin check
lenso plugin dev --operation execute \
  --request-json '{"name":"uppercase","arguments_json":"{\"text\":\"hello\"}"}'
lenso plugin pack
```

Add the package to the Harness App:

```sh
lenso plugins add path/to/uppercase/dist/uppercase-0.1.0.lenso-plugin
lenso plugins configure uppercase default
```

`pack` checks the exact bytes it writes and the Harness checks received bytes
again. There is no separate `plugin verify` step. See the
[10-minute Tool Plugin tutorial](docs/tutorials/10-minute-tool-provider.md).

## Embed the Harness

An application declares only the Plugins compiled into its Host Build, its
process-owned surface, and the Profile to run:

```rust
let host = AgentHost::builder()
    .plugins(lenso_agent_default_plugins::link)
    .surface(TuiSurface::terminal())
    .build()?;

let mut app = host.run(Profile::named("code")).await?;
// The TUI owns its event loop; `app` supplies Generation-pinned Agent Turns.
app.shutdown().await?;
```

A headless binary swaps only the surface:

```rust
let host = AgentHost::builder()
    .plugins(lenso_agent_default_plugins::link)
    .surface(HeadlessSurface::stdio())
    .build()?;

let mut app = host.run(Profile::Default).await?;
```

`lenso::host::HostBuilder` is the lower framework seam that owns durable
Generation recovery, Controller execution, fenced routes, and shutdown. Agent
Profiles, Turns, sessions, and TUI or channel loops remain in this Harness.

## Run chat channels

Copy the reviewed configuration, select exact chat or channel allowlists, and
keep tokens in environment variables:

```sh
cp lenso.channels.example.toml lenso.channels.toml
export TELEGRAM_BOT_TOKEN='<bot-token>'
export DISCORD_BOT_TOKEN='<bot-token>'
cargo run -p lenso-agent-channel
```

Delete either `[telegram]` or `[discord]` from the file to run only one
transport. The shared Host runs one Agent Turn at a time and bounds pending
work across channels.

The distributions are independent. Installing `lenso-agent-cli` does not
compile or install Ratatui, Telegram, or Discord support; install
`lenso-agent-tui` or `lenso-agent-channel` only when those surfaces are
needed. Each executable links only its own surface Plugin Catalog.

## Documentation

- [Documentation map](docs/README.md)
- [Plugin tutorial](docs/tutorials/10-minute-tool-provider.md)
- [Control-plane glossary](docs/glossary.md)
- [Host implementation and operational reference](docs/architecture/host-internals.md)

The README intentionally presents only the normal App and Plugin workflow.
Runtime lowering, immutable execution, recovery, and authority mechanics are
maintainer concerns documented behind the last link.
