# Rust Tool convention

Run `lenso app build --root examples/app-tools-rust` from this repository.
`app/greeter/agent/tools.rs` uses the existing `plugin`, `tool_provider`, and
`tool` macros. The directory convention creates a linked native provider;
there is no handwritten generated Host or contract registration.

Adopt `packages/agent-tool-convention` through `lenso app add` in another App.
The equivalent TypeScript example is `examples/app-tools`. Choose one language
per `agent/` directory. Private Rust dependencies may be declared in an
independent `agent/Cargo.toml`. Rust authoring requires Cargo.

The provider implements the existing Agent Tool contract. It does not start an
Agent or grant execution. The consuming App Agent must admit the provider under
its existing Profile and tool policy. These examples verify authoring and App
assembly, not an automatically configured model session.
