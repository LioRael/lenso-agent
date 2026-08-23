# Lenso Agent Harness

A headless-first, traceable Agent Harness composed from ordinary Lenso Modules
and portable Capabilities.

The project now includes its first executable `headless-readonly` slice. It
owns:

- the V1 product context and architecture decision;
- portable Agent, Model, Tools, Tool Provider, and Session Capability sources;
- generated Rust and TypeScript bindings derived from those sources;
- validation commands that keep generated artifacts fresh;
- deterministic, OpenAI-compatible, and experimental direct ChatGPT
  subscription Model Modules, Tool Runtime,
  workspace-read, file Session, Agent Loop, and CLI Modules; and
- three checked App Compositions plus their canonical Resolved App Plans.

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
