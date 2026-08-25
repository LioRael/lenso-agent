# Lenso Agent Harness

A headless-first Agent Harness for running a model with an explicitly selected
set of Tools, Prompt instructions, and durable Sessions.

## Try it

Run the deterministic, read-only App from the repository root:

```sh
cargo run -p lenso-agent-cli -- \
  "Summarize this workspace README."
```

The default `headless-readonly` App uses a deterministic fixture Model and can
only read the current workspace. Choose another reviewed App by name when you
need a different Model or Tool authority:

```sh
cargo run -p lenso-agent-cli -- \
  --app headless-coding \
  "Create and edit a workspace note."
```

Use `--session <id>` to resume a durable Session, `--no-tools` to remove Tool
access for one Turn, or repeated `--allow-tool <name>` options to narrow the
selected App's Tool set. Run `cargo run -p lenso-agent-cli -- --help` for the
complete interface.

## Choose an App

| App | Model | Workspace authority |
| --- | --- | --- |
| `headless-readonly` | deterministic fixture | read only |
| `headless-coding` | deterministic fixture | read and edit |
| `headless-local-coding` | deterministic fixture | read, edit, and reviewed processes |
| `openai-readonly` | OpenAI-compatible API | read only |
| `openai-codex-direct` | ChatGPT subscription | read only |
| `openai-codex-direct-skills` | ChatGPT subscription | read only, plus selected local Skills |
| `openai-codex-direct-coding` | ChatGPT subscription | read and edit |
| `openai-codex-direct-local-coding` | ChatGPT subscription | read, edit, and reviewed processes |

The coding Apps can mutate the selected workspace. The local-coding Apps also
run an explicit program catalog and are trusted-code profiles, not sandboxes.

## How Apps are composed

Each executable variant has one source `composition/<variant>.app.json` App
Definition. It selects locked Module packages, configuration, and bindings. The
authoring tool validates that definition and materializes an immutable Resolved
App Plan before boot; the Kernel never discovers packages or mutates the
running graph.

Normal runs select the reviewed App by name, so callers do not need to manage
Plan files. App authors and release automation can reproduce the exact derived
artifact explicitly:

```sh
lenso app check --definition composition/headless-readonly.app.json
lenso app resolve --definition composition/headless-readonly.app.json \
  --output .lenso/headless-readonly/resolved-plan.json
```

`--plan <path>` remains an advanced escape hatch for exact Plan replay. The
tracked `composition/*/resolved-plan.json` files are review and release
evidence; never edit them by hand. `scripts/check-removal.sh` proves that
optional Prompt, Skill, workspace-edit, and process Modules can be removed
while the remaining graph still resolves.

## Runtime baseline

The host currently uses released `lenso-app-plan 0.1.4` and
`lenso-kernel 0.1.9`. `lenso-runner` and `lenso-native-adapter` are locked to
`lenso-runtime-rust` commit
`c56e4a01d14704eeae26e2121dbd87dbf380b1d3`, which closes the generic dynamic
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
The Catalog admits the linked `lenso.agent.text-tools@0.1.0` factory as a
stateless, permission-free append-to-`many` Tool Provider. It also admits one
package-independent, isolated Wasm Tool Provider shape with the same exact
Capability and attachment, empty configuration, no Host imports, permissions,
state, Data mounts, or binding templates, and mandatory review evidence. It
also admits one reviewed variant with exactly one generated
`lenso.agent.workspace-read@1/read_text` Host import. The Host Profile fixes
that binding to the dedicated base `workspace-import-read` Instance; the Bundle cannot select a
provider or request workspace write, process, network, Secrets, state, or Data
mount authority. It
admits one restricted `lenso.agent.model.fixture@0.1.0` profile that replaces
the fixture base Plan's exact `model` provider for the `agent` consumer. The
experimental Codex Direct profile admits one atomic Model/Auth pair, its exact intra-Plugin
binding, and the coupled Agent model configuration. Experimental Artifact
profiles additionally allow a reviewed QuickJS or Wasm Component Module to
replace the exact native Agent Loop through the generated `AgentJsonCodec`.
Other Data mounts, permission requests, arbitrary binding templates, extra
Capability requirements, and incomplete Feature selections fail
admission. General provider/configuration selection, overlap replacement,
automatic rollback, distributed coordination, Generation deletion, Plugin
Store garbage collection, native-dylib product acceptance, and general
third-party Host Capability permissions remain deferred.

## Install, upgrade, roll back, and remove a Plugin release

A Bundle is a directory containing `lenso-plugin.json` plus exactly the files
declared by that Manifest. Admission rejects undeclared files, symlinks, digest
or size mismatches, unsupported selected targets, unregistered executable
factories, privileged or stateful contributions, and unbounded review evidence.

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
  "Use text.uppercase to uppercase Lenso plugin."

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
owns a prior CAS value. Upgrade and rollback accept `--app <name>` and default
to `headless-readonly`. `LENSO_RESOLVED_PLAN` and `--plan <path>` remain
advanced overrides for automation and exact Plan replay.

Admission stores immutable objects and its receipt under
`.lenso/plugins/store`. Activation atomically writes
`.lenso/plugins/active-set.json`, which embeds the exact `PluginSetLock`,
Manifest authorities, and Admission Receipt digests. The next App start
digest-verifies that closure and includes selected artifacts and executable
Instances in its initial Generation. The Catalog derives the registered Tool
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
retained digest. These commands are offline transitions, not running-Kernel hot
loading. A Host-owned cross-process authority fence lets startup and validated
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

Prompt and Skill plugins are ordinary Modules selected before boot. The
checked fixture Composition binds `fixture-instructions` and `summary-skill`
to the `prompt` aggregate. Their binding order is the Model-visible order.

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
descriptions. When one matches the task, the Model can call `skills.read`
directly without a preliminary catalog Tool call. It also contributes four
Tools:

- `skills.list` returns only ordered names, descriptions, and SHA-256 content
  versions;
- `skills.read` returns the full snapshotted document for one exact name;
- `skills.list_resources` returns paths, sizes, and SHA-256 versions for one
  Skill without returning resource contents;
- `skills.read_resource` returns one snapshotted UTF-8 resource by exact Skill
  name and relative path.

`skills.list` remains a diagnostic and overflow fallback. If the configured
Prompt catalog byte budget cannot include every Skill, the deterministic
catalog reports the omitted count and tells the Model to use `skills.list`.
Skill bodies and resource contents never enter the Prompt catalog.

The Module enforces catalog/resource entry, per-file, aggregate content, and
manifest output limits. It rejects malformed Skill documents, directory/name
mismatches, path traversal, special filesystem entries, and every resource
symlink. Hidden, binary, and oversized resources are omitted from the readable
manifest and reported through an omitted count. Scripts are returned only as
text and are never executed. No file changes are observed until the next App
generation.

The opt-in ChatGPT Subscription composition enables this catalog without
changing the base direct profile:

```sh
cargo run -p lenso-agent-cli -- \
  --app openai-codex-direct-skills \
  "Use the most relevant available Skill and one relevant resource to review this repository."
```

This profile requires `~/.agents/skills` to exist. The base
`openai-codex-direct` variant remains portable and does not inspect that
directory.

## Tool profiles

Tool profiles are App Composition recipes, not Kernel modes or Tool Runtime
switches. A profile expands to ordinary selected Tool Provider Module Instances
and explicit bindings:

- `readonly` selects rooted observation providers such as `workspace.list`,
  `workspace.search`, `workspace.read_text`, and the filesystem Skills
  provider;
- `coding` adds the separate create-only/exact-edit workspace mutation Provider;
- `local-coding` adds independently removable structured process Tools and a
  native process Provider to `coding`; and
- `automation` selects explicit domain Providers and does not receive raw
  workspace or process access by default.

The existing readonly Compositions still expose no generic shell, write, edit,
delete, browser, or network Tool. The two opt-in coding Compositions add only
`workspace.write_text` and `workspace.edit_text`. The two higher-authority
local-coding Compositions additionally expose `process.exec` with an explicit
program catalog, workspace-relative cwd, cleared-and-allowlisted environment,
timeout, argument, and combined-output limits. Removing Providers and bindings
removes those Tool surfaces without changing the Agent Loop or Kernel. See
[ADR-0004](docs/adr/0004-use-minimal-composed-tool-profiles-and-progressive-skills.md).

## Run the opt-in coding slice

The deterministic coding Composition proves create, unique exact edit, and
read-back without changing the readonly Composition:

```sh
cargo run -p lenso-agent-cli -- \
  --app headless-coding \
  "Create and edit a workspace note."
```

For ChatGPT Subscription, select the `openai-codex-direct-coding` App. This App
is explicitly mutating: run it only with a reviewed workspace root. Tool
arguments are retained in the durable Session trajectory, so do not use
mutation Tools for credentials or other secret content.

## Run the opt-in local coding slice

The deterministic local-coding Composition proves edit, `cargo check`, and
read-back through separate workspace and process providers:

```sh
cargo run -p lenso-agent-cli -- \
  --app headless-local-coding \
  "Edit and validate the workspace project."
```

For ChatGPT Subscription, select the `openai-codex-direct-local-coding` App.
That App allows `cargo`, `git`, and `rg`, but it is deliberately not
a hostile-code sandbox: Cargo build scripts, tests, Git configuration, and
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
for definition in composition/*.app.json; do
  lenso app check --definition "${definition}"
done
./scripts/check-contracts.sh
./scripts/check-removal.sh
```

The fixture Model deliberately proves replacement and orchestration boundaries;
it is not presented as a production model provider.

## Run an OpenAI-compatible provider

The second Composition replaces only the `model` Instance and adds an explicit
Env Secrets Module. The API key remains outside the project document and
Resolved App Plan:

```sh
export OPENAI_API_KEY="..."
cargo run -p lenso-agent-cli -- \
  --app openai-readonly \
  "Use workspace.read_text to read README.md, then summarize it."
```

The `openai-readonly` variant defaults to OpenAI's base URL and `gpt-4o-mini`. An App
author can select another Chat Completions-compatible base URL and model, then
resolve and review a new Plan. Loopback HTTP is accepted only for tests; remote
providers require HTTPS.

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

The subscription Composition defaults to `gpt-5.6-luna` with medium reasoning.

Run the subscription App with:

```sh
cargo run -p lenso-agent-cli -- \
  --app openai-codex-direct \
  "Summarize this repository."
```

This profile directly provides `lenso.agent.model@1`, while Lenso continues to
own the Agent Loop, Tool Runtime, and Session log. Its private Auth Module owns
OAuth refresh credentials outside the repository and App Plan. The integration
does not depend on the Codex CLI. The same Model/Auth pair can be installed over
the fixture Plan through `examples/plugins/codex-direct`; the Profile Catalog,
not the publisher Manifest, owns its exact configurations and base Agent
configuration replacement.
