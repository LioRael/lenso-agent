# Lenso Agent Harness

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

The base App in `lenso.app.json` uses a deterministic fixture Model and can only
read the current workspace. Enable workspace mutation without creating or
selecting another App definition:

```sh
cargo run -p lenso-agent-cli --bin lenso-agent-cli -- plugins enable workspace-edit \
  --evidence "reviewed workspace mutation"
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
contributions by name. The selection persists in
`.lenso/plugins/active-set.json`; there is no Composition file for each
combination:

```sh
# See the exact Releases bundled with this Host.
cargo run -p lenso-agent-cli -- plugins available

# Low-risk append-to-many Tool Provider: automatic local admission.
cargo run -p lenso-agent-cli -- plugins enable text-tools

# Workspace mutation changes authority and therefore requires review evidence.
cargo run -p lenso-agent-cli -- plugins enable workspace-edit \
  --evidence "reviewed workspace mutation"

# Add local Skills to both the Prompt and Tool aggregates.
cargo run -p lenso-agent-cli -- plugins enable skills \
  --evidence "reviewed local skills"

# Add the reviewed process catalog (cargo, git, and rg).
cargo run -p lenso-agent-cli -- plugins enable local-process \
  --evidence "reviewed local process execution"

cargo run -p lenso-agent-cli -- plugins status
cargo run -p lenso-agent-cli -- \
  "Create and edit a workspace note."

cargo run -p lenso-agent-cli -- plugins disable workspace-edit
```

The catalog also includes `openai-compatible` and experimental `codex-direct`
Model replacements. `workspace-edit`, `text-tools`, `skills`, and
`local-process` become independently selected Module Instances. Startup resolves
the persisted Plugin Set into a new immutable App Generation; the running Kernel
never discovers packages or mutates its graph. High-authority selections require
explicit review evidence.

## How Apps are composed

The repository has one source App Definition: `lenso.app.json`. It describes the
small read-only base. Optional Modules are selected through the persisted Plugin
Active Set, so combinations do not create more App files.

Before boot, the Host validates the definition and selected Plugins, then
materializes one immutable Resolved App Plan in ignored Host state. App authors
can check or reproduce that artifact:

```sh
lenso app check --definition lenso.app.json
lenso app resolve --definition lenso.app.json \
  --output .lenso/resolved-plan.json
```

`--plan <path>` remains an advanced escape hatch for exact Plan replay.
Resolved Plans are generated Host input and are never hand-edited or committed.
`scripts/check-removal.sh` proves static optional providers can be removed while
the remaining graph still resolves; Plugin tests prove independent Skills,
process, and workspace-edit removal. The TUI proof additionally removes every
panel Contribution while retaining the TUI Shell and Agent route.

## Compose the TUI

The root App selects a `tui` Shell Module that requires exactly one
`lenso.agent@1` provider and `many lenso.agent.tui-contribution@1` providers.
The Shell owns terminal mode, layout, focus, input, streaming, cancellation,
and cross-provider panel ID collision checks. Contribution Modules return
bounded semantic panel snapshots; they do not receive `ratatui` widgets or a
global registry.

`lenso.app.json` includes one removable `tui-help` static
Contribution. Another Module can contribute a panel by providing the same
Capability and being explicitly selected in App Composition. Removing all
Contribution providers leaves the Shell valid with only the conversation.

## Runtime baseline

The host currently resolves `lenso-app-plan 0.1.5` and `lenso-kernel 0.1.11`.
All runtime crates are locked to one reviewed `lenso-runtime-rust` commit,
`fb364b9ff3927d82e4911f1a1e23d9ac006adc6b`; contract authoring and codegen are
locked to `lenso-protocols` revision
`8a9b2482278224973417aaac1fd925ba1cfa5370`, while Module authoring remains on
`1424ffe25f05c0d3aaf746fb7fd66b26b9f803e0`. Secrets packages are locked to
`lenso-secrets-module` revision
`a52177f8508a23e9b72d9983ff79896c2a7e7695`, which uses the same native-adapter
revision as the Host. This closes the generic dynamic
Plugin control plane and preview Wasm Component, QuickJS, and native-dylib
Execution Adapters alongside the existing native host runtime, and preserves
declared request/stream operation kinds when Plugin Manifests become Plans.

The CLI now passes its reviewed Resolved App Plan through the first bounded
Plugin control-plane slice. It opens the content-addressed Plugin Store at
`.lenso/plugins/store`, closes the executable, installed native factories, and
the Wasm Component and QuickJS Adapter profiles in a Host Build Manifest, and resolves
the validated base Plan plus an empty Plugin lock into one exact initial App
Generation. The already-resolved base Plan, including explicit Provider order,
remains authoritative in the Generation spec. Each Agent Turn holds a
Generation lease until its stream reaches a terminal outcome; host shutdown
then drains all Generation-owned resources. Before the Ready Gate, the Host
stores the canonical Generation Spec by digest under
`.lenso/plugins/generations`. The lease injects that digest into the root Agent
Invocation Context, and each `turn_started` Session event records it without
making provenance a user-supplied request field.

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

## Install, upgrade, roll back, and remove a Plugin release

A Bundle is a directory containing `lenso-plugin.json` plus exactly the files
declared by that Manifest. Admission rejects undeclared files, symlinks, digest
or size mismatches, unsupported selected targets, unregistered executable
factories, privileged or stateful contributions, and unbounded review evidence.
For exact Releases bundled with this Host, prefer `plugins enable <name>` and
`plugins disable <name>`; the Bundle path commands remain the third-party and
upgrade surface.

```sh
cargo run -p lenso-agent-cli -- plugins install \
  --bundle ./reviewed-plugin \
  --feature extras

# Install the checked-in non-native Agent Loop replacement.
cargo run -p lenso-agent-cli -- plugins install \
  --bundle examples/plugins/quickjs-agent \
  --evidence local-review

cargo run -p lenso-agent-cli -- plugins status

cargo run -p lenso-agent-cli -- plugins install \
  --bundle examples/plugins/text-tools

# Build and install the standalone third-party Wasm Tool example.
cargo build \
  --manifest-path examples/external-plugins/wasm-text-tools/guest/Cargo.toml \
  --release --target wasm32-unknown-unknown
lenso plugin build \
  --manifest examples/external-plugins/wasm-text-tools/lenso-plugin.template.json \
  --artifact tool-wasm=examples/external-plugins/wasm-text-tools/guest/target/wasm32-unknown-unknown/release/external_wasm_text_tools.wasm \
  --output dist/external-wasm-text-tools
lenso plugin verify --bundle dist/external-wasm-text-tools
cargo run -p lenso-agent-cli -- plugins install \
  --bundle dist/external-wasm-text-tools \
  --evidence local-review

# The network example follows the same build flow. Its Manifest must request
# exact origins, and the base App must select those same origins in
# the lenso.agent.http-fetch Provider configuration before Ready succeeds.
cargo build \
  --manifest-path examples/external-plugins/wasm-http-fetch/guest/Cargo.toml \
  --release --target wasm32-unknown-unknown

# The Host reads the active Manifest CAS under its authority fence.
cargo run -p lenso-agent-cli -- plugins upgrade \
  --bundle examples/plugins/text-tools-v2

# Use the previous-active-set digest printed by upgrade.
cargo run -p lenso-agent-cli -- plugins rollback \
  --to sha256:<previous-active-set-digest>

cargo run -p lenso-agent-cli -- plugins history

cargo run -p lenso-agent-cli -- plugins inspect \
  --active-set sha256:<active-set-digest>

cargo run -p lenso-agent-cli -- generations inspect \
  --digest sha256:<generation-spec-digest>

cargo run -p lenso-agent-cli -- generations gc-preview

cargo run -p lenso-agent-cli -- sessions provenance \
  --session <session-id>

cargo run -p lenso-agent-cli -- \
  "Use uppercase to uppercase Lenso plugin."

cargo run -p lenso-agent-cli -- plugins remove \
  --plugin example.text-tools

cargo run -p lenso-agent-cli -- plugins install \
  --bundle examples/plugins/model-fixture \
  --evidence "review-ticket-88"

cargo run -p lenso-agent-cli -- \
  "Answer directly: hello"

cargo run -p lenso-agent-cli -- plugins remove \
  --plugin example.fixture-model

cargo run -p lenso-agent-cli -- auth login

cargo run -p lenso-agent-cli -- plugins install \
  --bundle examples/plugins/codex-direct \
  --evidence "review-ticket-92"

cargo run -p lenso-agent-cli -- \
  "Summarize this repository."

cargo run -p lenso-agent-cli -- plugins remove \
  --plugin example.codex-direct
```

Passive Releases and selected executable contributions that are stable,
trusted, stateless, permission-free, dependency-free, Artifact-free, and only
append to a `many` requirement receive automatic local admission. The Receipt
records that derived decision and the CLI prints it as `governance`.
Replacement, state, permissions, dependencies, Artifact-backed execution, and
preview or experimental Profiles still require explicit `--evidence`. An
explicit `--expected-manifest` remains available for automation that already
owns a prior CAS value. Upgrade and rollback use the root base definition by
default. `LENSO_APP_DEFINITION`, `LENSO_RESOLVED_PLAN`, and `--plan <path>`
remain advanced overrides for automation and exact Plan replay.

Review evidence does not itself create runtime authority. For the network
profile, the publisher's exact origin request is admitted as an
`ApprovedGrant`; the App must also select `lenso.agent.http-fetch` and configure
an allowlist containing that scope. The checked-in `lenso.app.json` keeps
the allowlist empty by default, so it grants no ambient network access.

Admission stores immutable objects and its receipt under
`.lenso/plugins/store`. Activation atomically writes
`.lenso/plugins/active-set.json`, which embeds the exact `PluginSetLock`,
Manifest authorities, and Admission Receipt digests. A running Host observes a
new committed Active Set without blocking on a Plugin command's authority
fence, resolves and stages a fresh immutable Generation, and advances the
routing epoch only after its Ready Gate succeeds. Existing Turns keep their old
Generation Lease; new Turns use the new Generation, and the old Generation is
retired after its final Lease is released. A stopped Host digest-verifies the
same closure on its next start. The Catalog derives the registered Tool
Provider attachment to the existing `tools` aggregator only when that consumer
declares the exact Capability with `many` cardinality. Its Model profile
requires `one` cardinality, the exact base `agent -> model` edge, and the
allowlisted fixture package; it removes the displaced Instance and all of that
Instance's bindings before resolving the next Generation. The Codex Direct
Bundle additionally closes exact Model and Auth contribution profiles, one
`model -> auth` requirement/template, `gpt-5.6-luna` with medium reasoning, and
the compatible base Agent configuration. Removing any replacement Plugin
atomically removes its Release, Instances, and derived bindings and restores
the exact base Plan. `plugins upgrade` admits a different immutable Manifest
only after an explicit Manifest CAS and a Runtime maintenance Ready Gate. It
retains canonical authorities by digest under `.lenso/plugins/active-sets`;
`plugins rollback` applies the same Ready-before-commit rule to an exact
retained digest. The commands remain offline validation and commit
transactions; a running Host separately reconciles the committed authority
through an overlap Generation transition. This is not running-Kernel graph
mutation. A Host-owned cross-process authority fence lets startup and validated
inspection snapshot either the complete old authority or the complete committed
authority, while install, remove, upgrade, and rollback retain exclusive
ownership from their first authority read through atomic commit. The read-only
history and inspection commands validate every selected canonical record and
its closure; Session provenance reports each Turn's Spec as available, missing,
or invalid without rendering the stored input. The local filesystem fence is
not a distributed lease or network-filesystem portability claim. Adding another
executable shape remains a Host code and review change. The one
package-independent pure Wasm Tool shape is not runtime discovery or general
permission to import Host Capabilities or replace a `one` binding. The
separately reviewed workspace-reader shape imports only the Host-selected
`workspace-read@1/read_text` Capability recorded in its immutable Generation.

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
base definition binds `fixture-instructions` and `summary-skill` to the
`prompt` aggregate. Their binding order is the Model-visible order.

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

This profile directly provides `lenso.agent.model@1`, while Lenso continues to
own the Agent Loop, Tool Runtime, and Session log. Its private Auth Module owns
OAuth refresh credentials outside the repository and App Plan. The integration
does not depend on the Codex CLI. The same Model/Auth pair can be installed over
the fixture Plan through `examples/plugins/codex-direct`; the Profile Catalog,
not the publisher Manifest, owns its exact configurations and base Agent
configuration replacement.
