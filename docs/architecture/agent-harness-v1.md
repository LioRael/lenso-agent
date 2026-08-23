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
       |- one lenso.agent.tools@1 -> tool-runtime
       |    `- many lenso.agent.tool-provider@1 -> workspace-read-tools
       |- one lenso.agent.prompt@1 -> prompt-runtime
       |    `- many lenso.agent.prompt-provider@1 -> prompt plugins
       `- one lenso.agent.session@1 -> session-log
```

All selected Instances run in one Execution Lane for the first slice. The
native Runner assembles the Rust and Bun Execution Adapters before Kernel boot.
No contract opts into cross-lane transfer in V1.

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

## First turn

1. CLI opens `lenso.agent@1/run_turn` and closes its sending half.
2. Agent opens or resumes the Session and atomically records `turn_started`.
3. Agent reads the bounded Session tail and the validated Tool catalog.
4. Agent opens `lenso.agent.model@1/complete` with normalized messages and Tool
   definitions.
5. Text deltas flow to CLI. A complete Tool call is recorded before execution.
6. Tool Runtime validates the arguments and dispatches to the owning Provider.
7. Agent records the Tool result and opens the next bounded Model step.
8. Agent records `turn_completed`, closes the stream, and returns terminal
   success. Domain or Runtime failure records `turn_failed` or
   `turn_cancelled` before the terminal outcome when Session remains available.

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
remote model endpoint and allowed workspace root. `workspace-read-tools`
normalizes paths, rejects symlink/root escapes, binary files, and bounded-size
violations, and owns the final access decision. The Agent Loop cannot bypass
the Tool Runtime or acquire undeclared Tool handles.

Model credentials resolve through `lenso.secrets@1`. They never enter model
messages, Session payloads, errors, configuration, or diagnostics. Because
workspace content may be sent to the selected remote Model, the profile must
make both the workspace root and model endpoint visible before execution.

## Acceptance

1. A deterministic Model fixture proves a composed Prompt behavior, direct
   streamed answer, one or more
   sequential `workspace.read_text` calls, and finite step/Tool-call limits.
2. OpenAI-compatible and direct ChatGPT subscription smoke tests exercise the
   same logical turn without changing the Agent Loop.
3. Restarting the App preserves the Session, its next revision, and bounded
   completed-turn conversational context.
4. Rebinding Model or Tool Provider Instances changes behavior without changing
   Agent Loop code.
5. Missing credentials or Session storage prevents readiness.
6. Workspace escape returns a Tool Domain Error and never reads the target.
7. Model failure is a Runtime Failure; no provider substitution or replay
   occurs.
8. Budget exhaustion produces a declared terminal Domain Error and a durable
   failure event.

## Deferred

Web UI, approval workflows, dynamic Skill discovery and selection, ordered Hooks,
automatic compaction, Trajectory UI, replay inspection, re-execution,
subagents, scheduling, shell/write Tools, Creator Mode, Code Mode, hostile-code
isolation, multi-lane placement, and App Generation are separate slices.
