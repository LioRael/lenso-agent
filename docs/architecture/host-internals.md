# Agent Harness Host internals

The normal product surface is deliberately small: run the Harness, place Plugin
configuration under `plugins/`, and use `lenso plugins` to manage that directory.
This document describes the machinery behind that surface.

## Public workflow

The Host starts with its compiled defaults when `plugins/` is absent or empty:

```sh
cargo run -p lenso-agent-cli --bin lenso-agent
cargo run -p lenso-agent-cli --bin lenso-agent-cli -- \
  "Summarize this workspace README."
```

App differences are visible files:

```text
plugins/
  lenso.agent.workspace-edit/
    default.toml
    default.disabled
  dev.example.uppercase/
    plugin.lenso-plugin/
    default.toml
    default/
      prompts/system.md
```

Use `lenso plugins list|add|configure|disable|enable|remove`. Use `lenso app
check|show|resolve` to inspect the App derived from the current Host and Plugin
Root. There is no App Definition, enabled-list file, binding document, Module
authoring layer, or separate package verification command.

## Resolution boundary

Each Host build generates an immutable Host Catalog containing:

- linked Plugin Descriptors and factories;
- default Plugin Instances and complete Host-owned configuration;
- root Slots that state which capabilities form the product surface; and
- private attachments needed to distinguish repeated capability providers.

Resolution snapshots the complete Plugin Root, validates strict directory,
TOML, and bounded Instance-resource shapes, verifies any external package bytes, merges package defaults, Host
configuration, and Instance patches, then derives bindings and one complete
Resolved App Plan. The Kernel sees only that Plan and never discovers packages
or files. Resource bytes are carried beside the Plan in the immutable
Generation and never exposed as a mutable Host path.

Missing `plugins/` means an empty Plugin Root, not a different mode. Built-in
and external Plugins follow the same resolver path; the package directory is
omitted only when the Host already supplies the implementation.

## Runtime generations

A long-lived Host never mutates a running graph. A Plugin Root change produces
a candidate Plan. The internal Controller prepares its resources, runs the Ready
Gate, and switches new routes only after success. Existing Turns keep a lease on
their exact Generation until they reach a terminal outcome; retired resources
drain afterward. Invalid configuration, invalid package bytes, or readiness
failure leaves the current Generation unchanged.

Generation, Controller, Supervisor, Receipt, and artifact Store are internal
runtime concepts because they express concurrency, recovery, and evidence. They
do not appear in `plugins/` and are not prerequisites for Plugin development.

## Plugin authoring

An extension author uses one workflow:

```sh
lenso plugin new uppercase
cd uppercase
lenso plugin check
lenso plugin dev --operation execute \
  --request-json '{"name":"uppercase","arguments_json":"{\"text\":\"hello\"}"}'
lenso plugin pack
```

`pack` validates the exact package bytes it creates. `plugins add` validates the
received bytes again before changing the Plugin Root. There is intentionally no
`plugin verify` command and no hand-written Manifest template.

## Configuration and secrets

Package defaults are immutable publisher data. Host configuration expresses
product policy. `plugins/<plugin-id>/<instance>.toml` contains only the App
owner's typed override. Resolution validates the final merged value against the
Plugin schema before boot. Secrets remain environment-backed and never enter
the Plugin Root, Plan, Session events, or diagnostics.

## Operational inspection

`lenso app show` explains selected Plugins and derived bindings; `lenso app
resolve` emits the exact Plan for diagnostics or replay. The Harness accepts an
explicit `--plan` only as an advanced exact-replay escape hatch. Resolved Plans
must not be edited or committed as source configuration.

Durable Sessions record the Generation digest used by each Turn. Provenance
commands may inspect retained Generations and garbage-collection reachability,
but those records never become a second App-authoring authority.

## Invariants

- Plugin is the only public removable behavior unit.
- `plugins/` is the only App-owned composition/configuration surface.
- Host Catalog is immutable and tied to one Host build.
- Resolution is deterministic and fail-closed over a complete snapshot.
- Kernel remains free of discovery, storage, package, and product policy.
- A failed candidate cannot change routing authority.
- External code receives only Plan-bound capabilities and explicit resources.

The rationale and superseded compatibility-era decisions are recorded in
[ADR 0039](../adr/0039-derive-app-from-host-catalog-and-plugin-root.md).
