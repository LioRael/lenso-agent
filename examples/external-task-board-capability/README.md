# External Task Board Capability

This is a source-local proof that an independent package can own an Agent-adjacent
Capability without an official Agent Plugin ID, a central role enum, or private
Agent implementation code.

The source declares three portable Capabilities:

- `external.task-board@1` is provided by `example.external-task-board-provider`;
- `external.task-board-report@1` and `external.task-board-audit@1` are both
  provided by `example.external-task-board-report`; and
- `example.external-task-board-caller` consumes the report and audit contracts.

The third contract is authored directly in `contracts/external.task-board-audit/src/contract.rs`.
Its descriptor, schemas, and Rust projection are synchronized by the Engine; do
not edit `src/generated.rs` manually.

The fixture tests configuration validation, invalid Host binding rejection,
Capability invocation, a preserved domain error, cancellation, and clean
Plugin deactivation. Run the independent Native consumer using published
crates.io artifacts:

```sh
python3 examples/external-task-board-capability/verify-registry.py
```

This verifier copies only the external App into a temporary directory, uses
the committed `runner/Cargo.lock` with `--locked`, rejects non-registry Lenso
dependencies, and runs the same Provider/Consumer lifecycle tests. It prints
the exact package versions and sources. Python 3.9 or later is required.
This is trusted-native registry qualification; it does not qualify the newly
added Agent contracts, Host imports in another Adapter, or an isolation target.

To refresh the fixture lock after an intentional dependency update, use
`--update-locks`. The verifier generates the lock in its isolated copy, retains
the registry-only admission checks and locked test, and writes the lock back
only after that test succeeds. Ordinary CI uses the committed lock without
this flag.

`verify-foundation.py` is the historical source verifier for the retired split
repository layout. It remains available for that baseline; it is outside the
current DX and registry-consumer qualification. The current source App workflow
is exercised by the Examples owner's `fixtures/ops-reference/` guide.
