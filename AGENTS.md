# Agent instructions

Read `CONTEXT.md` and the accepted ADRs before changing architecture or
contracts.

- Treat the Agent Harness as an ordinary Lenso App Composition. Do not add
  Agent concepts, plugin discovery, package installation, or graph mutation to
  `lenso-kernel`.
- Use `Module`, `Capability`, `App Composition`, `Resolved App Plan`, `Runtime
  Driver`, and `Execution Adapter` as the canonical runtime vocabulary.
  `Plugin` is an ecosystem and authoring term only.
- Keep every durable fact with one Module owner. The first Session Module must
  fail closed when its durable store is unavailable; it must not fall back to
  memory.
- Treat native Rust, Bun, and installed package code as trusted. Do not claim
  process or `node:vm` execution is a security sandbox. Untrusted code requires
  a reviewed Wasm or isolated-process Adapter.
- Capability Descriptors and package-local Schemas are the source of truth.
  Native Capability crates own only generated Rust bindings; the supported Bun
  SDK owns generated TypeScript bindings. Never hand-edit either projection.
- Use `/Users/leosouthey/Projects/framework/.lenso-tools/bin/lenso-cargo` for
  local Cargo commands. Public documentation must use portable `cargo`
  commands without local absolute paths.
- Preserve unrelated work. Use `wt switch --create` for task worktrees after
  this repository has a committed base.
