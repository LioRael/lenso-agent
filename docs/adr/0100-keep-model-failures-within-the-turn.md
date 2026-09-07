# ADR-0100: Keep model failures within the Turn

Status: Accepted

## Context

A direct Codex connection failure is a Model domain error. The Agent Loop
converted it to `RuntimeFailure::PluginFailure`, which retired the active
Generation and made subsequent requests fail until Host restart. Receive-side
transport failures also escaped the Model as runtime faults. Provider reason
codes were lost while crossing the Agent boundary.

## Decision

Agent Descriptor 3.1 retains `lenso.agent@3` and adds a `model_failure` domain
error carrying a bounded provider reason code and sanitized message. Model
business errors remain domain errors through Agent Loop; context overflow keeps
its existing dedicated Turn error. True runtime/admission/protocol misuse errors
retain their runtime classification.

The direct Model Plugin terminates an interrupted upstream WebSocket stream with its
existing non-retryable `provider_failure` domain error. It discards the socket
and continuation checkpoint and never replays a sent request or partial output.
The Agent Loop remains the only owner of the existing bounded model-open retry.
A later explicit Turn is newly admitted against durable Session state; this is
not automatic replay of the failed Turn or its Tools.

Transport diagnostics contain fixed phase/category codes and sanitized messages,
never credentials, headers, upstream response bodies, or arbitrary remote errors.
No Kernel policy, Session ownership, retry budget, or fallback rule changes.

## Consequences

Older consumers can decode the additive error through their unknown-domain-error
path. Current surfaces receive an explicit failed Turn rather than cancellation
caused by Generation retirement. A failed Turn may already have applied Tool
effects; callers must not interpret `model_failure` as permission to replay it.
