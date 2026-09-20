# App Agent composition

This App deliberately combines three independently discovered `agent/` entries:

- `orders/agent/tools.ts` becomes a Bun Tool Provider Bundle;
- `billing/agent/tools.rs` becomes portable Rust Tool Provider Bundles; and
- `assistant/agent/profile.toml` plus `instructions.md` becomes only an
  Agent Profile resource.

The Profile is explicit about its selected existing Model instance and its empty
Tool allowlist. It does not acquire either local Tool merely by sharing an
`app/` directory.

Build with an Engine host that includes resource-only convention output support:

```sh
lenso-engine-host app build --root examples/app-agent-composition
cargo build -p lenso-agent-cli
LENSO_AGENT_HOME=\"$PWD/.agent-home\" target/debug/lenso-agent-cli dx check \
  --from examples/app-agent-composition/dist
```

For the full integration proof, the verifier copies the App into a temporary
directory outside this repository before it builds. Supply the Engine host
binary and the Agent CLI that you built from their respective source trees:

```sh
python3 examples/app-agent-composition/verify-foundation.py \
  --engine-host ../lenso-engine/target/debug/lenso-engine-host \
  --agent-cli target/debug/lenso-agent-cli
```

It verifies that the resulting App resource inventory has two
`lenso.agent.deployment@1` Tool resources and one
`lenso.agent.deployment@2` Profile-only resource. Only the two Tool resources
have matching Bundles. `dx apply` then imports those Tool Plugins disabled and
publishes the Profile without creating a Plugin Root directory for the
Profile-only contribution.
