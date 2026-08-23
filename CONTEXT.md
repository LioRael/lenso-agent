# Lenso Agent Harness context

## Status

This repository is the product owner for a headless-first Agent Harness built
as an ordinary Lenso App. The first executable slice contains portable
Capability sources, native Module implementations, a CLI Runner, and the
checked `headless-readonly`, `openai-readonly`, and experimental
`openai-codex-direct` App Compositions.

The Harness depends inward on released Lenso Plan, Kernel, Runtime, Adapter,
protocol, and optional Module packages. The OpenAI-compatible profile pins the
external Secrets package by Git revision until that package is published.
Portable core must never depend back on this repository.

## Product outcome

A local developer starts one explicitly composed Agent, submits a turn,
consumes a streamed Model result, allows the Agent to use only selected Tool
providers, and can resume the durable Session after restart. Model, Tool,
Session, and UI choices remain replaceable through App Composition without
changing the Agent Loop. The Loop supports direct answers or bounded sequential
Tool steps, streams text incrementally, and reconstructs a bounded
completed-turn history from the Session log.

## Canonical ownership

- **Agent Loop Module** owns volatile Turn/Step coordination, budgets, model
  and Tool sequencing, and terminal outcomes.
- **Tool Runtime Module** owns Tool catalog aggregation, collision checks,
  argument validation, and deterministic dispatch to explicitly bound Tool
  Provider Modules.
- **Session Module** owns Session identity, ordered append-only events,
  revisions, recovery, retention policy, and its private durable store.
- **Model Modules** own provider protocol, credentials usage, streaming,
  cancellation, limits, and provider-error translation.
- **Tool Provider Modules** own their Tool definitions, resource policy, final
  authorization, execution, and Domain Errors.
- **CLI Module** owns terminal input, streamed rendering, local cancellation,
  and Session selection.
- **App Composition** owns exact Module Instances, configuration, bindings,
  execution classes, and admission limits.
- **Kernel** remains product-neutral and owns only its accepted portable runtime
  mechanisms.

## Hard invariants

- The Kernel receives one immutable Resolved App Plan. The Harness never asks a
  running Kernel to discover, install, rebind, or hot-load a plugin.
- User-facing Agent plugins are ordinary packages containing one or more
  Modules that provide declared Agent Capabilities.
- No Harness Module may discover dependencies through a global registry.
- Every invocation is bounded by Plan admission, deadlines, cancellation, and
  product limits. There is no unbounded queue or implicit retry loop.
- Model calls and Tool calls are never replayed automatically after uncertain
  failure.
- Session events are durable product facts. Runtime Diagnostics and live
  streams are not substitutes for the Session log.
- Secret values never enter App Composition, Session events, errors, Debug
  output, or Runtime Diagnostics.
- V1 Tool access is read-only and rooted in an explicitly selected workspace.
- V1 has no Creator Mode, Code Mode, shell/write Tools, subagents, automatic
  compaction, or runtime code replacement.

## First executable slice

The deterministic `headless-readonly` profile selects these keyed Module
Instances:

- `cli`
- `agent`
- `model`
- `tools`
- `workspace-read`
- `sessions`

The first useful transition asks the Agent to summarize a selected workspace
README. A deterministic Model fixture proves the Tool call and Session facts;
it also proves direct answers, sequential Tool calls, budget failures, and
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

## Deferred direction

Web UI, approval policy, prompt/skill contributions, ordered Hook interception,
Trajectory inspection, replay analysis, multi-agent scheduling, sandboxed Code
Mode, Creator experiments, and App Generation require their own product slices.
True seamless plugin-set replacement must stage a new Resolved App Plan and App
generation above the Kernel rather than mutate the running graph.
