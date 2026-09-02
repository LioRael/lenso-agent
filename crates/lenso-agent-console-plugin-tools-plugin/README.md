# Console Plugin Tools Plugin

`lenso.agent.console-plugin-tools` gives the Console Agent a bounded, model-facing consumer of one explicitly identified Agent's Plugin management target.

## Owned behavior

- `inspect_app` reports the target Agent's current desired Plugin Root revision and resolved size.
- `list_plugins` lists the Plugins visible to the target Agent.
- `inspect_plugin` reports one Plugin's Instances, selection, origin, configuration size, and source digest without sending raw configuration into model context.
- `check_plugin_change` validates an exact configuration candidate without changing state.
- `apply_plugin_change` publishes only a candidate whose expected revision and proposal digest still match.
- `set_plugin_enabled` directly enables or disables one exact Plugin Instance through the target Agent's selected selection authority.

The Plugin does not own Plugin configuration facts, target discovery, authority selection, runtime activation, Console HTTP contracts, or mutation policy. It requires the Host-private `lenso.agent.plugin-management-target@1`; the Host bridge routes an explicit `agent_id` and delegates to the authority owned by that Agent. Missing or unsupported targets fail closed. A successful publication means desired state was committed; it does not claim that a new Generation is active.

This internal authority role is deliberately distinct from Console's cross-Agent `lenso.agent.plugin-configuration@1`, which owns the broader management, history, rollback, publication-operation, and HTTP projection consumed by the Console UI.

## Boundary

The target authority can be the built-in local Plugin Root, SQLite, remote HTTP authority, or an injected custom implementation. Every request and response carries the target Agent identity. Responses include the authority kind and reference, are bounded, and never include raw stored configuration. Candidate configurations remain capped at 7 KiB so the complete write call fits the approval preview; read operations are parallel-safe, and publication is exclusive.

The Console inventory pairs `apply_plugin_change` and `set_plugin_enabled` with the interactive approval hook. Inspection and candidate validation are allowed without approval; each mutation asks the user to approve the exact Tool call once.

## Removal proof

The Host default is disableable. Creating `plugins/lenso.agent.console-plugin-tools/default.disabled` removes all six Tools while preserving the Console Agent App and its Web surface.

Run the focused checks with standard Cargo commands:

```sh
cargo test -p lenso-agent-console-plugin-tools-plugin
cargo test -p lenso-agent-web minimal_console_plugin_inventory_reaches_readiness_and_shuts_down --lib
cargo test -p lenso-agent-web removing_console_plugin_tools_preserves_the_console_agent_app --lib
```
