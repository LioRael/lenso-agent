# dev.example.portable-text-tools

This is one ordinary Rust Plugin source with two interchangeable implementations:
a sandboxed Wasm Component and a trusted native Process. The authored behavior lives in
`src/lib.rs`; `lenso-plugin-sdk` owns WIT, protocol dispatch, and runtime descriptors at compile
time, so no target-specific generated source is checked into the Plugin project.

```sh
lenso plugin check
lenso plugin dev --operation execute \
  --request-json '{"name":"uppercase","arguments_json":"{\"text\":\"hello\"}"}'
lenso plugin pack
```

`lenso plugin pack` builds both outputs into one immutable `.lenso-plugin` release. The Host
selects a compatible implementation; Plugin authors do not maintain two behavior implementations.

Add and exercise the Bundle from the Agent Harness:

```sh
lenso plugins add \
  examples/external-plugins/portable-text-tools/dist/dev.example.portable-text-tools-1.0.0.lenso-plugin

cargo run -p lenso-agent-cli -- \
  "Use the text Plugin to uppercase Lenso plugin."
```
