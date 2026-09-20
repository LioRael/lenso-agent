# External Agent Interaction Fixture

This copied, source-local fixture proves `lenso.agent.interaction@1` from a
third-party Plugin rather than the built-in Agent Loop. The Provider has a
single bounded duplex Stream: its client and Agent sequence spaces advance
independently, either direction may half-close, and an interrupt produces a
Capability terminal error.

The example does not pretend that every Provider supports reconnection. It
explicitly returns `resume_unsupported` when a caller supplies a cursor. Its
bounded `ProviderStream` capacity comes from the negotiated `max_pending_frames`
value, while the public `InteractionFlow` rejects output beyond that bound.

Run it from an Agent checkout:

```sh
python3 examples/external-agent-interaction/verify-foundation.py \
  --framework-root /path/to/framework \
  --agent-root /path/to/lenso-agent
```

This is V07 source/local evidence. The verifier only patches a temporary copy
to sibling source trees; it does not qualify released artifacts or a published
cross-target distribution.
