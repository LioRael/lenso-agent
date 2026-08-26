# ADR 0027: Admit bounded parallel Tool waves

## Status

Accepted.

## Context

One model response may request several independent Tools. The Harness previously advertised only
serial Tool submission and every derived binding inherited a single-request admission limit. That
left safe read-only work unnecessarily sequential. Treating every Tool as parallel would instead
allow mutations, processes, approvals, or other ordered effects to overlap without an explicit
owner declaring that behavior safe.

## Decision

Tool Providers publish one execution class with each catalog entry:
`parallel_safe` or `exclusive`.

- The Agent Loop preserves the model's call order and partitions each response into consecutive
  parallel-safe waves and single-call exclusive barriers.
- A parallel-safe wave uses a bounded pool controlled by the App-owned
  `max_parallel_tool_calls` limit. Exclusive calls run alone after the preceding wave settles and
  before later calls begin.
- Tool requested events are committed before dispatch. Every started call settles, and Tool result
  events, stream progress, and the next model input are restored to model order even when execution
  completes out of order.
- The immutable App Definition explicitly admits four concurrent requests on the Agent-to-Tools
  binding and on each admitted parallel Tool Provider binding. The resolved Plan remains the sole
  binding and request-admission authority.
- Read-only, deterministic Providers may declare `parallel_safe`. Workspace mutation and local
  process Providers remain `exclusive`. Unknown or missing execution metadata fails conservatively
  to exclusive scheduling.
- OpenAI-compatible Model adapters request parallel Tool calls only after the Capability contracts,
  scheduler, and explicit Plan admissions are present.

## Consequences

- Independent reads can overlap without allowing an exclusive Tool to cross their ordering fence.
- Provider declarations do not grant capacity by themselves; App binding admission and the Agent
  Loop bound must all permit concurrency.
- Cancellation and one-call failure wait for the already-started bounded wave to settle before the
  Turn reports its first error. This keeps durable ordering complete and deterministic.
- Per-call argument classification, resource-keyed mutation lanes, Code Mode subcalls, approvals,
  Hooks, and subagents remain separate changes.

## Rejected alternatives

Running every returned call through one unbounded join discards App admission authority and makes
exclusive effects race. Keeping the Model adapter permanently serial prevents the Runtime from
using reviewed read-only concurrency. Encoding concurrency only in the Model adapter cannot protect
other callers of the Tools Capability.
