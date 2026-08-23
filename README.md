# Lenso Agent Harness

A headless-first, traceable Agent Harness composed from ordinary Lenso Modules
and portable Capabilities.

The project now includes its first executable `headless-readonly` slice. It
owns:

- the V1 product context and architecture decision;
- portable Agent, Model, Tools, Tool Provider, and Session Capability sources;
- generated Rust and TypeScript bindings derived from those sources; and
- validation commands that keep generated artifacts fresh;
- deterministic Model, Tool Runtime, workspace-read, file Session, Agent Loop,
  and CLI Modules; and
- one checked App Composition plus its canonical Resolved App Plan.

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
after a process restart with `--session <id>`.

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
it is not presented as a production model provider. A real model package can
replace the `model` Instance without changing the Agent Loop or Kernel.
