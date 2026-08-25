# Rust/Wasm Agent Plugin

This example replaces the native Agent Loop with an isolated WebAssembly Component. Its business
code uses generated Capability clients such as `context.model.complete(...)`; it does not inspect
binding IDs or encode Host invocation envelopes itself.

The plugin imports the exact Model, Prompt, Session, and Tools bindings selected by the immutable
App Generation. The CLI integration test builds the core Wasm module, converts it to a Component,
materializes a digest-bound plugin bundle, installs it, resolves a new Generation, and runs a turn.

Build the guest core module with:

```sh
cargo build --manifest-path examples/plugins/wasm-agent/guest/Cargo.toml \
  --release --target wasm32-unknown-unknown
```

The source manifest is a bundle template. Artifact digest and size are materialized from the built
Component before installation; a later packaging command can automate that final distribution step.

