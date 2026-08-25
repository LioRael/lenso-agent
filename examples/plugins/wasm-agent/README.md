# Rust/Wasm Agent Plugin

This example replaces the native Agent Loop with an isolated WebAssembly Component. Its business
code uses generated Capability clients such as `context.model.complete(...)`; it does not inspect
binding IDs or encode Host invocation envelopes itself.

The plugin imports the exact Model, Prompt, Session, and Tools bindings selected by the immutable
App Generation. The CLI integration test builds the core Wasm module, uses Lenso's production
Bundle builder to convert it to a Component and materialize exact digests, installs it, resolves a
new Generation, and runs a turn.

Build the guest core module with:

```sh
cargo build --manifest-path examples/plugins/wasm-agent/guest/Cargo.toml \
  --release --target wasm32-unknown-unknown

lenso plugin build \
  --manifest examples/plugins/wasm-agent/lenso-plugin.template.json \
  --artifact agent-wasm=examples/plugins/wasm-agent/guest/target/wasm32-unknown-unknown/release/lenso_wasm_agent_example.wasm \
  --output dist/wasm-agent

lenso plugin verify --bundle dist/wasm-agent
```

The source manifest is a Bundle template. `plugin build` never executes the guest or overwrites an
existing output directory. It converts `wasm_component` source artifacts, calculates digest and
size, writes the canonical `lenso-plugin.json`, and verifies the exact file closure before success.
