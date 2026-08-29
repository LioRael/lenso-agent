# ADR 0028: Compose bounded subagents as Tools

## Status

Accepted.

## Context

A root Agent sometimes benefits from delegating one self-contained task to an
independent context. Putting child-agent scheduling in the Kernel would make a
product-specific orchestration policy part of the portable runtime. Invoking
the root Agent recursively would also create an activation cycle and inherit
authority that the App did not explicitly grant to the child.

## Decision

The Harness admits subagent delegation as an optional Tool Provider Plugin.

- The reviewed `lenso.agent.subagent-tools` Plugin contributes `delegate` to the root Tool
  Runtime. Removing the Plugin removes that model-visible surface in the next
  immutable App Generation.
- The provider requires one explicitly bound `lenso.agent@3` child Instance.
  It does not discover or construct Agents at runtime.
- The base App composes `subagent-agent` separately from the root `agent` and
  binds it to a narrow `restricted-read-tools` Runtime. That Runtime projects only the
  Host-selected `lenso.agent.workspace-read@1/read_text` Capability.
- A delegated call inherits deadline, cancellation, and Generation provenance,
  opens a fresh durable child Session, and returns the child Session ID in Tool
  result metadata. The parent Session durably records that metadata through its
  ordinary Tool result event.
- Success metadata uses the versioned `lenso.agent.subagent-result@1` shape and
  records terminal status, fresh-context mode, child Session identity, byte
  bounds, and observed child message/Tool-call counts. Child Domain Errors and
  delegated-output overflow retain the same child Session identity in structured
  failure details whenever the child emitted one. This observable contract is
  introduced by Plugin release `0.2.0`.
- Plugin release `0.3.0` also projects `spawn_subagent`, `wait_subagent`, and
  `cancel_subagent`. The Plugin owns a bounded, generation-local task registry;
  spawned work is a generation-owned managed task, `wait_subagent` consumes one
  terminal result and releases its slot, and cancellation uses a child-only
  Invocation Context while still observing parent cancellation and deadline.
  The child Session remains the durable record; task handles are not presented
  as surviving Host suspension or an App Generation switch.
- Plugin release `0.4.0` also projects `send_subagent`. It requires the
  separately bound `lenso.agent.turn-input@1` Capability from the same child
  Agent Loop Instance; the Host privately binds both requirements to
  `subagent-agent`, so steering cannot target the root Agent or discover another
  Session. The request identifies the child Session and waits until the Agent
  Loop has included the input in a durable `model_requested` Session fact for
  the next model boundary. Acceptance returns that exact Session revision.
- Running input does not mutate an in-flight Model request. On arrival, the
  Agent Loop ends that Model stream, records `interrupted_by_input`, and starts
  the next model step only after the additional input is included in a durable
  `model_requested` fact. The queue is bounded and rejects input once the Turn
  closes it. `lenso.agent@3` remains one opening input plus an output stream;
  the request Capability avoids a private side channel and preserves existing
  Agent consumers and providers.
- Task and output bytes, child Agent steps, Tool calls, history, output tokens,
  binding admission, and root Tool-call admission remain independently bounded.
- The first profile is `exclusive` and binds one child Agent. Pooling several
  child Instances and parallel-safe delegation is a later App Composition
  change, not a Kernel change.

## Consequences

Enabling write or process Plugins for the root does not grant those
Capabilities to the child. Model replacement redirects the base child Model
binding and updates both Agent Loop configurations atomically, so root and
child remain compatible across App Generations. The native provider is trusted
code, not a sandbox; independently authored untrusted subagent providers still
require a reviewed isolated Adapter profile. A replacement child Agent must
provide both `lenso.agent@3` and `lenso.agent.turn-input@1` for the `0.4.0`
subagent Tool contract.

## Rejected alternatives

Kernel-owned child Agents would couple one product workflow to the portable
runtime. Recursive root invocation creates a dependency cycle and ambiguous
authority. Giving the child the root Tool Runtime would silently inherit every
later root Plugin. Treating the child transcript as only live stream output
would lose durable inspection and provenance.
