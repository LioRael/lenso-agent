# ADR-0086: Resolve Provider controls and transient failures per Turn

Status: Accepted

## Context

Reasoning levels and fast service tiers are not universally supported. Provider
errors also distinguish transient overload from context overflow and invalid
requests. Sending optimistic controls or retrying after visible output can
duplicate effects.

## Decision

The Generation-revisioned Model Catalog declares reasoning choices and service
tiers per model. The TUI validates `/thinking` and `/fast` against that catalog,
and resets incompatible selections when the model changes. The direct ChatGPT
Adapter carries per-Turn reasoning and the `fast` service tier. OpenRouter is a
distinct Provider identity over the OpenAI-compatible Adapter and may supply
its attribution headers; generic OpenAI-compatible instances exclude
`openrouter.ai` configurations.

Model 2.1 adds typed rate-limited, overloaded, and context-overflow failures.
The Agent Loop retries a transient model stream open once, before any output or
Tool effect. Context overflow triggers one durable compaction attempt and one
retry. Optional deadlines, identical Tool-round detection, output reservation,
and all other strict quotas remain disabled unless configured.

## Consequences

- unsupported thinking and speed controls fail before a Provider request;
- fast mode is not advertised for models without explicit catalog support;
- retries cannot replay a partially observed model step; and
- OpenRouter and generic compatible endpoints can evolve independently without
  duplicating transport code.

## Proof

Catalog tests cover model-specific reasoning and fast support plus the distinct
OpenRouter identity. Adapter tests cover request fields and status mapping.
Agent Loop tests cover retry classification, stable repeated-call fingerprints,
model-aware compaction, and optional quota defaults.
