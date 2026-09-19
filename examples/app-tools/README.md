
## Real App Agent policy proof

After building this example, build the local Agent CLI and run:

```sh
cargo build -p lenso-agent-cli
python3 examples/app-tools/verify-agent.py \
  --agent-cli target/debug/lenso-agent-cli \
  --app-dist examples/app-tools/dist
```

The script creates an isolated Agent Home, installs the compiled convention
provider into its visible Plugin Root, and uses the deterministic fixture Model.
It verifies `greet` is visible and callable under the explicit allowlist, that
an out-of-scope model call is rejected before execution, and that disabling the
provider removes availability. It checks persisted Session evidence; it neither
calls an external model nor sends messages outside the process.

This proof found and fixes missing Bun V2 artifact/codec admission in Agent's
catalog factory and a nested-Tokio startup RPC defect in the Bun Adapter. The
Cargo patch pins the reviewed Adapter fix by immutable Git revision until its
registry release is available. A clean checkout needs no sibling worktree.
