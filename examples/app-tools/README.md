
## App Agent DX policy proof

Build this App with the Engine CLI, then inspect and explicitly import its Agent
contribution. The App build publishes both the Bun Tool Provider Bundle and a
hashed Agent deployment resource; it does not touch an Agent Home.

```sh
lenso app build --root examples/app-tools
cargo build -p lenso-agent-cli
LENSO_AGENT_HOME="$PWD/.agent-home" target/debug/lenso-agent-cli dx check \
  --from examples/app-tools/dist
python3 examples/app-tools/verify-agent.py \
  --agent-cli target/debug/lenso-agent-cli \
  --app-dist examples/app-tools/dist
```

The script creates an isolated Agent Home and configures the deterministic
fixture Model before it imports the App resource through `dx apply`. It proves
that the default Agent cannot see the Tool. Only the generated `greeter` Profile
selects the provider and grants `greet`; an out-of-scope model call is rejected
before execution. It also checks persisted Session evidence and proves that
removing the provider makes the Profile invalid. It neither calls an external
model nor sends messages outside the process.

This proof found and fixes missing Bun V2 artifact/codec admission in Agent's
catalog factory and a nested-Tokio startup RPC defect in the Bun Adapter. The
Host now requires the published Bun Adapter 0.1.11 containing that fix.
A clean checkout needs no sibling worktree or Adapter Git patch.
