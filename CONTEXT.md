# Lenso Agent Harness context

## Status

This repository is the product owner for a headless-first Agent Harness built
as an ordinary Lenso App. The first executable slice contains portable
Capability sources, native Module implementations, composable Prompt/Skill
contributions, semantic TUI panels, a CLI Runner, a no-subcommand `lenso-agent`
TUI entrypoint, and one checked read-only base definition at
`lenso.app.json`. Optional text Tools, workspace mutation, filesystem Skills,
local process execution, OpenAI-compatible Models, and experimental Codex
Direct access are selected independently through the Host's persisted Plugin
Active Set. The Host generates ignored `.lenso/resolved-plan.json`; no product
`composition/` directory or named App variants remain.

The Harness depends inward on released Lenso Plan, Kernel, protocol, and
optional Module packages. It temporarily pins the host Runtime and Adapter to
one exact Git revision until the Plugin Runtime baseline is published. The
OpenAI-compatible profile likewise pins the external Secrets package by Git
revision until that package is published. Portable core must never depend back
on this repository.

Agent Tool Provider is the first source-first Capability migration. Its
annotated Rust trait and value types own authoring, while the build derives and
byte-checks the committed Descriptor and package-local Schemas. Existing Rust
and TypeScript projections continue to consume those locked artifacts, so the
migration does not change the Capability identity, version, digest, wire
contract, or cross-language authority.

Agent Model is the second migration and the first source-derived Stream
Capability. Its `Stream<Message, DomainError>` return type derives the
interaction kind without a duplicated string annotation. The migration keeps
the existing stream Descriptor, open/message/error Schemas, generated clients
and providers, and plugin digest byte-identical.

Generated Rust Capability Clients implement the product-neutral
`CapabilityClient` contract and emit hidden provider/requirement metadata for
Module compilation. Every Harness Provider Module now uses the public
`lenso::prelude`, `lenso::module`, and `lenso::provides` source-first facade.
Its configuration type derives the package-owned Schema, `Port<Client>` and
`ManyPort<Client>` fields derive exact requirements, annotated Provider
implementations derive endpoints, and `#[module(lifecycle)]` exposes only the
prepare, activate, and deactivate hooks that the Module actually owns. The
former Harness-specific Module authoring and proc-macro crates are removed.
The CLI and TUI Shell Modules remain deliberate compatibility exceptions: they
are consumer-only identities used to anchor terminal-surface bindings, while the
current source-first facade finalizes Module metadata from a Provider
implementation and cannot yet describe a Module with no provided Capability.
Adding a fake Provider would make the graph less accurate. The TUI Shell binds
one Agent plus explicit `many` semantic panel Contributions. The native Host
surface snapshots those resolved providers and rejects cross-provider panel ID
collisions before entering terminal raw mode.

Source-derived Provider Descriptors use the standard fail-fast admission
default (`max_concurrency: 1`, `queue_capacity: 0`). The regenerated canonical
Plans therefore replace the old hand-written per-Module queue capacities while
preserving the same Module Instances, Capability requirements, bindings, and
configuration.

The root `lenso.app.json` is the sole product App Definition and selects only
the small read-only base. Module packages derive package identity,
configuration Schema, Capability endpoints and requirements, execution policy,
factory, and link-time registration into Cargo artifacts. The CLI builds the
declared Host package, discovers workspace and external Module artifacts
without executing Module code, then derives the immutable base Plan. The Host
adds only catalog-reviewed Plugin contributions from Desired State before
staging a new Generation. Test-only provider fixtures live beside integration
tests and have no product composition authority.

The current host baseline selects released `lenso-app-plan 0.1.4` and
`lenso-kernel 0.1.9`. It pins `lenso-runner`, `lenso-native-adapter`, and
`lenso-plugin-control-plane` to `lenso-runtime-rust` revision
`c56e4a01d14704eeae26e2121dbd87dbf380b1d3` and its standalone Plugin Bundle
builder from the same revision. That Runtime includes the durable
Generation Controller and Host suspension seam, the bounded request ABI shared
by the preview Wasm Component and QuickJS Adapters, the experimental native
dylib Adapter, and the source-derived native Module factory catalog. The catalog
records code linked into the Host; it does not discover dependencies or
activate Module Instances. The CLI uses the generic control plane to admit and
lock reviewed passive releases plus executable contributions registered in a
product-owned Plugin Profile Catalog, resolve and stage one initial App
Generation, and pin each Turn with a durable Controller route.
Before the Ready Gate, the Host content-addresses the canonical Generation Spec
under `.lenso/plugins/generations`; the Turn lease injects that digest into the
root Invocation Context and the Agent Loop records it in `turn_started` Session
events. Resumed Sessions can therefore cross Generations without losing which
immutable graph owned each Turn.

The Host now stores fenced Generation lifecycle authority under a stable
product-surface namespace: `.lenso/plugins/generation-control` for the companion
headless CLI and `.lenso/plugins/tui-generation-control` for `lenso-agent`.
These distinct App Compositions share Plugin authority and immutable Generation
records, but never recover each other's Controller lineage. Startup either
creates the initial durable Generation or recovers exact Active and Standby
Generations from `.lenso/plugins/generation-authorities`. Recovery authority is
separate from user-visible rollback history and GC roots. If committed Plugin
authority changed while the CLI was stopped, startup performs a standard
maintenance transition before routing. One shared authority fence covers
resolve, recovery, Ready, and switch. Normal exit suspends process-local Kernel
resources without retiring durable authority. The Controller owns
terminal-failure maintenance, while the Turn route injects the Generation
digest into Invocation Context. A later validated transition may reactivate the
exact immutable digest of a retired Generation; live candidate duplication
still fails closed. Each startup or offline transition hashes the exact Host
executable once and reuses that immutable build identity across every initial,
retained, current, and candidate Generation resolution in the operation.
Clean suspension commits a durable marker only after every process-local
Generation resource is released. When a replacement Host executable cannot
restage the old executable-bound Generation, that marker authorizes a fenced
cold replacement: the old control records are retained with
`host_build_replaced`, a new Supervisor and routing epoch are opened, and the
current exact Generation passes the ordinary initial Ready Gate. Missing clean
suspension evidence still fails closed instead of resetting Controller state.

The Host also owns one offline Plugin Release transition. Upgrade admission is
guarded by an exact active-Manifest compare-and-swap, resolves current and
candidate Generations against the reviewed base Plan, and uses the Runtime
maintenance Ready Gate before atomically committing. Canonical Active Sets are
retained by digest for explicit manual rollback through the same gate. The
Host-owned `AuthorityCoordinator` now gives startup and validated inspection a
shared snapshot fence and gives install, remove, upgrade, and rollback an
exclusive transition fence. The transition process shuts the preview
Generation down after validation; it is not live hot loading or distributed
coordination.

Local governance is risk-derived. Passive Releases and selected executable
contributions that are stable, trusted, stateless, permission-free,
dependency-free, Artifact-free, and append only to an existing `many`
requirement receive automatic local admission with derived evidence in their
Receipt. Provider replacement and every higher-authority boundary still
require review evidence. Upgrade may derive its Manifest CAS from the validated
active authority while holding the exclusive fence; callers can still supply
an expected digest explicitly.

The CLI provides read-only provenance inspection across retained Active Sets,
Generation Specs, and Session Turns. The File Session Module validates and
projects its private `turn_started` records, the Agent Loop interprets its own
payload contract, and the CLI joins them without printing user input. Missing
or corrupt Generation Specs are observable facts; inspection never repairs,
deletes, activates, or rolls back authority.

The Host can also produce a read-only Generation GC plan. It protects Specs
referenced by current or retained Plugin Sets or by any durable Session Turn,
and reports only the remaining Specs as candidates. A candidate is not deletion
authorization; deletion, time-based retention, and Plugin Store collection
remain deferred.

The Catalog currently contains one exact native Tool Provider append-to-`many`
entry plus reviewed workspace-edit, Skills, local-process, and Model Profiles;
one package-independent isolated Wasm Tool Provider shape with the same
bounded attachment, one restricted fixture Model Provider replace-`one` entry,
one package-independent reviewed Wasm Tool variant with a fixed
`lenso.agent.workspace-read@1/read_text` Host import, and one experimental
Codex Direct replacement set. The pure Wasm Tool shape fixes
the Capability, operations, execution class, empty configuration, and absence
of Host imports, permissions, state, Data mounts, and binding templates; it
still requires review evidence. The workspace-reader variant has the same
limits except for its single exact requirement, which the Host Profile binds
to the dedicated base `workspace-import-read` Instance; the Bundle cannot select the provider
or add another requirement. The Codex set closes exact Model and Auth
contributions, their intra-Plugin `one` binding, and the compatible base Agent
model configuration. Replacement requires the exact base edge and allowlisted
displaced package; removal restores the base Plan for the next Generation.
Package presence or Catalog extensibility is not a claim that arbitrary
executable Plugins, general permissioned Host imports, general provider replacement,
Generation deletion, or Plugin Store collection are ready. The CLI exposes
bundled `plugins enable` and `plugins disable` selection and persists the exact
result in the Active Set without another App Definition.

## Product outcome

A local developer starts one explicitly composed Agent, submits a turn,
consumes a streamed Model result, allows the Agent to use only selected Tool
providers, and can resume the durable Session after restart. Model, Prompt,
Tool, Session, and UI choices remain replaceable through App Composition
without changing the Agent Loop. The Loop supports direct answers or bounded
sequential Tool steps, streams text incrementally, and reconstructs a bounded
completed-turn history from the Session log.

## Canonical ownership

- **Agent Loop Module** owns volatile Turn/Step coordination, budgets, Model
  request construction, Prompt application, Tool sequencing, and terminal
  outcomes.
- **Prompt Runtime Module** owns contribution collision checks, deterministic
  assembly, aggregate limits, and content digests.
- **Prompt Provider Modules** own versioned instruction or Skill content and
  provider-local order. Filesystem Providers additionally own rooted path
  containment, bounded reads, and startup snapshots. The progressive Skills
  Provider contributes only its bounded metadata catalog to Prompt assembly;
  Skill bodies and resources remain Tool-selected.
- **Tool Runtime Module** owns Tool catalog aggregation, collision checks,
  argument validation, and deterministic dispatch to explicitly bound Tool
  Provider Modules.
- **Session Module** owns Session identity, ordered append-only events,
  revisions, recovery, retention policy, and its private durable store.
- **Model Modules** own provider protocol, credentials usage, streaming,
  cancellation, limits, and provider-error translation.
- **Tool Provider Modules** own their Tool definitions, resource policy, final
  authorization, execution, and Domain Errors.
- **Process Provider Modules** own the authoritative executable catalog,
  workspace-rooted cwd policy, environment projection, subprocess lifecycle,
  output and timeout bounds, and process-group cleanup. The Process Tool
  Provider owns only the Agent-facing projection.
- **CLI Module** owns headless terminal input, streamed rendering, local
  cancellation, and Session selection.
- **TUI Shell Module** owns interactive terminal layout, focus, input,
  streamed rendering, cancellation, and semantic panel aggregation.
- **TUI Contribution Modules** own bounded panel content. They do not own raw
  terminal state, the event loop, arbitrary `ratatui` widgets, or ambient
  Capability access.
- **App Composition** owns exact Module Instances, configuration, bindings,
  execution classes, and admission limits.
- **Agent Host control plane** owns the content-addressed Plugin Store, exact
  Host Build Manifest, Host Execution Policy, immutable App Generation
  resolution and Spec records, Ready Gate, Turn routing lease, and Generation
  resource drain.
- **Kernel** remains product-neutral and owns only its accepted portable runtime
  mechanisms.

## Hard invariants

- The Kernel receives one immutable Resolved App Plan. The Harness never asks a
  running Kernel to discover, install, rebind, or hot-load a plugin.
- Every Agent Turn is admitted through a lease for one exact active App
  Generation. Its `turn_started` Session event records that Generation Spec
  digest. Dropping the lease is required before Generation resource drain.
- User-facing Agent plugins are ordinary packages containing one or more
  Modules that provide declared Agent Capabilities.
- Passive Plugin Bundle admission never authorizes executable contributions.
  The active authority must close one exact local-review Receipt, Manifest,
  Feature selection, Product Metadata selection, and immutable Plugin lock.
- Prompt and Skill plugins never scan or mutate the running graph; Composition
  explicitly binds their Provider Instances in deterministic order.
- Filesystem Skill Providers read only selected documents or discovered Skill
  children below an explicitly configured root. They snapshot at startup,
  enforce path and byte limits, and never execute Skill assets or scripts.
- No Harness Module may discover dependencies through a global registry.
- TUI panels come only from explicit `many` bindings in the immutable Plan;
  providers cannot register widgets or mutate the running layout graph.
- The generated native factory catalog is Host build availability, not Module
  dependency discovery; only the immutable Plan may activate and bind an entry.
- Every invocation is bounded by Plan admission, deadlines, cancellation, and
  product limits. There is no unbounded queue or implicit retry loop.
- Model calls and Tool calls are never replayed automatically after uncertain
  failure.
- Session events are durable product facts. Runtime Diagnostics and live
  streams are not substitutes for the Session log.
- Secret values never enter App Composition, Session events, errors, Debug
  output, or Runtime Diagnostics.
- Default Tool access is read-only and rooted in an explicitly selected
  workspace. Workspace mutation exists only when its Plugin is enabled.
- The coding profile has create-only and unique exact-edit Tools. Process
  execution exists only in the higher-authority local-coding profile, with no
  shell-string parsing, generic overwrite/delete, approval workflow, subagents,
  automatic compaction, runtime code replacement, or hostile-code isolation.

## First executable slice

The deterministic root base selects these keyed Module Instances:

- `cli`
- `agent`
- `model`
- `prompt`
- `fixture-instructions`
- `summary-skill`
- `tools`
- `tui`
- `tui-help`
- `workspace-import-read`
- `workspace-read`
- `sessions`

The same base includes the removable `tui-help` semantic panel Contribution.
Running `lenso-agent` with no arguments resolves this root Plan and enters the
terminal interface directly; the product entrypoint has no subcommands. The CLI
and TUI keep separate durable Controller namespaces despite sharing the Plan.

The Prompt aggregate snapshots explicitly bound versioned contributions and
records their IDs, versions, kinds, and SHA-256 digests in `model_requested`
Session events. Contribution content becomes one system message and is not
copied into the Session log. Removing all Prompt Providers leaves the aggregate
and Agent runnable with an empty system prompt.

The first useful transitions ask the Agent to summarize a selected workspace
README and to navigate by listing, literal search, then targeted read. A
deterministic Model fixture proves the Tool calls and Session facts; it also
proves direct answers, sequential Tool calls, budget failures, and
completed-turn context after restart. Unavailable durable Session storage keeps
the App from becoming ready.

The `openai-compatible` Plugin replaces the fixture `model` Instance with
`lenso.agent.model.openai-compatible` and adds a `secrets` Instance from the
external `lenso.secrets.env` package. It maps Chat Completions request/Tool
shapes and incremental SSE events behind the same Model Capability. Missing
credentials keep the App from becoming ready; credentials, provider bodies,
and sensitive values never enter Plans, Session events, or diagnostics.

The experimental `openai-codex-direct` profile keeps the Lenso Agent Loop and
replaces only its Model provider. `lenso.agent.auth.openai-codex` owns browser
PKCE OAuth, headless device OAuth, refresh, and private credential storage in
`~/.lenso/agent/auth.json`; the direct Model Module uses its private Auth
Capability to call the Codex Responses backend. Tokens never enter the App
Plan, Session log, or diagnostic output. This integration does not shell out
to or read credentials from the Codex CLI.

The opt-in `skills` Plugin adds one filesystem Skills Module to both the Prompt
and Tool aggregates. The same immutable startup snapshot
provides a bounded name/description Prompt catalog plus `skill_list`, `skill`,
`skill_resources`, and `skill_resource`. Normal selection reads the matching
Skill directly; `skill_list` remains available
for diagnostics and catalog overflow.

The independently enabled `workspace-edit` Plugin contributes
`lenso.agent.workspace-edit` without another App Definition. It atomically
creates absent UTF-8 files and performs one unique exact replacement in an
existing UTF-8 file. Disabling it restores the exact root base.

The opt-in `local-process` Plugin adds `lenso.agent.process-tools` and
`lenso.agent.process.native`. The Tool projection
requires exactly one private `lenso.agent.process@1` provider. That provider
resolves only Profile-allowed executable basenames, preserves shim names,
rechecks executable identity, contains cwd below the workspace, clears and
selectively projects environment variables, bounds arguments/time/output, and
kills the whole Unix process group on timeout, cancellation, output overflow,
or dropped invocation.
Users enable `workspace-edit` separately when both authorities are required.

## Deferred direction

Web UI, approval policy, hostile-code sandboxing, marketplace Skill
installation, live Skill watching,
ordered Hook interception, Trajectory inspection, replay analysis, multi-agent
scheduling, sandboxed Code Mode, Creator experiments, additional production
Plugin Profile Catalog entries, general `one` replacement, `optional` binding
replacement, publisher-selected provider configuration, third-party Host
Capability permissions, automatic rollback,
Generation retention windows and deletion, Plugin Store garbage collection,
distributed coordination, and overlap replacement require their own product
slices. Replacement
must stage a new Resolved App Plan and App Generation above the Kernel rather
than mutate the running graph.
