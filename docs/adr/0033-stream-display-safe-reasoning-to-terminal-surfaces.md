# ADR 0033: Stream display-safe reasoning to terminal surfaces

- Status: accepted
- Date: 2026-08-26
- Relates to: ADR 0001, ADR 0005, ADR 0020, ADR 0031

## Context

The TUI can show Agent text and semantic Tool lifecycle events, but it cannot
truthfully render a Grok Build-style `Thinking...` block. Model providers may
receive a provider-designated reasoning summary, yet the Model Stream has no
message kind for it and the Agent Turn Stream cannot preserve its lifecycle.
Inferring thoughts from ordinary Agent text would conflate final output with
reasoning and could expose content that the provider did not designate for
display.

Both existing message-kind enums are closed. Adding a reasoning variant is a
breaking portable wire change rather than an additive Descriptor minor.

## Decision

Introduce `lenso.agent.model@2`, Descriptor `2.0.0`. Its existing `complete`
Stream retains the same opening request, terminal outcomes, Tool calls, text,
and usage messages, and adds `reasoning_summary_delta`. A Model Module may emit
only provider-designated, display-safe reasoning summary text through this
kind. Raw private chain-of-thought and encrypted reasoning state are not part
of the Capability.

Introduce `lenso.agent@3`, Descriptor `3.0.0`. Its Turn Stream retains text and
Tool lifecycle/progress messages and adds `reasoning_delta` and
`reasoning_completed`. Both carry one stable Turn-step `reasoning_id`;
completion also carries elapsed milliseconds when available. The Agent Loop
forwards summary deltas in order and closes the active reasoning block before
the first Agent text, Tool call, or terminal result for that Model step.

The TUI may create one provisional local `Thinking...` entry immediately after
submit for responsive feedback. It removes that entry if no reasoning delta
arrives, otherwise updates it in place and collapses the completed entry to
`Thought for Xs`. This provisional presentation state is not durable evidence.

Reasoning progress remains volatile. Session persistence continues to own
stable user input, Tool requests/results, Model output, Turn completion, and
Generation provenance; it does not persist reasoning summaries in this slice.

## Consequences

- Terminal surfaces can render truthful streaming Thoughts without parsing
  final Agent text.
- Providers that do not expose a display-safe summary remain valid behaviorally
  but must target the new Capability identity to satisfy the new App.
- Model protocol parsing remains owned by Model Modules, Turn ordering remains
  owned by the Agent Loop, and presentation remains owned by the TUI Shell.
- Kernel, Runtime Drivers, and Execution Adapters gain no reasoning-specific
  registry or behavior.

## Rejected alternatives

Treating the new enum values as a minor release would break older generated
consumers. Sending reasoning through `text_delta` would make final output and
reasoning indistinguishable. Persisting raw reasoning would create a new
durable data contract and privacy surface beyond the requested terminal UX.
