# Optional Agent convention

Adopt this local support package with `lenso app add <package-directory>`.
A selected `agent/` contains one `tools.ts` or `tools.rs`. It can also contain
an optional `profile.toml` and `instructions.md`:

```text
app/orders/agent/
├── tools.ts              # or tools.rs
├── profile.toml          # selects an explicit Agent Profile name and policy
└── instructions.md       # optional source for that Profile's instructions
```

`profile.toml` uses ordinary Agent Profile fields, then ends with an
authoring-only `[lenso]` name declaration. It must explicitly declare
`instances` and `allowed_tools`; the compiler adds only the generated Tool
Provider instance.
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

The build publishes a hashed `lenso.agent.deployment@1` resource alongside the
ordinary `lenso.agent.tool-provider@2` Bundle. Inspect it with
`lenso-agent dx check --from dist`; import it explicitly with
`lenso-agent dx apply --from dist`. The Agent stages and resolves the candidate
through its existing Host Profile and Plugin Root before publishing it. The
Provider is installed as disabled in the default Agent; only the generated
Profile selects it. A tools-only contribution stays disabled until the user
selects it through ordinary Agent configuration. This does not launch an Agent
or grant model permission. An App Agent's existing Tool runtime, Profiles and
explicit tool policy own availability and execution authorization. Console needs
neither this support nor an Agent to run.
