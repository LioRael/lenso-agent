# ADR 0109: Provider-owned business identity at turn admission

Status: Accepted

Business App grants belong to the removable business connection Plugin. The Host
must fix the identity before returning a Turn lease, while the App remains the
final authority for revocation, expiry, audiences and resource permissions.

The private native `lenso.agent.turn-binding@1` Capability captures provider state
under a random Host-issued scope ID. The Agent Loop declares an optional Many
requirement; the Host invokes those exact Plan-bound dependencies at admission.
No provider means no change to an ordinary Agent. Capture failure rejects the
admission. The cancellation token passed to capture is a lease lifetime token:
completion of capture does not release it. Dropping the Turn lease cancels it,
including partial capture and timeout failures. Providers release snapshots on
cancellation and impose their own finite capacity and lifetime bounds.

Only the opaque scope ID enters InvocationContext. Grants, credentials, URLs and
user identity do not enter RunScope, Session history, Tool arguments or hooks.
Tools reject missing, expired or unknown bindings instead of using whichever
account is currently logged in. Child calls may inherit the parent's binding;
they cannot select a newer account. Browser auth status and poll return no grant.

This supplements ADR 0108 for Plan-bound Plugins. It does not create a Kernel
registry, mutable Plan, cross-process context format or portable credential API.
The native provider is trusted code; an opaque scope is not a sandbox boundary.
