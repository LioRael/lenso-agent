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

Add the Bundle and create an Instance in an unmanaged Agent Home that has
already been initialized by the Agent Host:

```sh
lenso plugins add --root "$LENSO_AGENT_HOME" \
  /absolute/path/to/dev.example.portable-text-tools-1.0.0.lenso-plugin
lenso plugins configure --root "$LENSO_AGENT_HOME" \
  dev.example.portable-text-tools default

lenso-agent-cli "Use the text Plugin to uppercase Lenso plugin." \
  --allow-tool uppercase
```

Run the Agent from your workspace. Adding the Bundle makes its release
available; configuring `default` selects one Instance. The Host must support
its selected implementation and the explicit Tool grant must include
`uppercase`. A multi-implementation Bundle with Process authoring v2 requires
a CLI that can compose dependency-free v1/v2 Contracts and a Wasm Adapter
that admits that dependency-free v2 Contract. Older CLI or Host versions do
not satisfy this combination; see the [acceptance report](../../../docs/acceptance/2026-09-07-workflows.md) for the tested cohort.

Disable or remove the Instance from the same Agent Home:

```sh
lenso plugins disable --root "$LENSO_AGENT_HOME" dev.example.portable-text-tools default
lenso plugins enable --root "$LENSO_AGENT_HOME" dev.example.portable-text-tools default
lenso plugins remove --root "$LENSO_AGENT_HOME" dev.example.portable-text-tools
```
