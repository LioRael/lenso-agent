# ADR 0110: Console Agent owns plugin installation interaction

Status: Accepted

## Decision

Marketplace owns discovery, signed metadata and release presentation. It does not
connect to Agents, accept their credentials or proxy installation. Users request
installation in Console Agent. The existing Console Plugin Tools Plugin exposes
`list_available_plugins`, `check_plugin_install`, `apply_plugin_install`, and the
read-only `get_plugin_installation` through `PluginManagementTarget`. The target
is always explicit; unknown targets never fall back to Console.

The selected local authority owns operator-configured catalog trust, allowed
artifact origins, catalog checkpoints, verified archive cache and retained
installation proposals/receipts. Marketplace cannot select trust anchors or
execution authority. Verification is shared through CLI authoring APIs; directory
moderation and presentation remain Marketplace-owned. No installation enters Kernel.

Catalog entry IDs bind catalog, plugin and exact version. Preparation verifies the
signed release and exact archive identity, resolves a private candidate and binds
review to target identity, base/candidate revisions, configuration and expiry.
Apply refreshes signed metadata, rechecks admission and the reviewed candidate
under Plugin Root/Generation fences, then publishes atomically. A proposal ID is
the idempotency key: retries read the retained operation, never publish again.

Publication is not runtime readiness. The target journal records the runtime
operation and follows activation or failure. After restart, the active revision
can prove success; missing evidence requires recovery. A read never retries a
mutation. Runtime failure retains the previous active Generation; it does not
undo the desired filesystem publication. Repair/removal remains an explicit
reviewed operation. Installing a provider does not grant permission to its tools.

## Runtime seam

The target router holds a weak link to the surface runtime, bound after startup,
so it cannot retain a Host/channel cycle. Native management tools use the existing
runtime command channel. Direct tool execution services Plugin control messages
while waiting, preventing a tool from deadlocking on its own inventory or receipt
request. Other commands remain bounded and deferred; shutdown interrupts waiting.

## Limits and compatibility

The first signed-catalog implementation supports the built-in local authority.
Remote authorities retain their existing explicit routing and support decisions.
Catalog URLs and signing keys are operator configuration, not model arguments.
Metadata requires HTTPS (or explicitly configured loopback HTTP), no redirects,
no ambient proxies, a 30-second transport timeout and a 4 MiB bound. Artifact
transport additionally rejects private-network destinations and verifies size,
archive hash and canonical manifest identity before resolution.

There are at most 256 returned entries, 128 outstanding proposals, a 512 MiB
archive cache and ten-minute reviews bounded by signed metadata expiry. First
installation uses the plugin's default configuration; updates preserve existing
instances. Plugins needing additional configuration can fail candidate validation
and require a supported configuration/authoring workflow. Native/Process code is
trusted code with Host privileges, not a sandbox.

The additive `installation` operation is generated from the Capability source;
Host bridge and Console tools ship together. Source-cohort Cargo pins are
immutable development prerequisites and must be replaced with released minimum
versions before registry publication.

## Acceptance

The real Console tool-provider acceptance reads signed metadata over loopback
HTTP, rejects expired/wrong-target/wrong-review requests, installs and invokes a
Process plugin, reuses the receipt on retry, recovers after restart, upgrades its
behavior, observes a failing candidate while the old tool works, and removes the
plugin. Artifact bytes in this fixture are preverified cache entries. Separate
CLI HTTPS tests cover download policy and integrity. No Market-to-Agent request
or credential exchange participates.
