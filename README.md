# Lenso Agent Harness

A headless-first, traceable Agent Harness composed from ordinary Lenso Modules
and portable Capabilities.

The project now includes its first executable `headless-readonly` slice. It
owns:

- the V1 product context and architecture decision;
- portable Agent, Model, Tools, Tool Provider, and Session Capability sources;
- portable Prompt aggregate and Prompt Provider Capability sources;
- generated Rust and TypeScript bindings derived from those sources;
- validation commands that keep generated artifacts fresh;
- deterministic, OpenAI-compatible, and experimental direct ChatGPT
  subscription Model Modules; Tool Runtime; Prompt aggregation; static
  Prompt/Skill contributions; workspace-read and progressive-disclosure
  filesystem Skills; file Session; Agent Loop; and CLI Modules; and
- four checked App Compositions plus their canonical Resolved App Plans.

The Agent Harness is not a Kernel mode or a runtime plugin registry. Installed
packages and an App Composition materialize one immutable Resolved App Plan
before boot.

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

- `readonly` selects rooted observation providers such as
  `workspace.read_text` and the filesystem Skills provider;
- `coding` will add separate workspace mutation and process execution Providers
  when those slices are implemented; and
- `automation` selects explicit domain Providers and does not receive raw
  workspace or process access by default.

The current checked Compositions are `readonly`. They expose no generic shell,
write, edit, delete, browser, or network Tool. Removing a Provider and its
bindings removes its entire Tool surface without changing the Agent Loop or
Kernel. See [ADR-0004](docs/adr/0004-use-minimal-composed-tool-profiles-and-progressive-skills.md).

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
