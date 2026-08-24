# Lenso Agent Harness context

## Status

This repository is the product owner for a headless-first Agent Harness built
as an ordinary Lenso App. The first executable slice contains portable
Capability sources, native Module implementations, composable Prompt/Skill
contributions, a CLI Runner, and the
checked `headless-readonly`, `headless-coding`, `openai-readonly`, experimental
`openai-codex-direct`, opt-in `openai-codex-direct-skills`, and opt-in
`openai-codex-direct-coding` App Compositions, plus the higher-authority
`headless-local-coding` and `openai-codex-direct-local-coding` Compositions.

The Harness depends inward on released Lenso Plan, Kernel, protocol, and
optional Module packages. It temporarily pins the host Runtime and Adapter to
one exact Git revision until the Plugin Runtime baseline is published. The
OpenAI-compatible profile likewise pins the external Secrets package by Git
revision until that package is published. Portable core must never depend back
on this repository.

The current host baseline selects released `lenso-app-plan 0.1.2` and
`lenso-kernel 0.1.7`. It pins `lenso-runner` and `lenso-native-adapter` to
`lenso-runtime-rust` revision
`58c02e882b90147c2fb0c7a4a5d778ad083ab1c4`, the merged Runtime revision
that also contains `lenso-plugin-control-plane`, the preview Wasm Component,
QuickJS, and native-dylib Execution Adapters, and the source-derived native
Module factory catalog. The catalog records code linked into the Host; it does
not discover dependencies or activate Module Instances. The CLI now uses the
generic control plane to admit and lock reviewed passive releases plus executable
contributions registered in a product-owned Plugin Profile Catalog, resolve and
stage one initial App Generation, and pin each Turn with a Generation lease.
Before the Ready Gate, the Host content-addresses the canonical Generation Spec
under `.lenso/plugins/generations`; the Turn lease injects that digest into the
root Invocation Context and the Agent Loop records it in `turn_started` Session
events. Resumed Sessions can therefore cross Generations without losing which
immutable graph owned each Turn.

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
entry, one restricted fixture Model Provider replace-`one` entry, and one
experimental Codex Direct replacement set. The Codex set closes exact Model and
Auth contributions, their intra-Plugin `one` binding, and the compatible base
Agent model configuration. Replacement requires the exact base edge and
allowlisted displaced package; removal restores the base Plan for the next
Generation. The non-native Adapter crates remain unselected. Package presence
or Catalog extensibility is not a claim that arbitrary executable Plugins,
permissions, general provider replacement, Generation deletion, Plugin Store
collection, or non-native execution classes are ready.

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
- **CLI Module** owns terminal input, streamed rendering, local cancellation,
  and Session selection.
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
  workspace. Workspace mutation exists only in explicitly selected coding
  Compositions.
- The coding profile has create-only and unique exact-edit Tools. Process
  execution exists only in the higher-authority local-coding profile, with no
  shell-string parsing, generic overwrite/delete, approval workflow, subagents,
  automatic compaction, runtime code replacement, or hostile-code isolation.

## First executable slice

The deterministic `headless-readonly` profile selects these keyed Module
Instances:

- `cli`
- `agent`
- `model`
- `prompt`
- `fixture-instructions`
- `summary-skill`
- `tools`
- `workspace-read`
- `sessions`

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

The `openai-readonly` profile replaces the fixture `model` Instance with
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

The opt-in `openai-codex-direct-skills` Composition adds one filesystem Skills
Module to the `readonly` Tool profile. The same immutable startup snapshot
provides a bounded name/description Prompt catalog plus `skills.list`,
`skills.read`, `skills.list_resources`, and `skills.read_resource`. Normal
selection reads the matching Skill directly; `skills.list` remains available
for diagnostics and catalog overflow.

The opt-in `headless-coding` and `openai-codex-direct-coding` Compositions add
the independent `lenso.agent.workspace-edit` Tool Provider. It atomically
creates absent UTF-8 files and performs one unique exact replacement in an
existing UTF-8 file. Existing readonly Compositions remain unchanged.

The opt-in `headless-local-coding` and
`openai-codex-direct-local-coding` Compositions add
`lenso.agent.process-tools` and `lenso.agent.process.native`. The Tool projection
requires exactly one private `lenso.agent.process@1` provider. That provider
resolves only Composition-allowed executable basenames, preserves shim names,
rechecks executable identity, contains cwd below the workspace, clears and
selectively projects environment variables, bounds arguments/time/output, and
kills the whole Unix process group on timeout, cancellation, output overflow,
or dropped invocation.

## Deferred direction

Web UI, approval policy, hostile-code sandboxing, marketplace Skill
installation, live Skill watching,
ordered Hook interception, Trajectory inspection, replay analysis, multi-agent
scheduling, sandboxed Code Mode, Creator experiments, additional production
Plugin Profile Catalog entries, general `one` replacement, `optional` binding
replacement, publisher-selected provider configuration, automatic rollback,
Generation retention windows and deletion, Plugin Store garbage collection,
distributed coordination, and overlap replacement require their own product
slices. Replacement
must stage a new Resolved App Plan and App Generation above the Kernel rather
than mutate the running graph.
