# Lenso Agent Harness

A terminal-first Agent that starts with a small base App and gains optional
behavior through Plugins.

## Run it

Start the interactive TUI from the repository root:

```sh
cargo run -p lenso-agent-cli --bin lenso-agent
```

Run one headless Turn:

```sh
cargo run -p lenso-agent-cli --bin lenso-agent-cli -- \
  "Summarize this workspace README."
```

Use `--session <id>` to resume a Session, `--no-tools` to remove Tool access
for one Turn, or repeat `--allow-tool <name>` to narrow the selected Tools.

## Choose Plugins

The base App is read-only. List the Plugins shipped with this Host and enable
only the behavior you need:

```sh
cargo run -p lenso-agent-cli -- plugins list
cargo run -p lenso-agent-cli -- plugins enable workspace-edit
cargo run -p lenso-agent-cli -- plugins status

cargo run -p lenso-agent-cli -- \
  "Create and edit a workspace note."

cargo run -p lenso-agent-cli -- plugins disable workspace-edit
```

Normal Plugin management has six commands:

```text
list
add <bundle>
status
enable <plugin-id>
disable <plugin-id>
remove <plugin-id>
```

Adding a newer Bundle with the same Plugin ID updates it. Disable keeps the
selected Release available for re-enabling; remove forgets it from this App.

## Build a Plugin

Create one Rust/Wasm Tool Plugin without authoring a separate internal unit or
Manifest template:

```sh
lenso plugin new uppercase
cd uppercase
lenso plugin check
lenso plugin dev --operation execute \
  --request-json '{"name":"uppercase","arguments_json":"{\"text\":\"hello\"}"}'
lenso plugin pack
```

Add the package to the Harness:

```sh
cargo run -p lenso-agent-cli -- plugins add \
  path/to/uppercase/dist/uppercase-0.1.0.lenso-plugin
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
cargo run -p lenso-agent-cli --bin lenso-agent-channel
```

Delete either `[telegram]` or `[discord]` from the file to run only one
transport. The shared Host runs one Agent Turn at a time and bounds pending
work across channels.

## Documentation

- [Documentation map](docs/README.md)
- [Plugin tutorial](docs/tutorials/10-minute-tool-provider.md)
- [Control-plane glossary](docs/glossary.md)
- [Host implementation and operational reference](docs/architecture/host-internals.md)

The README intentionally presents only the normal App and Plugin workflow.
Runtime lowering, immutable execution, recovery, and authority mechanics are
maintainer concerns documented behind the last link.
