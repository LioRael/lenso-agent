# Optional Agent convention

Adopt this local support package with `lenso app add <package-directory>`.
A selected `agent/` contributes either one `tools.ts` or `tools.rs`, an explicit
`profile.toml`, or both. `instructions.md` is a Profile input, never a standalone
Agent or permission grant:

```text
app/orders/agent/
├── tools.ts              # or tools.rs
├── profile.toml          # selects an explicit Agent Profile name and policy
└── instructions.md       # optional source for that Profile's instructions

app/assistant/agent/
├── profile.toml          # a Profile-only composition is valid
└── instructions.md
```

`profile.toml` uses ordinary Agent Profile fields, then ends with an
authoring-only `[lenso]` name declaration. It must explicitly declare
`instances` and `allowed_tools`. When the same directory contributes a Tool,
the compiler adds only that generated Tool Provider instance. A Profile-only
directory leaves its declared `instances` unchanged, so it can select existing
configured capabilities without creating a placeholder Provider.
`instructions.md` requires this Profile and cannot silently replace inline
Profile instructions. Private dependencies belong to `agent/package.json` or
`agent/Cargo.toml`; inactive surfaces are not installed or compiled. TypeScript
exports one `tools([...])` declaration from `@lenso/agent-tool-sdk`. Rust uses
the existing Plugin and `tool_provider` macros. Its generated project uses the
portable `lenso-plugin-sdk` facade and packages one source as both Wasm and
Process implementations. A Rust `agent/Cargo.toml` may add dependencies, but
its `lenso` dependency must remain `package = "lenso-plugin-sdk"`; native
`lenso` is rejected because it would make the Tool Host-linked.

```toml
description = "Order operations"
instances = []
allowed_tools = ["greet"]

[lenso]
profile = "orders"
```

Tool contributions publish a hashed `lenso.agent.deployment@1` resource beside
the ordinary `lenso.agent.tool-provider@2` Bundle. A Profile-only contribution
publishes `lenso.agent.deployment@2` through the Engine's generic
resource-only convention output: it has no Bundle, Plugin Root directory,
runtime artifact, or language dependency installation. Inspect either form
with `lenso-agent dx check --from dist`; import it explicitly with
`lenso-agent dx apply --from dist`.

The Agent stages and resolves the resulting Profile through its existing Host
Profile and Plugin Root before publishing it. A Tool Provider is installed as
disabled in the default Agent; only an explicit Profile can select it. A
tools-only contribution stays disabled until the user selects it through the
ordinary Agent configuration lifecycle. A Profile-only contribution never
activates a Tool by virtue of its directory or ancestry. None of these build or
import steps launches an Agent or grants model permission. An App Agent's
existing Tool runtime, Profiles, and explicit tool policy own availability and
execution authorization. Console needs neither this support nor an Agent to
run.
