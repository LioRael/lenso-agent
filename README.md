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

Start the Console Web surface with a durable, administrator-controlled Tool
allowlist:

```sh
LENSO_AGENT_CONTROL_TOKEN=replace-with-a-local-control-token \
  cargo run -p lenso-agent-web -- \
  --listen 127.0.0.1:8788 \
  --tool-policy .lenso/console-agent-tool-policy.json
```

The control route accepts only the matching bearer token. Updates validate
against the active Plan-bound Tool catalog, use an expected revision, persist
before activation, and affect only Turns admitted after the update.

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

For a model service or remote gateway, select the bundled command Adapter:

```toml
# plugins/lenso.agent.context-compaction.command/semantic.toml
program = "/absolute/path/to/compaction-gateway"
arguments = []
timeout_ms = 30000
max_response_bytes = 1048576
```

Cross-Session Memory uses a separate replaceable seam. The default offline
Adapter stores bounded, provenance-bearing memories in SQLite and recalls them
with FTS5. Configure its Instance under the standard Plugin directory:

```toml
# plugins/lenso.agent.memory.sqlite/memory.toml
database = ".lenso/memory/code.sqlite3"
scope = "code"
max_records = 10000
max_item_characters = 16384
max_recall_items = 8
max_recall_characters = 16384
```

A `game` Profile may select another `lenso.agent.memory.sqlite` Instance with
a different database or scope, or replace it with a remote Adapter for
`lenso.agent.memory@1`. The same Plugin code therefore supports isolated
per-Profile policy without a central App file. Recalled text is always
lower-authority request context; it never edits the Session's System
Instruction.

The bundled remote-friendly Adapter uses the same configuration shape:

```toml
# plugins/lenso.agent.memory.command/team-memory.toml
program = "/absolute/path/to/memory-gateway"
arguments = ["--endpoint", "https://memory.example.test"]
timeout_ms = 10000
max_response_bytes = 1048576
```

Both command Adapters receive one
`lenso.agent.command-adapter@1` JSON request on stdin and must return exactly
one JSON response on stdout. The executable can bridge HTTP or MCP; the
Harness never interprets a shell command or embeds transport credentials in
the Plugin protocol.

Secrets use the same Profile and Plugin-directory model. The distributed Host
links four interchangeable Providers for `lenso.secrets@1`: environment
variables, macOS Keychain, an age-encrypted local file, and a bounded command
resolver for 1Password or another remote Secret Manager CLI. Provider
configuration is never hidden in the Host Catalog.

For example, two Profiles can use the same Keychain Plugin code with isolated
service and account mappings:

```toml
# plugins/lenso.secrets.keychain/code.toml
service = "com.lenso.agent.code"

[references]
"model/openai-api-key" = "openai-api-key"
```

```toml
# plugins/lenso.secrets.keychain/game.toml
service = "com.lenso.agent.game"

[references]
"model/openai-api-key" = "openai-api-key"
```

```toml
# profiles/code.toml
description = "Code agent with macOS Keychain credentials"
instances = [
  "lenso.agent.model.openai-compatible/code",
  "lenso.secrets.keychain/code",
]
```

The Profile selects only the Instance identity; the TOML beside the Plugin
remains the sole configuration authority. Selecting a Provider verifies every
configured source before the Generation becomes ready. Values stay in the
Provider and never enter Profile files, Plans, diagnostics, or Session facts.
See the
[Secrets Plugins repository](https://github.com/LioRael/lenso-secrets-plugin)
for encrypted-file and remote resolver configuration.

Git support is an opt-in semantic Tool Plugin rather than unrestricted command
access. Configure it beside the Process provider that authorizes `git`:

```toml
# plugins/lenso.agent.process.native/default.toml
root = "."
allowed_programs = ["git"]
environment_allowlist = ["PATH", "HOME", "TMPDIR", "LANG", "LC_ALL"]
max_timeout_ms = 600000
max_output_bytes = 262144
max_argument_bytes = 131072
```

```toml
# plugins/lenso.agent.git-tools/default.toml
default_timeout_ms = 30000
max_log_entries = 50
max_commit_message_bytes = 4096
enable_branch_management = true
enable_history_integration = false
allowed_network_remotes = ["origin"]
```

Then select both Instances for the coding experience:

```toml
# profiles/code.toml
description = "Code agent with bounded Git tools"
instances = [
  "lenso.agent.process.native/default",
  "lenso.agent.git-tools/default",
]
```

The minimal configuration adds `git_status`, `git_diff`, `git_log`,
`git_stage`, and `git_commit`. `enable_branch_management` additionally exposes
bounded branch list/create/switch operations. `enable_history_integration`
adds non-interactive merge and rebase. A non-empty `allowed_network_remotes`
adds fetch and non-force push only for those exact remote names.
Staging requires explicit repository-relative paths; commit includes only
already staged changes and intentionally disables hooks and signing. No reset,
branch deletion, force push, arbitrary refspec, interactive rebase, or remote URL
is exposed. Keep history integration and network remotes disabled in ordinary
profiles. Add an Approval Hook to the same Profile so every mutation uses
approve-then-retry.

MCP servers are opt-in Plugin Instances too. The bundled MCP Client supports
stdio and MCP 2026-07-28 Streamable HTTP, plus protocol negotiation, Tool
discovery, namespacing, cancellation, restart, and cleanup. Stdio supports modern per-request
metadata and legacy `initialize` servers; `auto` probes a disposable process
before opening the real session.

```toml
# plugins/lenso.agent.mcp-client/filesystem.toml
transport = "stdio"
program = "/absolute/path/to/node"
arguments = ["/absolute/path/to/mcp-filesystem-server", "/workspace"]
working_directory = "/workspace"
environment_allowlist = ["PATH", "HOME"]
protocol = "auto"
tool_namespace = "filesystem"
startup_timeout_ms = 5000
request_timeout_ms = 30000
```

Remote MCP uses the same Plugin and Slot:

```toml
# plugins/lenso.agent.mcp-client/team.toml
transport = "streamable_http"
endpoint = "https://mcp.example.test/mcp"
authorization_environment = "MCP_AUTHORIZATION"
protocol = "modern"
tool_namespace = "team"
startup_timeout_ms = 5000
request_timeout_ms = 30000
```

`MCP_AUTHORIZATION` contains the complete Authorization header value and is
resolved only into the running Plugin. HTTP requests send the required
protocol/method/name headers, support JSON or request-scoped SSE responses,
and mirror valid `x-mcp-header` Tool parameters. The Tool catalog is refreshed
at each new Turn; the current Turn keeps its already admitted immutable set.
Prompt and Resource metadata use the separate `lenso.agent.context-source@1`
contract instead of becoming model-controlled Tools.

Select it only for the Profile that needs those Tools:

```toml
# profiles/code.toml
description = "Code agent with filesystem MCP tools"
instances = ["lenso.agent.mcp-client/filesystem"]
```

Remote names are normalized to lowercase snake case and exposed as
`mcp__filesystem__<tool_name>`; normalization collisions fail readiness. They
are treated as exclusive because MCP does not provide a portable side-effect
or concurrency classification. The process runs as trusted native code with a cleared
environment plus the explicit allowlist; it is not a sandbox. Removing the
Instance removes the process and every projected Tool or Context Source.

The CLI can explicitly render one user-selected Prompt and attach one or more
application-selected Resources before opening the Turn:

```sh
lenso-agent-cli contexts --profile code

lenso-agent-cli \
  --profile code \
  --context-prompt filesystem/review \
  --context-arguments '{"focus":"safety"}' \
  --context-resource 'filesystem=file:///workspace/README.md' \
  "Review this project."
```

The TUI adds no-argument MCP Prompts and text Resources to `/` completion as
`/prompt:<source>/<name>` and `/resource:<source>/<name>`. Selecting one leaves
the composer open for the task. Required-argument Prompts remain available
through the CLI, where arguments are explicit JSON. Version 1 rejects binary
MCP content rather than dropping it. Elicitation and Sampling remain request
continuations over User Interaction and Model—not Context Sources or Tools.

Lifecycle integrations use ordinary Plugin configuration. The default local
audit Adapter writes typed Session, Turn-start, and terminal Turn events to
`.lenso/lifecycle/events.jsonl`. A trusted command
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
