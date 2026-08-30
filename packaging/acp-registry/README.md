# ACP Registry packaging

Zed installs new ACP agents through the ACP Registry. The former Zed Agent
Server extension format is deprecated, so this directory contains Registry
submission assets rather than a Zed extension.

The repository Release workflow builds, tests, and packages `lenso-agent-acp`
for every admitted release target. For a local packaging check, build one
target and create its deterministic archive and checksum:

```sh
cargo build --locked --release -p lenso-agent-acp --target aarch64-apple-darwin
./scripts/package-acp-binary.sh \
  0.1.0 \
  darwin-aarch64 \
  target/aarch64-apple-darwin/release/lenso-agent-acp \
  dist
```

After every intended release target has been packaged, render the Registry
entry against the exact versioned GitHub Release URL:

```sh
./scripts/render-acp-registry-entry.py \
  --version 0.1.0 \
  --base-url https://github.com/LioRael/lenso-agent/releases/download/v0.1.0 \
  --artifacts dist \
  --output dist/agent.json
```

Do not submit the entry until every declared archive URL returns the published
bytes, its checksum matches, and `lenso-agent-acp` advertises the `chatgpt`
Agent Auth method. Copy `agent.json` and `icon.svg` into an `lenso/` directory
in a fork of `agentclientprotocol/registry`, then run:

```sh
python3 .github/workflows/verify_agents.py --auth-check --agent lenso
uv run --with jsonschema .github/workflows/build_registry.py
```

Open the Registry PR only after both checks pass against the live Release.
