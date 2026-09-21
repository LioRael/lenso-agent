# Released execution target qualification

```sh
rustup target add wasm32-unknown-unknown
python3 examples/external-agent-targets/verify.py
```

The verifier copies this consumer outside the repository, checks every Cargo
dependency comes from crates.io (apart from these fixture crates), and uses
committed lockfiles. `--update-locks` explicitly refreshes those lockfiles.
It executes compiled Wasm Components through released
`lenso-wasm-component-adapter = 0.2.12` and `lenso-guest-sdk = 0.5.0`.

The guest reaches a real bound Host request and event endpoint. Raw forged
binding IDs, undeclared operations, and unsupported interaction kinds are
rejected by Host imports even when the guest bypasses the SDK. Host-side
controls demonstrate that a local file, environment variable, and listening
TCP socket are available; the guest cannot access them. No WASI imports or
native fallback are provided.

Additional tests cover real streams, half-close, declared errors, cancellation,
descriptor extraction, and source package assembly. A complete bound Plan
using authoring v2 imports is rejected with the precise unsupported-profile
error before loading an artifact or activating guest code. Dependency-free v2
and v1 imports are supported by this released Adapter.

This qualifies the exercised Wasm profile, not every Agent Capability on every
target. Durable Task remains native-only (`portable = false`). Native and Bun
Plugins remain trusted code. This is a regression qualification, not a general
security audit of Wasmtime.

The baseline tests and guests are adapted from Lenso Runtime Rust's
`lenso-wasm-component-adapter` tests (MIT OR Apache-2.0), with published
dependencies and explicit authorization/isolation assertions added here.
