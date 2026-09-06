# SQLite coding Profile import acceptance

The built-in SQLite authority can import the official `plan`, `code`, and
`code-sandbox` Profiles while Agent Web continues running. This is an import of
the compiled official preset, not arbitrary Profile or package upload.

## Verified behavior

- The CLI reads the live Plugin Root revision and process stream, then uses the
  authenticated `POST /control/profiles/import` operation.
- Import validates Normal and all three named Profile resolutions in a staged
  Home. It preserves customized files by rejecting their replacement and keeps
  existing enabled Instances enabled.
- SQLite journals exact prior/candidate bytes before materialization, advances
  its desired revision with CAS, and restores an interrupted publication before
  startup inspection. Unexplained edits fail closed before partial undo.
- Repeated import retains the same semantic revision. Stale revisions, stale
  process streams, and missing control authorization are rejected.
- The live Host switches Plan → Code → Normal without restart. A restart with
  `--profile plan` succeeds against the same SQLite database.
- A missing Code executable fails the Ready Gate and leaves the current Plan
  active. Concurrent import and selection complete without blocking the actor;
  selection may return the existing retryable authority-busy conflict.
- Configuration publication while Plan is selected returns that Profile's
  actual desired Plan digest. The authoring dependency is pinned to the released
  minimum `lenso-cli 0.5.2`, which preserves candidate configuration identity.
- Offline `profiles install coding` remains rejected in managed Homes.

## Validation

```sh
cargo test --locked -p lenso-agent-web -p lenso-agent-cli -p lenso-agent-host
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

CLI and Host regression suites passed; the final Web run passed 88 library
unit tests, 7 configuration-service binary tests, 9 existing Web HTTP tests,
the configuration-service integration test, and the normal reconciler benchmark
check. Existing explicitly ignored tests remain ignored.

A separate local process smoke used built Agent Web and CLI binaries, an
isolated SQLite Agent Home, and a separate Git Workspace. Real CLI imports,
Profile switches, restart inspection, and the offline guard passed. No user
Home, credentials, or running Console instance were changed.

Import success means definitions were published. Activation is a separate
Ready Gate result. Remote/injected authority import and arbitrary custom
Profile import are not supported by this operation.
