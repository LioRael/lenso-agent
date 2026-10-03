# ADR 0121: Console member provider leases

Status: Accepted

Console member settings select only Host-configured provider IDs, authorized
against the verified issuer/subject pair. An empty provider policy denies
member access. A shared provider, explicit user assignments and configured
group membership can grant provider choices. Group membership is platform
configuration; a request cannot invent its own subject or groups. Removing a
grant invalidates saved preferences immediately.

Each configured provider runs in a separately prepared Host Home using its
complete Plugin Root. Member provider policies reject named Profiles: their
Instance selection can exclude or replace the durable providers checked below.
Platform administrators configure each provider's Model Instance in its own
Home. Existing local operator named Profiles remain supported. Turns select
already prepared Hosts rather than switching a shared Host's active Profile.
Every provider Home explicitly
configures the same authenticated Session Plugin database and verification
authority. Artifact providers share the same absolute storage directory and
verification authority. Interaction providers use that authority as well.
The base surface Home is distinct from all provider Homes and has the same
durable boundaries for history and attachment routes. Canonical Home aliases
are rejected before boot. Runtime ledgers remain distinct. Existing local
operator mode uses its original Host when no member provider policy is configured.

Optional BYOK in this slice uses the OpenAI-compatible Model Plugin's existing
Secrets consumer and admits only configured provider endpoints and models. A
member submits a credential, never an endpoint or Plugin Instance mutation.
The settings authority writes an Age-encrypted version-1 Secrets file in a
new issuer/subject-derived, versioned Home and configures the existing
encrypted-file Secrets Plugin to resolve the model's logical reference.
The platform passphrase remains in the configured environment source. No
credential enters SQLite settings, Session events, API responses or debug
representations. A new immutable lease replaces future admissions; existing
turns keep their original Host and credential version. Deleting BYOK removes
the member's active metadata mapping; existing leases drain before old
encrypted versions can be reclaimed.

Durable quota reservations bound all BYOK revisions, including older encrypted
versions retained by draining turns. Per-user and total revision limits are
configurable; exhausted capacity rejects uploads before encryption. Failed
uploads release their reservation and remove the new Home.
If deletion itself fails, the reservation remains counted until operator
cleanup; uploads cannot bypass storage limits through cleanup failures.
Deleting current BYOK metadata does not reset the revision budget. Provider
Host caches have a separate configured bound. Background reclamation of drained versions is not
part of this slice; bounds prevent unbounded retained Hosts and encrypted data.

The existing removable `lenso.agent.web` Plugin is the logical owner of
preferences and BYOK-version metadata. `AssistantProviderService` is its Web
Host surface adapter's private configuration authority, following the existing
configuration-store authority boundary. Only this mounted surface reads and
writes the store. Removing the Web Plugin removes the required surface route;
there is no alternative settings or conversation fallback.

Its SQLite tables contain only preferences and encrypted-version metadata,
keyed by the exact JSON issuer/subject pair. Settings fail closed
when storage is unavailable. Reads and writes have no owner argument in the
public request. Member settings cannot modify the platform Plugin Root.

Member Tool configuration defaults to an empty list. The initial supported
grant is `ask_user`, whose User Interaction provider verifies the same signed
owner. Other Tool names are rejected at configuration validation; ordinary
native filesystem, Process, Bun and subagent Tools do not acquire a member
authorization boundary merely by being included in a list.

This is a trusted native Host boundary and durable ownership policy. Native
Rust, Bun and ordinary Process Plugins remain trusted code; separate Homes
and process execution do not provide operating-system isolation.
