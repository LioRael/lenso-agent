# ADR 0006: Admit passive Plugin releases before executable Plugins

- Status: accepted
- Date: 2026-08-24
- Relates to: ADR 0005 and Lenso ADR 0065

## Context

The Runtime control plane can validate immutable Plugin Bundles, Admission
Receipts, Plugin locks, selected artifacts, Host policy, and Generation
identity. The current Harness Host has accepted only its statically linked
native Adapter. The upstream native built-in Plugin path does not yet close the
control-plane factory identity over the Native Adapter's package revision, and
the Wasm Component, QuickJS, and native-dylib Adapters remain preview execution
classes without Harness product acceptance.

Admitting an executable contribution under those conditions would make the
lock appear stronger than the executable path actually is.

## Decision

The Harness initially admits only passive Plugin releases:

- target-scoped immutable artifacts;
- product-owned inert metadata; and
- optional Feature selections that close exactly over those two kinds.

`plugins install` requires a directory Bundle and explicit bounded local-review
evidence. The loader rejects symlinks, undeclared files, traversal, excessive
entry depth/count/bytes, and missing Manifest closure. The generic Store then
verifies canonical Manifest, artifact size/digest, Product Metadata digest, and
admission policy.

Activation is a separate atomic commit. `.lenso/plugins/active-set.json`
contains one nested canonical `PluginSetLock`, the exact Manifest values, and
Admission Receipt digests. Startup revalidates the complete closure from Store
before resolving the initial Generation. Module, Data mount, permission, and
binding contributions fail admission.

## Consequences

- Store admission alone never makes a release active.
- A failed activation may leave an immutable admitted Store object, but no live
  authority points at it.
- The same Plugin ID cannot silently move to a different Manifest; upgrade and
  uninstall need explicit future transition commands.
- Selected passive artifacts participate in the Generation artifact-set and
  Generation spec digests without adding hidden Module Instances or bindings.
- Executable Plugin support requires an accepted Adapter/catalog path and real
  host evidence before the admission policy is widened.

## Rejected alternatives

### Enable every transitive preview Adapter

Package presence does not establish support policy, isolation, codecs,
Capability compatibility, cancellation, or shutdown evidence.

### Treat an admitted Bundle as immediately active

That would collapse immutable storage and App selection into one mutation and
leave no atomic, reviewable Plugin Set authority for the next Generation.
