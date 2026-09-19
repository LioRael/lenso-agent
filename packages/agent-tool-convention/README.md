# Optional App Tool convention

Adopt this local support package with `lenso app add <package-directory>`.
A selected `agent/` contains one `tools.ts` or `tools.rs`. Private dependencies
belong to `agent/package.json` or `agent/Cargo.toml`; inactive surfaces are not
installed or compiled. TypeScript exports one `tools([...])` declaration from
`@lenso/agent-tool-sdk`. Rust uses the existing Plugin and `tool_provider` macros.

The output is an ordinary `lenso.agent.tool-provider@2` provider. This does not
launch an Agent or grant model permission. An App Agent's existing Tool runtime,
Profiles and explicit tool policy own availability and execution authorization.
Console needs neither this support nor an Agent to run.
