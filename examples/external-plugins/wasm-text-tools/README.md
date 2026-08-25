# External Wasm Tool Plugin

This standalone Cargo project models a third-party Tool Plugin. It has no path dependency on the
Agent Harness workspace and can be copied to another repository unchanged.

Build and package it from the Agent Harness repository root:

```sh
cargo build --manifest-path examples/external-plugins/wasm-text-tools/guest/Cargo.toml \
  --release --target wasm32-unknown-unknown

lenso plugin build \
  --manifest examples/external-plugins/wasm-text-tools/lenso-plugin.template.json \
  --artifact tool-wasm=examples/external-plugins/wasm-text-tools/guest/target/wasm32-unknown-unknown/release/external_wasm_text_tools.wasm \
  --output dist/external-wasm-text-tools

lenso plugin verify --bundle dist/external-wasm-text-tools
```

Install and exercise the reviewed Bundle:

```sh
cargo run -p lenso-agent-cli -- plugins install \
  --bundle dist/external-wasm-text-tools \
  --evidence local-review

cargo run -p lenso-agent-cli -- \
  "Use the text Plugin to uppercase Lenso plugin."
```

The Host accepts this shape without registering its package identity in code. Admission remains
bounded to an isolated Wasm Component that provides the exact Agent Tool Provider Capability,
requires no Host Capability, requests no permission or state, uses empty configuration, and appends
only to the existing `tools` aggregate.
