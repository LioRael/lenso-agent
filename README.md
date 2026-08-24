# Lenso Agent Harness

A headless-first, traceable Agent Harness composed from ordinary Lenso Modules
and portable Capabilities.

The project now includes its first executable `headless-readonly` slice. It
owns:

- the V1 product context and architecture decision;
- portable Agent, Model, Tools, Tool Provider, and Session Capability sources,
  plus the private structured Process Capability;
- portable Prompt aggregate and Prompt Provider Capability sources;
- generated Rust bindings derived from those sources, with Bun projections
  distributed by `@lenso/bun`;
- validation commands that keep generated artifacts fresh;
- deterministic, OpenAI-compatible, and experimental direct ChatGPT
  subscription Model Modules; Tool Runtime; Prompt aggregation; static
  Prompt/Skill contributions; workspace-read, opt-in workspace-edit, structured
  process execution, and progressive-disclosure filesystem Skills; file
  Session; Agent Loop; and CLI Modules; and
- eight checked App Compositions plus their canonical Resolved App Plans.

The Agent Harness is not a Kernel mode or a runtime plugin registry. Installed
packages and an App Composition materialize one immutable Resolved App Plan
before boot.

## Runtime baseline

The host currently uses released `lenso-app-plan 0.1.2` and
`lenso-kernel 0.1.7`. `lenso-runner` and `lenso-native-adapter` are locked to
`lenso-runtime-rust` commit
`25812bcbaf3b488d1a03f1864eb0130b53cadd93`, which closes the generic dynamic
Plugin control plane and preview Wasm Component, QuickJS, and native-dylib
Execution Adapters alongside the existing native host runtime.

The CLI now passes its reviewed Resolved App Plan through the first bounded
Plugin control-plane slice. It opens the content-addressed Plugin Store at
`.lenso/plugins/store`, closes the executable and installed native factories in
a Host Build Manifest, applies a native-only Host Execution Policy, and resolves
the validated base Plan plus an empty Plugin lock into one exact initial App
Generation. The already-resolved base Plan, including explicit Provider order,
remains authoritative in the Generation spec. Each Agent Turn holds a
Generation lease until its stream reaches a terminal outcome; host shutdown
then drains all Generation-owned resources.

The Host can admit reviewed passive Plugin releases and one narrow executable
native Tool Provider profile. Executable contributions must select the exact
linked `lenso.agent.text-tools@0.1.0` factory, expose only
`lenso.agent.tool-provider@1`, and remain stateless and permission-free. Data
mounts, permission requests, and binding templates fail admission. Overlap replacement, rollback, durable
cross-process fencing, Generation provenance in Session events, and product
acceptance of the preview Wasm Component, QuickJS, and native-dylib Adapters
remain deferred.

## Install and remove a reviewed Plugin release

A Bundle is a directory containing `lenso-plugin.json` plus exactly the files
declared by that Manifest. Admission rejects undeclared files, symlinks, digest
or size mismatches, unsupported selected targets, unregistered executable
factories, privileged or stateful contributions, and unbounded review evidence.

```sh
cargo run -p lenso-agent-cli -- plugins install \
  --bundle ./reviewed-plugin \
  --feature extras \
  --evidence "review-ticket-42"

cargo run -p lenso-agent-cli -- plugins status

cargo run -p lenso-agent-cli -- plugins install \
  --bundle examples/plugins/text-tools \
  --evidence "review-ticket-77"

cargo run -p lenso-agent-cli -- \
  --prompt "Use text.uppercase to uppercase Lenso plugin."

cargo run -p lenso-agent-cli -- plugins remove \
  --plugin example.text-tools
```

Admission stores immutable objects and its receipt under
`.lenso/plugins/store`. Activation atomically writes
`.lenso/plugins/active-set.json`, which embeds the exact `PluginSetLock`,
Manifest authorities, and Admission Receipt digests. The next App start
digest-verifies that closure and includes selected artifacts and executable
Instances in its initial Generation. The Harness owns one explicit attachment
rule from approved Tool Provider Plugin Instances to the existing `tools`
aggregator. Removing a Plugin atomically removes its Release, Instances, and
derived bindings from the next Generation. Reinstalling the same Plugin ID from
a different immutable Manifest remains an explicit future upgrade flow.

## Run the deterministic slice

From the repository root:

```sh
lenso check --project lenso.json --execution-class lenso.native-rust@1
lenso resolve --project lenso.json \
  --execution-class lenso.native-rust@1 \
  --output composition/headless-readonly/resolved-plan.json
cargo run -p lenso-agent-cli -- \
  --prompt "Summarize this workspace README."
```

The CLI writes the generated Session ID to stderr. Resume the durable Session
after a process restart with `--session <id>`. The Agent Loop streams text as
the selected Model produces it, supports direct answers and bounded sequential
Tool calls, and rebuilds a bounded completed-turn history for resumed Sessions.

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
lenso check --project lenso.openai-codex-direct-skills.json \
  --execution-class lenso.native-rust@1
lenso resolve --project lenso.openai-codex-direct-skills.json \
  --execution-class lenso.native-rust@1 \
  --output composition/openai-codex-direct-skills/resolved-plan.json
cargo run -p lenso-agent-cli -- \
  --plan composition/openai-codex-direct-skills/resolved-plan.json \
  --prompt "Use the most relevant available Skill and one relevant resource to review this repository."
```

This profile requires `~/.agents/skills` to exist. The base
`lenso.openai-codex-direct.json` remains portable and does not inspect that
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
lenso check --project lenso.coding.json --execution-class lenso.native-rust@1
lenso resolve --project lenso.coding.json \
  --execution-class lenso.native-rust@1 \
  --output composition/headless-coding/resolved-plan.json
cargo run -p lenso-agent-cli -- \
  --plan composition/headless-coding/resolved-plan.json \
  --prompt "Create and edit a workspace note."
```

For ChatGPT Subscription, use
`lenso.openai-codex-direct-coding.json` and
`composition/openai-codex-direct-coding/resolved-plan.json`. This profile is
explicitly mutating: run it only with a reviewed workspace root. Tool arguments
are retained in the durable Session trajectory, so do not use mutation Tools
for credentials or other secret content.

## Run the opt-in local coding slice

The deterministic local-coding Composition proves edit, `cargo check`, and
read-back through separate workspace and process providers:

```sh
lenso check --project lenso.local-coding.json \
  --execution-class lenso.native-rust@1
lenso resolve --project lenso.local-coding.json \
  --execution-class lenso.native-rust@1 \
  --output composition/headless-local-coding/resolved-plan.json
cargo run -p lenso-agent-cli -- \
  --plan composition/headless-local-coding/resolved-plan.json \
  --prompt "Edit and validate the workspace project."
```

For ChatGPT Subscription, use
`lenso.openai-codex-direct-local-coding.json` and its matching resolved Plan.
That ChatGPT profile allows `cargo`, `git`, and `rg`, but it is deliberately not
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
pnpm typecheck
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
lenso check --project lenso.openai.json \
  --execution-class lenso.native-rust@1
lenso resolve --project lenso.openai.json \
  --execution-class lenso.native-rust@1 \
  --output composition/openai-readonly/resolved-plan.json
cargo run -p lenso-agent-cli -- \
  --plan composition/openai-readonly/resolved-plan.json \
  --prompt "Use workspace.read_text to read README.md, then summarize it."
```

`lenso.openai.json` defaults to OpenAI's base URL and `gpt-4o-mini`. An App
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

Resolve and run the subscription Composition with:

```sh
lenso check --project lenso.openai-codex-direct.json \
  --execution-class lenso.native-rust@1
lenso resolve --project lenso.openai-codex-direct.json \
  --execution-class lenso.native-rust@1 \
  --output composition/openai-codex-direct/resolved-plan.json
cargo run -p lenso-agent-cli -- \
  --plan composition/openai-codex-direct/resolved-plan.json \
  --prompt "Summarize this repository."
```

This profile directly provides `lenso.agent.model@1`, while Lenso continues to
own the Agent Loop, Tool Runtime, and Session log. Its private Auth Module owns
OAuth refresh credentials outside the repository and App Plan. The integration
does not depend on the Codex CLI.
