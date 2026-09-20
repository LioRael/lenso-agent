# Build an Agent from the Foundation

The Lenso Agent source tree now has three separate starting points. Choose the
one that matches the behavior you want to own:

| Starting point | What it selects | What it deliberately does not select |
| --- | --- | --- |
| Blank Foundation | A product-neutral Host Catalog and an inspectable Plugin Root | Model, Loop, Tool Provider, memory, prompt, or surface |
| Dialogue starter | A deterministic fixture Model, Loop, SQLite Session and Memory, prompt, artifact store, empty Tool runtime, and CLI surface | Workspace, network, coding Tools, messaging channel, or external Model |
| Existing coding distribution | The established coding Profiles and their compatibility behavior | A generic blank composition |

These commands currently run from a source checkout. They do not install a
released Foundation package and they do not modify an existing Agent Home.

## 1. Inspect a blank Foundation

```sh
mkdir support-agent
cargo run -p lenso-agent-foundation -- init --root support-agent
cargo run -p lenso-agent-foundation -- check --root support-agent --json
```

The expected result is a `blank` diagnostic. It means the Foundation has no
hidden product behavior, rather than being a broken coding Agent. The catalog
contains only optional attachment slots. `check` resolves the visible Plugin
Root through public Plan APIs and never starts a runtime or calls a Model.

`init` refuses to replace a different existing Host Catalog. Use a new root or
the Host that already owns that configuration instead of copying Foundation
files into a managed Agent Home.

## 2. Run the explicit dialogue starter

```sh
cargo run -p lenso-agent-dialogue -- plan
cargo run -p lenso-agent-dialogue -- run "Answer directly: selected dialogue starter"
```

The command writes its exact resolved Plan below `.lenso-agent-dialogue/` in
the current directory. It uses a deterministic fixture Model so it is an
executable composition proof, not a recommendation for a production Model.

## 3. Contribute local Agent behavior with `agent/`

An App can keep business code next to its Agent contribution:

```text
app/
├── orders/agent/tools.ts
├── billing/agent/tools.rs
└── assistant/agent/
    ├── profile.toml
    └── instructions.md
```

The Engine makes each selected contribution explicit. `tools.ts` and
`tools.rs` become Tool contributions; `profile.toml` plus `instructions.md`
becomes a Profile-only resource. A directory neither inherits another
directory's Tools nor grants its credentials, instructions, Profile, or
authority. A Profile-only contribution creates no fake Tool Plugin.

Use the checked fixture as a source-local reference:

```sh
python3 examples/app-agent-composition/verify-foundation.py \
  --engine-host ../lenso-engine/target/debug/lenso-engine-host \
  --agent-cli target/debug/lenso-agent-cli
```

It proves a Bun Tool, a Rust Tool, and a Profile-only contribution through
Engine discovery, published resources, `dx check`, and `dx apply`.

## 4. Use the full Plugin and Capability path when a Tool is not enough

`agent/` is a convenience convention. A Plugin can own a new versioned
Capability and a related Provider/Consumer graph without disguising it as a
Tool. The external Task Board fixture demonstrates the public Rust path:

```sh
python3 examples/external-task-board-capability/verify-foundation.py \
  --engine-host ../lenso-engine/target/debug/lenso-engine-host
```

It authorizes three explicit Plugin identities, two related Capabilities from
one Plugin, public Host bindings, configuration validation, cancellation,
domain errors, and lifecycle shutdown. The verifier uses sibling source patches
only in a temporary copied App. It is source-local evidence; released-package
installation remains a separate qualification.

## 5. Add a narrow extension instead of replacing the whole Agent

Use the public Capability that matches the behavior being changed. These are
ordinary versioned source-first contracts; an author does not edit the default
Loop, a generated binding, or a Host-private registry.

| Need | Capability | Contract boundary |
| --- | --- | --- |
| Project the model-visible context, change Tool arguments, or redact a model-visible Tool result | `lenso.agent.turn-processing@1` | Processors run in resolved Plan order. Tool arguments are normalized and schema-validated before final approval; result projection cannot alter the immutable Tool fact. |
| Persist Plugin-owned facts associated with a Session, Run, or Task | `lenso.agent.extension-state@1` | The selected Provider derives owner identity. Safe presentation is bounded data; unknown recovery-required state blocks recovery. |
| Build a real-time, non-chat surface | `lenso.agent.interaction@1` | A Provider owns a bounded duplex stream with independent sequence spaces, half-close, interruption, cancellation, and explicit resume support or rejection. |
| Pick authorized Tools for one tenant or phase | `lenso.agent.dynamic-authority@1` | A Host seals an exact resource snapshot and the Tools runtime revalidates it just before a side effect. Dynamic selection never installs new Plugins. |
| Persist an approval or long-running task | `lenso.agent.durable-task@1` | Persist serializable state and explicit recovery steps. Duplicate/stale signals are safe; unknown versions and uncertain effects block an automatic retry. |

The fixtures show the public authoring route and the security properties that
the small contracts preserve:

```sh
python3 examples/external-agent-processing/verify-foundation.py \
  --framework-root /path/to/framework \
  --agent-root /path/to/lenso-agent
python3 examples/external-agent-durable-state/verify-foundation.py \
  --framework-root /path/to/framework \
  --agent-root /path/to/lenso-agent
python3 examples/external-agent-interaction/verify-foundation.py \
  --framework-root /path/to/framework \
  --agent-root /path/to/lenso-agent
python3 examples/external-agent-dynamic-authority/verify-foundation.py \
  --framework-root /path/to/framework \
  --agent-root /path/to/lenso-agent
```

Each script is a source-local proof: it creates a clean temporary consumer but
points that consumer to sibling source packages. It is useful while authoring,
but it is not an installation or target-distribution result.

## 6. Diagnose composition before starting it

The Foundation check remains read-only. It exposes the selected Plugin source,
runtime package revision, entrypoint, execution profile, exact Capability
provider selection, and the Foundation's authorization boundary:

```sh
cargo run -p lenso-agent-foundation -- check --root support-agent --json
```

`plugin_sources` tells whether each selected Plugin came from the visible
Plugin Root or a Host default. `capability_selections` records the named
requirement, exact descriptor version, provider, and deterministic order.
`authorization_boundary` always states that a blank Foundation granted no
runtime authority. Use the selected execution Host to diagnose credentials,
admission, target compatibility, and actual invocation policy.

See [the Foundation audit](../architecture/agent-foundation-audit.md) for the
full V01–V10 matrix, including source/local evidence, target limitations, and
the still-required released-artifact qualification.
