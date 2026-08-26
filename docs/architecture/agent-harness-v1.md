# Agent Harness V1 architecture

## Outcome

A local developer can start one explicitly composed Agent, ask it to summarize
a file inside an allowed workspace, observe a streamed answer, and resume the
durable Session after restart. Model and Tool providers are replaceable through
Composition. Missing credentials, invalid paths, provider failure, exhausted
budgets, and unavailable durable storage remain explicit failures.

## Runtime graph

```text
agent-cli
  `- one lenso.agent@1 -> agent-loop
       |- one lenso.agent.model@1 -> openai-compatible-model
       |    `- one lenso.secrets@1 -> env-secrets
       |- one lenso.agent.tools@2 -> tool-runtime
       |    `- many lenso.agent.tool-provider@2 -> selected Tool providers
       |         `- process-tools -> one lenso.agent.process@1 -> native-process
       |- one lenso.agent.prompt@1 -> prompt-runtime
       |    `- many lenso.agent.prompt-provider@1 -> prompt plugins
       `- one lenso.agent.session@1 -> session-log
```

All selected Instances run in one Execution Lane for the first slice. Before
Kernel boot, the Host control plane assembles native Rust, QuickJS, and Wasm
Component Execution Adapters from its build manifest and registers the
generated JSON codec for `lenso.agent@1`. The built-in profile remains native;
a reviewed Artifact profile may replace that provider in the next immutable App
Generation. No contract opts into cross-lane transfer in V1.

## Authoritative facts

- App Composition owns selected package inputs, keyed Instances, bindings,
  execution classes, non-secret configuration, and admission limits.
- Session owns Session identity, its monotonic revision, accepted event order,
  recovery, and retention.
- Agent Loop owns only volatile Turn and Step progress reconstructed from the
  Session log.
- Tool Runtime derives its catalog from exact resolved Provider bindings; it
  persists no second plugin catalog.
- Tool Providers own Tool definitions, resource policy, execution, and final
  Domain Errors.
- Model Modules own provider protocol, egress, cancellation, response limits,
  and provider error translation.
- The Host control plane owns Plugin Store authority, Host Build and Execution
  Policy documents, Generation resolution, Ready Gate, routing leases, and
  Generation resource drain.
- The Host Plugin Profile Catalog owns the finite executable contribution
  allowlist and product attachment recipes. It projects the same registrations
  into admission, Host policy, startup revalidation, and Generation bindings;
  Plugins cannot add entries or binding templates.
- Passive Plugin activation owns one atomic active-set document closing the
  exact Plugin lock, embedded Manifests, selected Features and Product
  Metadata, and immutable local-review Admission Receipts.
- Offline Plugin Release transition owns Manifest CAS, current/candidate
  Generation resolution, maintenance Ready Gate, content-addressed Active Set
  history, and atomic candidate or manual-rollback commit.
- Provenance inspection validates Active Set and Generation content addresses,
  while Session storage and Agent event semantics remain with their owning
  Modules.

## First turn

1. CLI leases the exact active App Generation and resolves the Agent route from
   that lease.
2. CLI opens `lenso.agent@1/run_turn` and closes its sending half.
3. Agent opens or resumes the Session and atomically records `turn_started`.
4. Agent reads the bounded Session tail and the validated Tool catalog.
5. Agent assembles bounded Prompt contributions. A selected progressive Skills
   Provider contributes only Skill names and descriptions from its startup
   snapshot.
6. Agent opens `lenso.agent.model@1/complete` with normalized messages and Tool
   definitions.
7. Text deltas flow to CLI. A complete Tool call is recorded before execution.
8. Tool Runtime validates the arguments and dispatches to the owning Provider.
9. Agent records the Tool result and opens the next bounded Model step.
10. Agent records `turn_completed`, closes the stream, and returns terminal
   success. Domain or Runtime failure records `turn_failed` or
   `turn_cancelled` before the terminal outcome when Session remains available.
11. CLI releases the Generation lease only after the terminal outcome.

The first implementation sets finite limits for steps, Tool calls, Session
events read per turn, model output bytes, Tool output bytes, stream messages,
queue capacity, deadlines, and App shutdown.

## Session event envelope

V1 stores an ordered envelope containing event id, type, Turn id, timestamp,
and a portable JSON payload encoded as text. The Session Module treats the
payload as opaque while enforcing identity, revision, ordering, size, and
durability. The Agent Loop owns the payload meaning for its declared event
types. Secret values are forbidden.

Initial event types are `session_created`, `turn_started`, `model_requested`,
`model_output`, `tool_requested`, `tool_result`, `turn_completed`,
`turn_failed`, and `turn_cancelled`.

## Trust and authorization

The App author selects all trusted packages and explicitly configures the
remote model endpoint and allowed workspace root. Workspace Tool Providers
normalize paths, reject symlink/root escapes and bounded-size violations, and
own their final access decisions. The opt-in mutation Provider adds only
create-absent and unique exact-edit semantics. The higher-authority local
process Provider owns final executable, cwd, environment, timeout, argument,
output, and process-lifecycle decisions; the Process Tool Module only projects
that bound Capability to the Model. The Agent Loop cannot bypass the Tool
Runtime or acquire undeclared handles.

Model credentials resolve through `lenso.secrets@1`. They never enter model
messages, Session payloads, errors, configuration, or diagnostics. Because
workspace content may be sent to the selected remote Model, the profile must
make both the workspace root and model endpoint visible before execution.

## Acceptance

1. A deterministic Model fixture proves a composed Prompt behavior, direct
   streamed answer, one or more
  sequential `list`, `search`, and `read`
  calls, and finite step/Tool-call limits.
2. OpenAI-compatible and direct ChatGPT subscription smoke tests exercise the
   same logical turn without changing the Agent Loop.
3. Restarting the App preserves the Session, its next revision, and bounded
   completed-turn conversational context.
4. Rebinding Model or Tool Provider Instances changes behavior without changing
   Agent Loop code.
5. Missing credentials or Session storage prevents readiness.
6. Workspace escape or symlink traversal returns a Tool Domain Error and never
   reads the target; list/search traversal remains deterministically bounded.
7. Model failure is a Runtime Failure; no provider substitution or replay
   occurs.
8. Budget exhaustion produces a declared terminal Domain Error and a durable
   failure event.
9. The opt-in workspace-edit Plugin proves atomic create, unique exact edit,
   and read-back; disabling it restores the readonly graph.
10. The local-process plus workspace-edit Plugins prove edit, structured
    `cargo check`, and read-back. Provider tests prove nonzero exit capture,
    policy rejection, timeout, output overflow, cancellation, descendant
    cleanup, and root loss. Removing both Process Modules restores the readonly
    graph.
11. Installing the reviewed native text Tool Bundle adds one Plugin-owned
    Instance and derived `tools` binding; the Agent invokes `uppercase`.
    Removing the Plugin deletes that Tool from the next Generation.
12. Catalog tests prove deterministic multi-profile registration, reject
    duplicate profile/factory authority, and require an exact `many`
    Capability attachment before deriving a binding.
13. An upgrade test proves failed CAS leaves authority unchanged, a reviewed
    candidate passes a real maintenance Ready Gate before commit, manual
    rollback restores byte-identical authority, and tampered history fails
    closed.
14. Read-only inspection tests recover retained rollback handles, close Plugin
    authority, inspect Generation fields, trace resumed Session Turns across
    Generations, and classify corrupted Specs without exposing Turn input.
15. A real child process holding an exclusive Plugin authority transition fence
    blocks App startup until release; direct process exit releases the OS-owned
    fence and leaves later snapshots available.
16. A read-only GC plan validates all Generation Specs and Session Turn
    provenance, protects current or retained Plugin Set and Session references,
    and reports unreferenced Generation candidates without deleting them.
17. Installing the reviewed QuickJS Agent Bundle replaces the built-in Agent
    Loop, starts the resolved durable Generation, and completes a typed streamed
    Turn through the generated `lenso.agent@1` codec. The same product profile
    boundary admits reviewed Wasm Component Agent artifacts; Adapter-level
    tests execute a real Rust Component guest.
18. A standalone external Wasm Tool source tree, with no Harness path
    dependency, builds and verifies a Bundle, installs under mandatory review,
    serves a real Agent Tool call, upgrades, rolls back to the exact retired
    Generation, serves again, and disappears after removal without Host code
    registration or recompilation.
19. A second standalone Wasm Tool shape imports the generated
    `lenso.agent.workspace-read@1/read_text` client through one immutable
    Host-selected binding. Tests reject an added process requirement and prove
    a real workspace read across install, upgrade, rollback, and removal.
20. A reviewed network Wasm Tool shape imports only
    `lenso.agent.http-fetch@1/get`. Its exact origin request is promoted to an
    immutable grant, must fit inside the App-selected Provider allowlist, and
    is enforced on every bounded HTTP request. The base App has an empty
    allowlist.
21. Two parallel-safe Tools can complete out of order under a bounded pool but
    are persisted and returned to the Model in request order; an exclusive
    Tool between safe calls drains the preceding wave and blocks the next one.

## Deferred

Web UI, approval workflows, marketplace Skill installation, live Skill
watching, ordered Hooks, automatic compaction, per-call resource-keyed Tool
classification, Trajectory UI, replay
inspection, re-execution, subagents, scheduling, generic overwrite/delete,
shell-string execution, Creator Mode, hostile-code isolation, multi-lane
placement, additional production Catalog entries, `one` or `optional` binding
replacement, distributed coordination, automatic rollback, Generation deletion,
Plugin Store collection, retention windows, and overlap replacement are
separate slices. General third-party Guest imports are also separate: the
bounded pure Wasm Tool shape has no Host imports, while the workspace-reader
shape admits only one Host-selected read Capability and the network shape only
one exact-origin HTTP GET Capability. Other permissioned
external Modules require their own reviewed product profiles and policy.
