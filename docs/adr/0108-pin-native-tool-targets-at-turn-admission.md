# ADR 0108: Pin native Tool target authority at Turn admission

## Status

Accepted for the native Host bridge. Business-user connection implementation and
end-to-end Projects acceptance remain separate required work.

## Decision

The Host captures its native Tool target when it admits a Turn, before returning
its Generation lease. A mutable connection adapter returns an immutable target
snapshot, including the disconnected state. The default is only for adapters
whose authority is already immutable. The existing App Agent adapter snapshots
its prepared catalog, routes and native HTTP clients.

The Turn owns a native lease. Invocation context carries only an opaque random
lease identifier, never a credential, authorization header or account claim.
The bridge routes scoped calls to that lease's target. It fails closed for an
unknown, malformed, foreign or released identifier; it never substitutes the
currently selected connection. Unscoped operator catalog requests retain their
existing routing. This identifier is a native routing handle, not authentication
proof and not a public wire contract. HTTP ingress must not accept client-supplied
InvocationContext extensions.

A Host-local bounded table belongs to the existing native Tool bridge. It is not
a Plugin registry or durable store. Dropping the Turn releases its entry and held
snapshot. In-flight invocations may retain an already resolved target until they
finish. Process restart invalidates all handles; resumed Turns capture fresh
connection authority. A login change affects future Turns only. Revocation and
expiry are still checked at authenticated business ingress on each operation;
pinning identity does not extend a credential's validity.

Connection Plugins own login, credential storage and revocation policy. Projects
owns final authorization. The Host owns only the lifetime of an admitted native
adapter snapshot. Kernel, resolved Plans, model input and Session schemas gain
no credential or connection policy.

## Verification

Bridge regressions cover switching identity before the first Tool call,
independent concurrent Turns, cleanup, capacity, cancellation, stale/foreign
references and credential-free diagnostics. These tests do not prove browser
login, a business Tool HTTP endpoint, real model use or npm publication.
