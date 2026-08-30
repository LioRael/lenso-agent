# Agent instructions

Read `CONTEXT.md` and the accepted ADRs before changing architecture or
contracts.

- Treat Lenso Agent as one Host plus a visible `plugins/` Plugin Root. Do
  not add Agent concepts, Plugin discovery, package installation, or graph
  mutation to `lenso-kernel`.
- Use `Plugin` for every removable behavior unit. `Module` is retired public
  vocabulary and must not be restored as a compatibility alias.
- Keep every durable fact with one Plugin owner. The first Session Plugin must
  fail closed when its durable store is unavailable; it must not fall back to
  memory.
- Treat native Rust, Bun, and installed package code as trusted. Do not claim
  process or `node:vm` execution is a security sandbox. Untrusted code requires
  a reviewed Wasm or isolated-process Adapter.
- For a source-first Capability crate, its annotated Rust contract is the
  authoring source; committed Descriptors and package-local Schemas are locked
  cross-language artifacts and generated Rust/TypeScript bindings must not be
  hand-edited. Unmigrated Capability crates still own their Descriptor and
  Schemas directly. For Plugins already migrated to source-first authoring, do
  not restore hand-written factories, endpoint glue, Schemas, or Host
  registration.
- Host defaults and private wiring are generated into the immutable Host
  Catalog. App differences live only under `plugins/<plugin-id>/`; never add a
  central App Definition or local enabled list. Never hand-edit a derived Plan.
- Use `/Users/leosouthey/Projects/framework/.lenso-tools/bin/lenso-cargo` for
  local Cargo commands. Public documentation must use portable `cargo`
  commands without local absolute paths.
- Preserve unrelated work. Use `wt switch --create` for task worktrees after
  this repository has a committed base.
