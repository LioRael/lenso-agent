# Lenso Agent context

Lenso Agent runs as one Host with compiled defaults and a visible global Agent
Home.
Its Plugin Root is `~/.lenso/agent/plugins/` by default, or
`$LENSO_AGENT_HOME/plugins/` when explicitly overridden. A user adds or
configures behavior as a Plugin. The process current directory is the
Workspace, not App configuration or runtime state. There is no separate Module
authoring model and no central App Definition.

## Public workflow

Plugin authors use:

```text
lenso plugin new -> check -> dev -> pack
```

App owners use:

```text
lenso plugins list|add|configure|disable|enable|remove
lenso app check|show
lenso run
```

`pack` verifies the bytes it writes and the Host verifies received packages;
there is no separate `plugin verify` command.

App differences are ordinary files below the Agent Home:

```text
plugins/<plugin-id>/plugin.lenso-plugin/
plugins/<plugin-id>/<instance>.toml
plugins/<plugin-id>/<instance>/**
plugins/<plugin-id>/<instance>.disabled
```

The package directory is required only for an external Plugin. A Plugin linked
into the Host needs only its Instance configuration. An empty or absent Plugin
Root means the exact Host defaults.

## Resolution boundary

The executable exposes an immutable generated Host Catalog containing:

- linked Plugin Descriptors;
- root Slots and their cardinality/replacement policy;
- default Plugin Instances and Host-owned configuration; and
- private Slot/Instance attachments needed to disambiguate repeated
  Capability providers.

The Plugin Root contains no binding decisions. Resolution is pure:

```text
Host Catalog + Plugin Root snapshot -> Resolved App Plan
```

The Plan is complete, deterministic runtime input but is not a user-authored
document. Kernel receives only this Plan and never scans packages or files.

## Runtime boundary

Generation control remains above Kernel. A candidate Plugin Root is snapshotted
and resolved, its complete Generation is staged behind the Ready Gate, and only
then may a long-lived surface route new Turns to it. Existing Turns retain their
Generation lease. Controller state, recovery records, receipts, and artifact
cache are derived Host internals, not an App authoring model.
Controller lineages and immutable Generation records share one private Host
runtime ledger; surfaces select only a semantic kind and never own its physical
storage layout. Operating-system process fences remain private runtime leases.

The Host currently owns terminal, Telegram, Discord, Model, Tool, Prompt,
Session, approval, workspace, process, and other Agent Plugins. Capabilities
remain explicit typed contracts. Native factories are Host availability, not
activation; the resolved Plan selects exact Plugin Instances.

## Authority rules

- `LENSO_AGENT_HOME` must be an absolute UTF-8 path and is resolved before boot.
- Plugin configuration is non-secret and Schema validated before boot.
- Secrets remain in environment or provider-owned credential storage.
- Tool providers retain final authorization for filesystem, process, and
  network effects.
- Native code is trusted. Reviewed Wasm profiles provide the isolated external
  extension path; JavaScript realms and child processes are not described as a
  hostile-code sandbox.
- No global registry, live graph mutation, fallback provider, or package
  discovery belongs in Kernel.
- Failed candidate resolution or readiness preserves the current Generation
  and the previous Plugin Root bytes.

## Retired concepts

The following are migration inputs only and must not reappear in normal docs or
commands:

- `lenso.app.json` and `lenso.app.toml`;
- `lenso.local.toml` and central enabled lists;
- public Module packages, descriptors, macros, or configuration directories;
- user-authored binding decisions;
- `plugin verify`; and
- Store, Receipt, Active Set, Controller, Supervisor, or Generation as concepts
  the ordinary Plugin user must learn.
