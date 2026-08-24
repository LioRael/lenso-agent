# ADR 0011: Record Generation provenance in Session events

- Status: accepted
- Date: 2026-08-25
- Refines: ADR 0005

## Context

Every Agent Turn already holds a lease for one exact App Generation, but that
identity previously stopped at the Host route table. The Agent request exposed
only user input and an optional Session ID, while `turn_started` recorded only
the input. After a future Plugin upgrade, one resumed Session could therefore
contain Turns from several Generations without a durable way to identify which
immutable graph produced each Turn.

Recording only a synthetic ID in the Session would not be sufficient. The
referenced canonical Generation Spec must survive active Plugin Set replacement
and must fail closed if its content-addressed record is corrupted.

## Decision

Before staging the initial Generation, the Harness Host writes its canonical
`AppGenerationSpec` bytes to
`.lenso/plugins/generations/<sha256-hex>.json`. The filename is derived from the
Spec digest. An existing regular file must match the exact canonical bytes;
symlinks, non-files, or mismatched content reject App startup.

When the Host leases a Turn, it attaches the lease's canonical
`sha256:<hex>` Generation Spec digest to the root Invocation Context under the
Harness-owned `lenso.app.generation-spec-digest@1` extension. The public Agent
request Schema does not expose this value. The Agent Loop requires the
extension before opening or appending a Session and adds it to every
`turn_started` payload as `generation_spec_digest`.

The Session Capability Descriptor remains unchanged. Session events already
carry bounded opaque `payload_json`; this slice changes the Agent Loop's owned
event meaning rather than the portable Session provider role. History
reconstruction continues to use the existing `input` field and tolerates the
additional provenance field.

## Consequences

- Every Session-recorded Turn begins with a durable reference to the exact
  Generation leased for that Turn.
- Resuming one Session after the active Plugin Set changes records a new digest
  without rewriting earlier events or Generation Specs.
- Generation Specs contain digests, not credentials, provider bodies, Plugin
  Manifests, or full Resolved Plans. Secret values remain outside both stores.
- A caller that bypasses the Harness Host cannot start an Agent Turn without
  supplying canonical Generation provenance. The extension is product-owned
  caller context, not a hostile-code authentication boundary for trusted native
  Modules.
- ADR 0012 adds Plugin upgrade and manual rollback, ADR 0013 adds provenance
  inspection, and ADR 0014 fences local Host authority across processes.
  Automatic rollback, distributed coordination, retention, and garbage
  collection of unreferenced Generation Specs remain separate slices.

## Rejected alternatives

### Add the digest to `RunTurnRequest`

That would make a Host-owned routing fact user-supplied and forgeable through
the public Agent Capability.

### Copy the complete Generation into every Session event

That would duplicate large immutable authority documents, increase Session
retention cost, and blur ownership between the Host control plane and Session
storage.

### Record only the active Plugin Set digest

A Generation also closes the Host build, execution policy, resolved Plan,
artifacts, and effective grants. The Generation Spec digest is the existing
authority over that complete closure.
