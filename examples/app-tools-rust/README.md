# Rust Tool convention

Run `lenso app build --root examples/app-tools-rust` from this repository.
`app/greeter/agent/tools.rs` uses the existing `plugin`, `tool_provider`, and
`tool` macros. The directory convention lowers that source to portable Wasm and
Process implementations; there is no handwritten generated Host or contract
registration.

Adopt `packages/agent-tool-convention` through `lenso app add` in another App.
The equivalent TypeScript example is `examples/app-tools`. Choose one language
per `agent/` directory. Private Rust dependencies may be declared in an
independent `agent/Cargo.toml`. Rust authoring requires Cargo.

The adjacent `profile.toml` and `instructions.md` compile into an immutable
Agent deployment resource. `lenso-agent dx check --from dist` verifies it;
`lenso-agent dx apply --from dist` stages and validates it against an unmanaged
Agent Home. The provider is disabled for the default Agent and is selected only
by the generated `greeter` Profile, whose `allowed_tools` policy remains
explicit. This example Profile explicitly selects the fixture Model only for its
isolated verification flow; a real App selects its own Model and credentials.

```sh
lenso app build --root examples/app-tools-rust
cargo build -p lenso-agent-cli
python3 examples/app-tools/verify-agent.py \
  --agent-cli target/debug/lenso-agent-cli \
  --app-dist examples/app-tools-rust/dist
```
