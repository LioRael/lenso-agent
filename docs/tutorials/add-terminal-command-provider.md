# Add one Plugin command to the CLI and TUI

Use this path when a feature already owns useful behavior through a Capability
and should expose the same operation in terminal products. Do not put Clap or
Ratatui in the feature Plugin.

The complete reference implementation is
[`lenso-agent-session-terminal-plugin`](../../crates/lenso-agent-session-terminal-plugin/src/lib.rs).
It contributes two commands while all durable facts remain behind
`lenso.agent.session@1`.

## 1. Declare the provider package

Add the generic provider contract and the feature Capability to the Plugin:

```toml
[package.metadata.lenso]
plugin-id = "example.project-terminal"
root-slot = "terminal-command-providers"

[dependencies]
lenso.workspace = true
lenso-capability-terminal-command-provider.workspace = true
example-project-capability = { path = "../example-project-capability" }
```

The root slot is Host authoring policy. The Capability is the reusable contract;
another Host may assign its provider Plugins to a differently named slot.

## 2. Advertise semantic command metadata

Implement `lenso.terminal.command-provider@1` and return a bounded catalog. One
definition contains:

- a globally stable provider-owned ID such as `example.project.status`;
- a nested surface path such as `project status`;
- summary and help text;
- positional, option, or flag parameters; and
- one or both of the `text` and `json` output formats.

Call `validate_catalog` in a unit test. The aggregate repeats validation during
activation and additionally rejects conflicts across providers. A command path
cannot also be a parent group: `project` and `project status` may not both be
leaf commands.

A TUI also rejects a command root that shadows one of its local controls.
Choose a provider-owned namespace that remains unambiguous in every selected
surface.

`--json` belongs to the CLI adapter and is reserved. Providers receive the
selected format separately from their JSON argument object.

## 3. Execute through the feature Capability

Deserialize `ExecuteOpen.arguments_json` with `deny_unknown_fields`, validate
feature-specific limits, and invoke the existing feature Port with the supplied
`InvocationContext`. Returning text or JSON is presentation; authorization and
durable state stay with the feature provider.

Execution is a stream, even for a one-message result. This lets the same command
emit progress, stdout, stderr, and a final result, and lets the CLI or TUI cancel
it. Bound every message and any provider-owned total output. Do not spawn an OS
process unless the feature already owns an explicit Process or Execution Adapter
boundary.

## 4. Compose, do not register

Link the feature provider factory into the Host build and select its Plugin
Instance under the Plugin Root. The App also needs:

- one `lenso.terminal.command` aggregate Instance;
- a `lenso.terminal.cli` consumer for each CLI surface; or
- a `lenso.terminal.tui` consumer for each TUI surface.

There is no mutable command registry. The resolved Plan binds providers to the
aggregate and the aggregate to each consumer. Invalid catalogs reject readiness.

Multiple CLIs are ordinary multiple consumer Instances. An embedded Agent Host
can call `lease_terminal("lenso.terminal.cli/admin")` and
`lease_terminal("lenso.terminal.cli/developer")`; each returned catalog and its
execution stream remain pinned to one immutable Generation.

## 5. Verify both surfaces

At minimum, prove:

1. provider-local catalog validation;
2. aggregate duplicate and prefix rejection;
3. CLI nested help and argv-to-JSON parsing;
4. text and JSON execution from a fresh state root;
5. TUI suggestion projection and streamed transcript output; and
6. deletion: remove the provider Instance and confirm the App remains valid
   while only its command paths disappear.

For the reference slice:

```sh
lenso-agent-cli sessions list
lenso-agent-cli sessions list --json
lenso-agent-cli sessions show --help
```

In the TUI, the same catalog appears as `/sessions list` and
`/sessions show `. Local Shell controls such as `/clear` remain surface-owned
because they do not project a feature Capability.
