# dev.example.portable-text-tools

This is one ordinary Agent Tool Plugin source with two interchangeable implementations:
a sandboxed Wasm Component and a trusted native Process. `src/lib.rs` uses the same
`#[lenso::plugin]`, `#[tool_provider]`, and `#[tool]` authoring interface as a statically linked
Plugin. The Agent Tool SDK owns Tool catalog and dispatch semantics; the Runtime SDK owns WIT,
protocol framing, and target descriptors. No target-specific business source is checked in.

```sh
lenso plugin check
lenso plugin dev --operation execute \
  --request-json '{"name":"uppercase","arguments_json":"{\"text\":\"hello\"}"}'
lenso plugin pack
```

`lenso plugin pack` builds both outputs into one immutable `.lenso-plugin` release. The Host
selects a compatible implementation; Plugin authors do not maintain two behavior implementations.

Add and exercise the Bundle from Lenso Agent:

```sh
lenso plugins add \
  examples/external-plugins/portable-text-tools/dist/dev.example.portable-text-tools-1.0.0.lenso-plugin

cargo run -p lenso-agent-cli -- \
  "Use the text Plugin to uppercase Lenso plugin."
```
