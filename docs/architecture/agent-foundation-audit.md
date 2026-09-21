# Agent foundation extension audit

Status: source-local implementation, 2026-09-21

This record maps the Agent Foundation implementation specification to code and
reproducible evidence. It separates a source-local proof from target
qualification and released-package installation. It does not claim a registry
publication, a managed import, a clean consumer installation, or production
readiness.

## What an author can use now

The source tree has three intentionally different entry points:

| Path | Author goal | Authority and boundary |
| --- | --- | --- |
| `lenso-agent-foundation` | Start with no selected product behavior and inspect an explicit Plugin Root. | It only resolves. It has no linked defaults and grants no runtime or external-effect authority. |
| `lenso-agent-dialogue` | Run a small, explicit non-coding dialogue composition. | Its deterministic fixture Model and local stores are selected by that starter only. |
| `agent/` convention | Add nearby Bun/Rust Tools or a Profile-only contribution to an App. | Engine discovers declared files; Agent `dx check` and `dx apply` keep compilation, import, enablement, and authorization separate. |

Advanced authors use ordinary versioned Capabilities and Plugins. The
Foundation does not require a new central Agent role or a default Loop to make
that graph work.

`lenso-agent-foundation check --json` is a read-only composition diagnostic.
Alongside the concise instance/binding lists it now reports:

- `plugin_sources`: Plugin Root versus Host-default provenance, selected runtime
  package identity/revision, entrypoint, runtime profile, and execution class;
- `capability_selections`: named requirement, exact descriptor version, provider
  instance, deterministic many-provider order, and whether a persisted choice
  was present; and
- `authorization_boundary`: an explicit statement that the blank Foundation
  grants no authority and that the selected execution Host owns runtime
  bindings, credentials, and invocations.

The output never exposes Plugin configuration or credentials, and it never
chooses a Provider, changes a Plan, or starts a Host.

## Implemented boundaries

| Spec area | Current source boundary | Evidence and limit |
| --- | --- | --- |
| AG-01 / V01 | [Foundation](../../apps/lenso-agent-foundation) contains only many-cardinality attachment slots. [Dialogue starter](../../apps/lenso-agent-dialogue) selects its own fixture composition. | The blank check does not link a Model, Loop, Tool, memory, prompt, or Surface. The starter is an executable fixture, not a production default. |
| AG-02 | Public turn/profile/model facts live with the Agent Capability owners. The legacy Loop keeps compatibility re-exports only. | A third-party Loop can depend on public types without depending on `lenso-agent-loop-plugin`. |
| AG-03 / V03 | The Task Board fixture uses three external Plugin identities and two Capability contracts owned by the same external Plugin. | Native source-local assembly, configuration, cancellation, domain error, and shutdown are covered. It is not a released artifact test. |
| AG-04 / V02 | The `agent/` convention supports Bun Tools, Rust Tools, and Profile-only resources. | A Profile-only contribution publishes no fake Tool Plugin and directory proximity supplies no authority. |
| AG-05 / V04 | [`lenso.agent.turn-processing@1`](../../crates/lenso-capability-agent-turn-processing) provides ordered model-request projection, Tool-argument transformation, and Tool-result presentation projection. | Tool arguments are canonicalized and schema-validated after every transformation; final approval sees the transformed value. Factual Tool outcomes remain immutable while model presentation is projected with trace digests. |
| AG-06 / V05 | [`lenso.agent.extension-state@1`](../../crates/lenso-capability-agent-extension-state) stores owner-derived Plugin facts associated with a Session, Run, or Task. | Namespace/schema/version/sequence/idempotency are bounded. Unknown recovery-required state blocks recovery; safe presentation is data, never a module-loading instruction. |
| AG-07 / V07 | [`lenso.agent.interaction@1`](../../crates/lenso-capability-agent-interaction) is a separate bounded duplex interaction Capability; compatible `run_turn` remains unchanged. | Client and Agent sequence spaces are independent. Half-close, interrupt, bounded pending frames, cancellation, and explicit unsupported resume are part of the contract. |
| AG-08 / V08 | [`lenso.agent.dynamic-authority@1`](../../crates/lenso-capability-agent-dynamic-authority) selects precise Resource identities and revalidates a Host-sealed Tool grant immediately before execution. | Normal caller JSON cannot forge the sealed grant. An absent grant leaves the static Plan path intact; this first integration is deliberately the Tool runtime, not a dynamic Plugin installer. |
| AG-09 / V05–V06 | [`lenso.agent.durable-task@1`](../../crates/lenso-capability-agent-durable-task) exposes serializable start/observe/signal/cancel/recover/wait operations. | State is owner-derived and serializable. Stale signals, duplicate signals, unknown versions, and uncertain external effects block unsafe recovery. The Capability is intentionally `portable = false`; no cross-target promise follows from native source tests. |
| AG-10 | Target capability profiles and the owning Adapter/CLI preflight are separate from Agent Plan resolution. | No Agent target qualification is claimed here. A target must prove its own lifecycle, transport, resource, and authorization behavior; a profile declaration alone is not readiness. |
| AG-11 | Tutorial, external fixtures, Foundation diagnostics, and Console's generic unavailable-service behavior form the author-facing surface. | Console renders a fixed safe fallback before loading an unavailable Workspace module. Typed task pages remain Plugin-owned and must use declared services. |

## Reproducible external-consumer evidence

Source verifiers copy external fixtures to temporary directories and patch only
those copies. Registry and target verifiers instead require published packages
and reject Git or sibling dependencies. Both exercise public contracts.

| Matrix item | State | Reproduction |
| --- | --- | --- |
| V01 blank Foundation and explicit dialogue | **Source-local pass** | `cargo test -p lenso-agent-foundation`, `cargo test -p lenso-agent-dialogue-starter`, and `cargo test -p lenso-agent-dialogue` |
| V02 Bun Tool + Rust Tool + Profile-only contribution | **Source-local external pass** | [app-agent-composition](../../examples/app-agent-composition/verify-foundation.py) with a supplied Engine Host and Agent CLI |
| V03 external Capability Provider + Consumer | **Source-local and registry native external pass** | [source verifier](../../examples/external-task-board-capability/verify-foundation.py) with an Engine Host; [registry verifier](../../examples/external-task-board-capability/verify-registry.py) uses a committed lockfile, only crates.io framework dependencies, and no source patches |
| V04 external Loop plus ordered processors | **Source-local external pass** | [external-agent-processing](../../examples/external-agent-processing/verify-foundation.py) |
| V05 extension state, safe unknown presentation | **Source-local external pass** | [external-agent-durable-state](../../examples/external-agent-durable-state/verify-foundation.py) |
| V06 restartable approval task | **Source-local external pass** | [external-agent-durable-state](../../examples/external-agent-durable-state/verify-foundation.py) uses separate operating-system processes for start, recover, signal, and recovery-after-signal |
| V07 bounded duplex interaction | **Source-local external pass** | [external-agent-interaction](../../examples/external-agent-interaction/verify-foundation.py) |
| V08 dynamic tenant Tool authority | **Source-local external pass** | [external-agent-dynamic-authority](../../examples/external-agent-dynamic-authority/verify-foundation.py) |
| V09 disable/remove, active-work drain, and upgrade | **Native lifecycle and distinct-binary upgrade pass** | The durable-state fixture closes admission, rejects retained handles, settles an active stream with `Unavailable`, releases its lease and shuts down cleanly. A new composition omits both Plugins. Durable bytes survive removal; restart can inspect them, incompatible state blocks recovery, and stale approval cannot resume it. The [binary upgrade verifier](../../examples/external-agent-durable-state/verify-binary-upgrade.py) builds and hashes three different Provider executables against published packages; a compatible upgrade preserves state byte-for-byte, while an incompatible one rejects caller-claimed compatibility and invalidates old approvals. |
| AG-10 real imports, isolation and unsupported interactions | **Released Wasm profile pass** | Bound Host requests/events succeed; raw forged bindings, undeclared operations and interaction mismatches fail. Host file/environment/network access is unavailable in the guest. Unsupported authoring v2 imports fail before artifact loading. Streams and cancellation run through the real Adapter. |
| V10 exact artifacts in a clean consumer | **Published Foundation SDK and Wasm target consumers pass** | All six Foundation SDKs are published as `0.1.0`; the [SDK verifier](../../scripts/verify-foundation-sdk-registry.py) consumes them outside the checkout. The [target verifier](../../examples/external-agent-targets/verify.py) uses locked published Adapter/Guest SDK dependencies and real compiled Wasm guests. |

Run V04–V08 from an Agent checkout with explicit source roots:

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

## Current limits and next qualification

The 2026-09-21 closeout passed `cargo fmt --all -- --check`,
`cargo clippy --locked --workspace --all-targets -- -D warnings`,
`cargo test --locked --workspace --all-targets`, the generated-contract check,
and the five Agent convention tests. Engine workspace tests, formatting, and
Clippy also passed. Console's changed files passed formatting/lint; its local
typecheck and five contribution-outlet browser tests passed.

Final dynamic authorization now runs after approval Hooks complete, including
for streamed execution. The external tenant-policy fixture revokes permission
inside approval, verifies that both invocation forms reject the call without
writing another effect, and checks that started Hooks still settle on denial.

The external durable-state fixture now checks upgrade-state revision changes:
the first incompatible recovery increments the revision, while repeated checks
leave it stable. Its removal policy preserves durable facts and cancels live
transport work; removal never implies rollback of an external effect.
The child-policy phase also proves parent cancellation propagates through
children and grandchildren, persists across restart, and rejects late approval.
Expired deadlines are settled on admission/read and cannot resume a task.

All six Foundation SDK crates are published at `0.1.0`. The registry-only
consumer checks their sources and runs the full durable lifecycle fixture.
The binary-upgrade verifier installs separately built Provider versions 1, 2,
and 3 as independent executables and verifies different SHA-256 digests.
Version 2 retains v1 state and uncertain-effect facts across repeated process
restarts. Version 3 supports only state v2: even a caller claiming v1 support
cannot make it resume v1 state. The original payload remains inspectable;
recovery marks the task upgrade-required and rejects stale approval.
This is native Provider replacement in an external Host, not an Agent package
manager hot-upgrade or automatic schema migration.

The isolated target qualification uses published Wasm Component Adapter 0.2.12
and Guest SDK 0.5.0. It tests the supported v1 Host-import profile and
v2 dependency-free profile; v2 imports are explicitly rejected. There is no
native fallback. This closes the required single-target execution proof without
claiming all SDKs are portable or depending on unpublished Bun target changes.
See the [qualification scope](../../examples/external-agent-targets/README.md)
for reproduced authorization boundaries and limits.

- Trusted native Plugins remain trusted code. These Capability contracts do not
  claim to sandbox a Plugin that has direct process or credential authority.
- Extension state is associated Plugin state, not a second authoritative Agent
  Session store. Replay/projection must not re-run a Model or Tool.
- A dynamic-authority snapshot authorizes only exact resources already bound by
  the immutable Plan. It cannot discover or install a new Plugin or privilege.
- A durable task Provider owns its store, external-effect receipt policy, and
  upgrade compatibility. `UncertainExternalEffect` is observable-only rather
  than a retry instruction.
- Console's fallback is a safe shell state for a missing Workspace requirement.
  It does not execute unknown extension payloads or provide a general task UI.
- Native SDK lifecycle and isolated Wasm execution are separate proofs. Durable
  Task is native-only; the Wasm fixture does not change its portability contract.
