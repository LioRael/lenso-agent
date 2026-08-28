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
