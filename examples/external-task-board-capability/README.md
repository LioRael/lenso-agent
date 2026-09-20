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

The fixture proves configuration validation, invalid Host binding rejection,
Capability invocation, a preserved domain error, cancellation, and clean
Plugin deactivation. `verify-foundation.py` copies the fixture outside this
repository, uses source patches only in that copy, assembles a local Native
Host, checks its lifecycle, and runs the public-plan test suite.

Build the Engine host first, then run the verifier from this repository:

```sh
cargo build -p lenso-engine-host --manifest-path ../lenso-engine/Cargo.toml
python3 examples/external-task-board-capability/verify-foundation.py \
  --engine-host ../lenso-engine/target/debug/lenso-engine-host
```

Pass `--framework-root` when the Lenso sibling repositories are not discovered
from the fixture's ancestors. The source manifests deliberately name released
crate versions. The verifier replaces those versions with sibling source paths
only in its temporary copy, so this is **not** evidence that released packages
or a clean consumer installation have been qualified. That remains the V10
package-boundary proof.
