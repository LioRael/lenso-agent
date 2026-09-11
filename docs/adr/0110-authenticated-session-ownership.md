# ADR 0110: Authenticated Session ownership

Status: Accepted

The Session Plugin owns durable conversation ownership. Console authentication
and a hidden navigation entry do not authorize access to an Agent's shared Home.
The SQLite Session Plugin can require an Auth assertion using immutable issuer
and verification-key configuration. It independently verifies the signature,
issuer, operation audience, validity, and user actor kind on every invocation.
Identity is never accepted from a tool argument or an unsigned subject header.
An assertion sent to an unconfigured provider is rejected rather than entering
its local scope.

Owners are unambiguous pairs of issuer and subject. They are inserted in the
same transaction as Session creation, cannot be reassigned by reopening a known
identifier, and are checked for open, append, read, list, and rename. Listing
filters before applying the limit. Unauthorized identifiers return the same
NotFound result as absent identifiers. Worker admission captures the verified
owner with the command instead of consulting a mutable current account.

Schema version 2 adds the ownership table. Existing sessions remain ownerless
and are available only in the original local scope. The local inspector does
not expose owned sessions; archive import creates ownerless sessions and never
overwrites an existing identifier. Older binaries reject version 2 on startup.
This migration requires draining old writers before admitting member traffic;
it is not a live-upgrade security boundary against an already running old binary.

This is one prerequisite, not a claim that Agent Web is multi-user ready. An
embedding surface must verify its own ingress, preserve identity across turn
admission, continuations and child calls, and arrange assertion renewal or stop
on expiry. Attachment, interaction, task and tool providers require their own
explicit authorization paths. Host inspection must not replace authorized
Session calls. Profiles only narrow existing authorization, and native code or
Agent Home directories are not isolation sandboxes. Member Agent navigation
remains disabled until these boundaries have end-to-end negative tests.

## Provider and Host implementation

File Artifact storage independently verifies the actor and operation, then uses
an issuer/subject-derived directory. Artifact handles stay session-relative;
knowing a handle does not select another user's directory. Capacity and pruning
apply inside the selected owner's directory. Local artifacts are not migrated
into a user's directory. This is data ownership, not native filesystem isolation.

The local User Interaction Plugin scopes pending IDs, listing, answers, and
cleanup by the same verified identity. Equal interaction IDs in different users
cannot collide or consume each other's answers. Its configured total pending
limit remains a shared resource bound.

The Host retains the assertion in one immutable turn lease and attaches it to
provider identity capture, Session operations, normal invocations, and task
snapshots. Its debug representation redacts assertion material. It does not
refresh, invent, or switch the identity midway through a lease. The public
embedding API carries the assertion; ingress and target providers still own
verification. The existing operator business-connection Plugin rejects member
capture instead of substituting its global browser-consent grant.

Authentication rejection is a domain error, never a Plugin runtime failure.
Session 1.9, Artifact 1.1, and User Interaction 2.1 expose `permission_denied`.
A rejected request must leave the provider available for the next authorized
request. The native Host and business-connection tests exercise that sequence.

## Ownership and deletion

SQLite Session owns the ownership table and event log; removing it removes the
Session implementation, not an alternate Console copy. File Artifact owns its
namespaced bytes and quotas. Local User Interaction owns only ephemeral pending
questions and cancellation cleanup. Auth owns signing, login and grant issuance;
these providers consume verification keys, not signing secrets. Host owns the
lease carrying the assertion and never stores that assertion in Session events.
All implementations here are linked native Rust; no new portable identity
transport or hostile-code execution boundary is claimed.
