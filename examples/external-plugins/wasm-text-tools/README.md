# dev.example.wasm-text-tools

This standalone project was created with `lenso plugin new`. It provides one Tool Plugin without a
hand-written manifest or Host-specific internal wiring.

Use the public Plugin workflow from this directory:

```sh
lenso plugin check
lenso plugin dev
lenso plugin pack
```

Add and exercise the Bundle from the Agent Harness:

```sh
cargo run -p lenso-agent-cli -- plugins add \
  examples/external-plugins/wasm-text-tools/dist/dev.example.wasm-text-tools-1.0.0.lenso-plugin

cargo run -p lenso-agent-cli -- \
  "Use the text Plugin to uppercase Lenso plugin."
```

The Harness admits only the bounded V1 shape: one isolated Wasm Component, one Agent Tool Provider
entry, no dependencies, permissions, state, mounts, features, or replacement behavior.
