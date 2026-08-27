# Agent Harness host internals

A terminal-first Agent Harness for running a model with an explicitly selected
set of Tools, Prompt instructions, durable Sessions, and composed UI panels.

## Try it

Open the deterministic, read-only TUI from the repository root:

```sh
cargo run -p lenso-agent-cli --bin lenso-agent
```

`lenso-agent` has no subcommands. Running it directly enters the TUI; use
`--session`, repeated `--allow-tool`, `--no-tools`, or the advanced `--plan`
flag to narrow that interactive session. Enter submits a Turn, Esc cancels an
active Turn or exits while idle, and Tab cycles panels contributed by selected
Modules. The conversation supports mouse, Page Up/Page Down, and Ctrl-U/Ctrl-D
scrolling without losing the draft; End returns to live tail-following. The
composer supports cursor editing, multiline input, and in-process prompt
history. Press Ctrl-. to open the context-sensitive keyboard reference.

The existing companion binary remains available for headless automation and
Host maintenance:

```sh
cargo run -p lenso-agent-cli --bin lenso-agent-cli -- \
  "Summarize this workspace README."
```

Run Telegram and Discord together through one Channel Host. Copy the reviewed
example, replace its allowlist placeholders, export the tokens for the Channels
you selected, and start the Host directly:

```sh
cp lenso.channels.example.toml lenso.channels.toml
export TELEGRAM_BOT_TOKEN='<bot-token>'
export DISCORD_BOT_TOKEN='<bot-token>'
cargo run -p lenso-agent-cli --bin lenso-agent-channel
```

Delete either `[telegram]` or `[discord]` from `lenso.channels.toml` to run only
one Channel. The file contains policy and environment-variable names, never
token values. One Agent Turn runs at a time across every Channel; the bounded
shared queue prevents either transport from creating unlimited pending work.
The Host resolves the root App automatically, so ordinary use does not require
creating or passing a Plan file.

The focused binaries remain useful for debugging one transport. For Telegram,
create a Bot with BotFather, export its token, and explicitly select the chats
that may invoke it:

```sh
export TELEGRAM_BOT_TOKEN='<bot-token>'
cargo run -p lenso-agent-cli --bin lenso-agent-telegram -- \
  --allow-chat '<telegram-chat-id>'
```

Use `--allow-chat '*'` only as an intentional initial test setting, then
replace it with exact private or group chat IDs. Telegram Turns have no Tools
by default; repeat `--allow-tool <name>` to expose only reviewed Tools. Private
chat text is accepted directly. Group and supergroup text requires an
`@bot_username` mention or a reply to the Bot unless
`--respond-all-groups` is explicitly selected. The surface long-polls without
a public webhook, stores only its update cursor and
conversation-to-Session mapping in `.lenso/telegram/state.json`, and obtains a
fresh App Generation lease for every message. Bot tokens remain in the
environment and never enter the Plan or Session.

For Discord, create an application and Bot in the Discord Developer Portal,
invite it with permission to view channels, read message history, and send
messages, then select the channels that may invoke it:

```sh
export DISCORD_BOT_TOKEN='<bot-token>'
cargo run -p lenso-agent-cli --bin lenso-agent-discord -- \
  --allow-channel '<discord-channel-id>'
```

Direct messages are accepted from allowed channels. Guild messages require an
`@mention` or reply to the Bot by default, which avoids requesting Discord's
privileged Message Content Intent. `--respond-all-guilds` requires the explicit
`--message-content-intent` switch and the matching Developer Portal setting.
Discord Turns expose no Tools unless repeated `--allow-tool <name>` options
select them. Gateway resume information and channel-to-Session mappings live
in `.lenso/discord/state.json`; the Bot token stays in the environment.

The base App in `lenso.app.json` uses a deterministic fixture Model and can only
read the current workspace. Enable workspace mutation without creating or
selecting another App definition:

```sh
cargo run -p lenso-agent-cli --bin lenso-agent-cli -- plugins enable workspace-edit
cargo run -p lenso-agent-cli --bin lenso-agent-cli -- \
  "Create and edit a workspace note."
```

Use `--session <id>` to resume a durable Session, `--no-tools` to remove Tool
access for one Turn, or repeated `--allow-tool <name>` options to narrow the
selected App's Tool set.

To build and run a removable Tool Provider from source, follow the
[10-minute Tool Provider tutorial](docs/tutorials/10-minute-tool-provider.md).
The [documentation map](docs/README.md) separates tutorials, operational
how-to guides, reference material, and architecture explanations; the
[glossary](docs/glossary.md) gives each control-plane term one stable meaning.

## Choose capabilities

Start from the default read-only App, then enable independently shipped Plugin
contributions by name. Personal selections are stored as one sorted, versioned
enabled list in the visible, Git-ignored `lenso.local.toml` beside
`lenso.app.json`; there is no Composition file or private Plugin database for
each combination. The file is created only after the first selection and is
removed when the selection becomes empty:

```sh
# See the exact Releases bundled with this Host.
cargo run -p lenso-agent-cli -- plugins list

# Low-risk append-to-many Tool Provider: automatic local admission.
cargo run -p lenso-agent-cli -- plugins enable text-tools

# The versioned Host profile fixes the maximum workspace mutation authority.
cargo run -p lenso-agent-cli -- plugins enable workspace-edit

# Add local Skills to both the Prompt and Tool aggregates.
cargo run -p lenso-agent-cli -- plugins enable skills

# Add the reviewed process catalog (cargo, git, and rg).
cargo run -p lenso-agent-cli -- plugins enable local-process

cargo run -p lenso-agent-cli -- plugins status
cargo run -p lenso-agent-cli -- \
  "Create and edit a workspace note."

cargo run -p lenso-agent-cli -- plugins disable workspace-edit
```

The catalog also includes `openai-compatible` and experimental `codex-direct`
Model replacements. `workspace-edit`, `text-tools`, `skills`, and
`local-process` become independently selected Module Instances. Startup resolves
the enabled IDs through exact Host-built Profiles into a new immutable App
Generation; Manifest, Receipt, lock, and Plan authority exist only in memory.
The running Kernel never discovers packages or mutates its graph.

## How Apps are composed

The repository has one source App Definition: `lenso.app.json`. It describes the
small read-only base. Optional Modules are selected through the persisted Plugin
enabled list in the visible, Git-ignored `lenso.local.toml`, so combinations do
not create more App files or modify reviewed source.

Stable conservative Module defaults are locked into package-owned Descriptors.
The App Definition records only product behavior, authority-bearing values, and
overrides. Stateless Modules need no configuration file. The intended static
Module authoring shape is one reviewed `config/modules/<instance>.toml` file
referenced by `configuration_file` in the App Definition; that authoring-layer
support is tracked separately. Plan resolution still materializes and validates
one complete configuration for every selected Module before boot.

Before boot, the Host validates the definition and selected Plugins, then
resolves one immutable App Plan in memory for the candidate Generation. App
authors can validate the source intent without materializing a generated file:

```sh
lenso app check --definition lenso.app.json
```

`--plan <path>` remains an advanced escape hatch for exact Plan replay.
Resolved Plans are runtime values and are never hand-edited or committed.
`scripts/check-removal.sh` proves static optional providers can be removed while
the remaining graph still resolves; Plugin tests prove independent Skills,
process, and workspace-edit removal. The TUI proof additionally removes every
panel Contribution while retaining the TUI Shell and Agent route.

## Compose the TUI

The root App selects a `tui` Shell Module that requires exactly one
`lenso.agent@3` provider and `many lenso.agent.tui-contribution@1` providers.
The Shell owns terminal mode, layout, focus, input, streaming, cancellation,
and cross-provider panel ID collision checks. Contribution Modules return
bounded semantic panel snapshots; they do not receive `ratatui` widgets or a
global registry.

`lenso.app.json` includes one removable `tui-help` static
Contribution. Another Module can contribute a panel by providing the same
Capability and being explicitly selected in App Composition. Removing all
Contribution providers leaves the Shell valid with only the conversation.

## Runtime baseline

The host currently resolves `lenso-app-plan 0.2.0` from reviewed core revision
`d25a785577354dbc942fa792fac3baff95c58515` and `lenso-kernel 0.1.12`. All
runtime crates are locked to one reviewed `lenso-runtime-rust` commit,
`f2e506de04ee5286251d0c9cabd9cf56ccacefe4`; contract authoring and codegen are
locked to `lenso-protocols` revision
`8a9b2482278224973417aaac1fd925ba1cfa5370`. App Definition extensions are
locked to `lenso-cli` revision
`68c361ac484ae340d48389d3ed163ac269bbf679`. This closes the generic dynamic
Plugin control plane and preview Wasm Component, QuickJS, and native-dylib
Execution Adapters alongside the existing native host runtime, and preserves
declared request/stream operation kinds when Plugin Manifests become Plans.

The normal source-backed path expands bundled enabled IDs through the exact
Host build, resolves the base Plan and Plugin contributions in memory, and
stages an immutable Generation behind the existing Ready Gate. Bundled-only
selection creates no `.lenso/plugins` Store, Active Set, lock, Receipt, Plan,
or Generation record. When a user installs a third-party Bundle, the Host
retains that Release's immutable Store authority and merges it with the local
bundled selection before resolving the same candidate Generation.
`plugins status` prints the Plugin folder and a concise list of usable or
problematic Plugins. `plugins status --verbose` adds the exact local
configuration path and control-plane diagnostics. Module graph,
binding, product behavior, and authority-bearing values remain in the reviewed
`lenso.app.json`; stable implementation defaults come from locked Module
Descriptors, and `lenso.local.toml` is not an arbitrary Module configuration
overlay.
Each Agent Turn still holds a Generation lease until its stream reaches a
terminal outcome, and the Kernel still receives only one immutable Plan.

The Host can admit passive Plugin releases plus executable profiles
registered in its product-owned Plugin Profile Catalog. Each code-level Catalog
entry closes the exact package, implementation authority, entrypoint, configuration Schema,
Capability Descriptor and Operations, operation kinds, execution class, target,
support/trust policy, canonical configuration, and one bounded attachment rule.
The Catalog admits the linked `lenso.agent.text-tools@0.2.0` factory as a
stateless, permission-free append-to-`many` Tool Provider. It also admits
reviewed workspace-edit, Skills, local-process, and Model replacement Profiles.
One
package-independent, isolated Wasm Tool Provider shape with the same exact
Capability and attachment, empty configuration, no Host imports, permissions,
state, Data mounts, or binding templates, and mandatory review evidence. It
also admits one reviewed variant with exactly one generated
`lenso.agent.workspace-read@1/read_text` Host import. The Host Profile fixes
that binding to the dedicated base `workspace-import-read` Instance; the
Bundle cannot select a provider or request workspace write, process, Secrets,
state, or Data mount authority. A separate reviewed Wasm Tool shape imports
exactly `lenso.agent.http-fetch@1/get` and requests one canonical `network`
Permission scope. The reviewed origin set becomes an immutable approved grant,
must be contained by the App-selected HTTP Provider allowlist, and is enforced
again for every request. Redirects, credentials, non-UTF-8 responses, and
oversized bodies fail closed. It
admits one restricted fixture Model profile that replaces the base Plan's exact
`model` provider for the `agent` consumer. The
experimental Codex Direct profile admits one atomic Model/Auth pair, its exact intra-Plugin
binding, and the coupled Agent model configuration. Experimental Artifact
profiles additionally allow a reviewed QuickJS or Wasm Component Module to
replace the exact native Agent Loop through the generated `AgentJsonCodec`.
Other Data mounts, Permission shapes, arbitrary binding templates, extra
Capability requirements, and incomplete Feature selections fail
admission. General provider/configuration selection, state-changing overlap,
automatic rollback, distributed coordination, Generation deletion, Plugin
Store garbage collection, native-dylib product acceptance, and general
third-party Host Capability permissions remain deferred.

## Manage Plugins

The normal Harness workflow has one user-facing unit: the Plugin. A Plugin project is created and
packed with the `lenso plugin` authoring commands, then managed by the Harness with six commands:

```sh
cargo run -p lenso-agent-cli -- plugins list
cargo run -p lenso-agent-cli -- plugins add ./dist/my-plugin
cargo run -p lenso-agent-cli -- plugins status
cargo run -p lenso-agent-cli -- plugins disable dev.example.my-plugin
cargo run -p lenso-agent-cli -- plugins enable dev.example.my-plugin
cargo run -p lenso-agent-cli -- plugins remove dev.example.my-plugin
```

Run `plugins add` again with a newer Release of the same Plugin ID to update it. The Harness
validates and stages the candidate, waits for Ready, switches new work, and lets existing work
drain. `disable` keeps the selected Release so it can be enabled again; `remove` forgets it.
Ordinary output contains only Plugin IDs, versions, and status. Runtime coordination stays private.

New external Plugins use the bounded Rust/Wasm shape produced by `lenso plugin new`: one Component,
one `lenso.agent.tool-provider@2` entry, and no dependencies, permissions, state, mounts, features,
replacement behavior, or publisher-selected bindings. See
`examples/external-plugins/wasm-text-tools` for the complete `new → check → dev → pack → add`
workflow.

### Safety boundary

A Plugin command never patches a running process in place. The Harness verifies immutable Bundle
bytes, derives the bounded execution attachment privately, stages the candidate, and publishes the
new selection only after Ready succeeds. Existing work drains on its previous selection. Startup
validates the same stored authority before use. These mechanics are absent from the normal Plugin
CLI and are not concepts Plugin authors must supply.

Broader executable shapes, Host imports, permissions, state, provider replacement, and arbitrary
bindings remain unsupported by the public Plugin workflow.

## Run the deterministic slice

From the repository root:

```sh
cargo run -p lenso-agent-cli -- \
  "Summarize this workspace README."
```

The CLI writes the generated Session ID to stderr. Resume the durable Session
after a process restart with `--session <id>`. The Agent Loop streams text as
the selected Model produces it, supports direct answers and bounded sequential
Tool calls, and rebuilds a bounded completed-turn history for resumed Sessions.
If a Host disappears after `turn_started`, the next resume atomically records a
`turn_failed` event with `host_interrupted` before starting new work. A caller
may also narrow one Turn with repeated `--allow-tool <name>` or `--no-tools`;
the Agent Loop rejects names outside the Tool catalog bound by the immutable
Plan, so the Turn-local scope can only remove authority.
Every Turn records the leased `generation_spec_digest`; changing the active
Plugin Set before resuming produces a new digest while preserving the earlier
content-addressed Generation Spec and Session events.

## Compose Prompt and Skill plugins

Prompt and Skill providers are ordinary Modules selected before boot. The root
base definition binds the removable `summary-skill` provider to the `prompt`
aggregate. Additional providers are Model-visible in their resolved binding
order.

Each static plugin Instance declares one or more versioned contributions in
the project document:

```json
{
  "key": "rust-review",
  "package": "lenso.agent.prompt.static",
  "configuration": {
    "contributions": [
      {
        "id": "review.rust",
        "version": "1.0.0",
        "kind": "skill",
        "content": "Review Rust changes for correctness and explicit failure handling."
      }
    ]
  }
}
```

The App Composition must also explicitly bind that Instance to the `prompt`
consumer through `lenso.agent.prompt-provider@1`, then be checked and resolved
again. The running Kernel never discovers or hot-loads Prompt plugins. Session
events retain contribution IDs, versions, kinds, and content digests for audit.

### Load selected Skills from `~/.agents`

`lenso.agent.prompt.filesystem` can snapshot explicitly named
`~/.agents/skills/<name>/SKILL.md` files during App startup:

```json
{
  "key": "agents-skills",
  "package": "lenso.agent.prompt.filesystem",
  "configuration": {
    "root": "~/.agents/skills",
    "skills": ["lenso-module-authoring", "lenso-app-composition"],
    "id_prefix": "agents.skills",
    "max_file_bytes": 65536,
    "max_total_bytes": 131072
  },
  "configuration_schema": "crates/lenso-agent-prompt-filesystem-module/config.schema.json",
  "provides": [
    {
      "capability_id": "lenso.agent.prompt-provider@1",
      "descriptor_version": "1.0.0",
      "operations": ["contribute"]
    }
  ],
  "execution_class": "lenso.native-rust@1"
}
```

Add the ordinary Cargo package input and an explicit `prompt` consumer binding,
then check and resolve the project again. The Module does not enumerate
unselected directories, execute referenced scripts, follow a Skill outside the
configured root, or observe file changes after startup. A missing or malformed
selected Skill prevents the App from becoming ready.

### Discover Skills on demand

`lenso.agent.skills.filesystem` is an ordinary Prompt and Tool Provider for
progressive Skill disclosure. It snapshots the immediate
`~/.agents/skills/<name>/SKILL.md` children and their readable resources during
startup. Its bounded Prompt contribution contains only ordered Skill names and
descriptions. When one matches the task, the Model can call `skill`
directly without a preliminary catalog Tool call. It also contributes four
Tools:

- `skill_list` returns only ordered names, descriptions, and SHA-256 content
  versions;
- `skill` returns the full snapshotted document for one exact name;
- `skill_resources` returns paths, sizes, and SHA-256 versions for one
  Skill without returning resource contents;
- `skill_resource` returns one snapshotted UTF-8 resource by exact Skill
  name and relative path.

`skill_list` remains a diagnostic and overflow fallback. If the configured
Prompt catalog byte budget cannot include every Skill, the deterministic
catalog reports the omitted count and tells the Model to use `skill_list`.
Skill bodies and resource contents never enter the Prompt catalog.

The Module enforces catalog/resource entry, per-file, aggregate content, and
manifest output limits. It rejects malformed Skill documents, directory/name
mismatches, path traversal, special filesystem entries, and every resource
symlink. Hidden, binary, and oversized resources are omitted from the readable
manifest and reported through an omitted count. Scripts are returned only as
text and are never executed. No file changes are observed until the next App
generation.

Enable the Skills Plugin independently of the selected Model:

```sh
cargo run -p lenso-agent-cli -- plugins enable skills \
  --evidence "reviewed local skills"
cargo run -p lenso-agent-cli -- \
  "Use the most relevant available Skill and one relevant resource to review this repository."
```

This Plugin requires `~/.agents/skills` to exist. When disabled, the base does
not inspect that directory.

## Tool profiles

Tool profiles are selected Module contributions, not Kernel modes or Tool
Runtime switches. Static profiles expand from a base App Definition; supported
optional profiles come from the persisted Plugin Active Set:

- `readonly` selects rooted observation providers such as `list`,
  `search`, `read`, and the filesystem Skills
  provider;
- `coding` enables the separate create-only/exact-edit workspace mutation
  Plugin;
- `local-coding` selects independently removable structured process Tools and a
  native process Provider;
- `automation` selects explicit domain Providers and does not receive raw
  workspace or process access by default.

The base exposes no generic shell, write, edit, delete, browser, or network
Tool. Enabling the reviewed `workspace-edit` Plugin adds only
`create_file` and `edit`. Enabling `local-process` adds `run_process` with an
explicit program catalog, workspace-relative cwd,
cleared-and-allowlisted environment, timeout, argument, and combined-output
limits. Removing Providers and bindings removes those Tool surfaces without
changing the Agent Loop or Kernel. See
[ADR-0004](docs/adr/0004-use-minimal-composed-tool-profiles-and-progressive-skills.md).

The base App admits up to four concurrent requests on its Agent-to-Tools and
Tool-Provider bindings. Providers mark each catalog entry as `parallel_safe`
or `exclusive`; the Agent Loop overlaps only consecutive safe calls, treats
every exclusive call as an ordering barrier, and returns results to the Model
in its original call order. The App's immutable binding admission and
`max_parallel_tool_calls` remain hard bounds; Provider metadata alone cannot
grant concurrency. See
[ADR-0027](docs/adr/0027-admit-bounded-parallel-tool-waves.md).

Enable bounded delegation independently:

```sh
cargo run -p lenso-agent-cli -- plugins enable subagent \
  --evidence "reviewed child Agent delegation"
cargo run -p lenso-agent-cli -- \
  "Delegate a README.md summary."
```

The `delegate` Tool invokes a separately composed child Agent and returns its
text plus a durable child Session identity. The child Tool Runtime can only
call the reviewed `workspace-read@1/read_text` Capability; enabling root
workspace mutation or local process Plugins does not expand child authority.
The first profile runs one child Turn at a time. See
[ADR-0028](docs/adr/0028-compose-bounded-subagents-as-tools.md).

Enable constrained Code Mode independently:

```sh
cargo run -p lenso-agent-cli -- plugins enable code-mode \
  --evidence "reviewed constrained Code Mode"
cargo run -p lenso-agent-cli -- \
  "Use Code Mode to compare README.md twice."
```

`run_code` executes bounded Lua 5.4 and exposes only `tool(name, arguments)`
and `parallel(calls)`. Its nested calls go through the separate read-only Tool
Runtime, so enabling root mutation or process Plugins does not widen code
authority. The interpreter has source, instruction, memory, output, subcall,
and parallel-subcall limits and no `io`, `os`, `package`, `debug`, filesystem,
process, or network library. It is not a hostile-code security sandbox. See
[ADR-0029](docs/adr/0029-compose-constrained-code-mode-as-a-tool.md).

Enable one-shot Tool approval independently:

```sh
cargo run -p lenso-agent-cli -- plugins enable approval \
  --evidence "reviewed one-shot Tool approval"
cargo run -p lenso-agent-cli -- \
  "Create one approved workspace note."
cargo run -p lenso-agent-cli -- approvals list
cargo run -p lenso-agent-cli -- approvals approve <approval-id>
cargo run -p lenso-agent-cli -- \
  "Create one approved workspace note."
```

The first attempt returns `approval_required` before the Provider runs. The
operator approves only the exact Tool name and normalized arguments in the
current App Generation, then retries; the grant is consumed once. The same
Hook provider is Plan-bound to the root and restricted read-only Tool Runtimes,
so direct Tools, Code Mode, and subagents cannot introduce a separate bypass.
The bundled policy allows `read_text` and asks for other Tool names. Disable it
with `plugins disable approval`. See
[ADR-0030](docs/adr/0030-compose-unified-tool-hooks-and-one-shot-approval.md).

## Run the opt-in coding slice

Enable workspace mutation over the deterministic readonly base, then prove
create, unique exact edit, and read-back:

```sh
cargo run -p lenso-agent-cli -- plugins enable workspace-edit \
  --evidence "reviewed workspace mutation"
cargo run -p lenso-agent-cli -- \
  "Create and edit a workspace note."
```

Model choice is independent of workspace authority. Workspace mutation is
explicitly privileged: use it only with a reviewed workspace root. Tool
arguments are retained in the durable Session trajectory, so do not use
mutation Tools for credentials or other secret content.

## Run the opt-in local coding slice

Enable process execution and workspace mutation independently to prove edit,
`cargo check`, and read-back through separate providers:

```sh
cargo run -p lenso-agent-cli -- plugins enable local-process \
  --evidence "reviewed local process execution"
cargo run -p lenso-agent-cli -- plugins enable workspace-edit \
  --evidence "reviewed local coding mutation"
cargo run -p lenso-agent-cli -- \
  "Edit and validate the workspace project."
```

The `local-process` Plugin allows `cargo`, `git`, and `rg`, but it is
deliberately not a hostile-code sandbox: Cargo build scripts, tests, Git configuration, and
allowed programs can execute code or perform effects available to the host
user. Use it only with reviewed code and a reviewed workspace. There is no
shell-string parsing, but that alone is not a security boundary. Command
arguments and output are durable Session trajectory facts and must not contain
secrets.

## Validate

```sh
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets
cargo test --locked --workspace
lenso app check --definition lenso.app.json
./scripts/check-contracts.sh
./scripts/check-removal.sh
```

The fixture Model deliberately proves replacement and orchestration boundaries;
it is not presented as a production model provider.

## Run an OpenAI-compatible provider

The `openai-compatible` Plugin replaces only the `model` Instance and adds an
explicit Env Secrets Module. The API key remains outside the project document
and Resolved App Plan:

```sh
export OPENAI_API_KEY="..."
cargo run -p lenso-agent-cli -- plugins enable openai-compatible \
  --evidence "reviewed remote model provider"
cargo run -p lenso-agent-cli --bin lenso-agent-cli -- \
  "Use read to read README.md, then summarize it."
```

The bundled profile defaults to OpenAI's base URL and `gpt-4o-mini`. Loopback
HTTP is accepted only by the test profile; remote providers require HTTPS.

## Use a ChatGPT subscription (experimental)

Start the browser PKCE OAuth flow, then check the app-local credential:

```sh
cargo run -p lenso-agent-cli -- auth login
cargo run -p lenso-agent-cli -- auth status
```

The default flow opens a browser and receives the verified callback on
`localhost:1455`, matching Pi's normal login shape. On a headless machine use:

```sh
cargo run -p lenso-agent-cli -- auth login --device-auth
```

OAuth profiles are stored together in `~/.lenso/agent/auth.json`, using a
Pi-style provider-keyed JSON shape. The directory is private and the credential
file is created with mode `0600` on Unix.

The subscription Plugin defaults to `gpt-5.6-luna` with medium reasoning.

Enable and run the subscription Model with:

```sh
cargo run -p lenso-agent-cli -- plugins enable codex-direct \
  --evidence "reviewed experimental subscription provider"
cargo run -p lenso-agent-cli -- \
  "Summarize this repository."
```

This profile directly provides `lenso.agent.model@2`, while Lenso continues to
own the Agent Loop, Tool Runtime, and Session log. Its private Auth Module owns
OAuth refresh credentials outside the repository and App Plan. The integration
does not depend on the Codex CLI. The same Model/Auth pair can be installed over
the fixture Plan through `examples/plugins/codex-direct`; the Profile Catalog,
not the publisher Manifest, owns its exact configurations and base Agent
configuration replacement.
