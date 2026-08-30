# ADR-0087: Compose remote OAuth and large Artifacts

Status: Accepted

## Context

Remote MCP authorization is credential lifecycle, discovery, audience binding,
and refresh policy. It is not transport configuration. Large Tool results are
durable payloads with retention and bounded-read policy. They are not Session
events. Making either concern an MCP or Agent Loop special case would hide its
authority and prevent another Provider from replacing it.

## Decision

`lenso.agent.oauth-access@1` is the typed Auth boundary. A consumer requests a
short-lived token for one resource URI and exact scope set, and may invalidate
that resource after an unauthorized response. Tokens are sensitive outputs and
are never persisted in the Plan or Session. The MCP client requests a token for
each HTTP request, sends it only to a same-origin endpoint, and retries once
after a 401 only after invalidating the cached token.

The default linked Auth implementation is the removable
`lenso.agent.oauth.client-credentials` Plugin. It follows MCP protected-resource
metadata discovery, OAuth authorization-server metadata discovery, the
client-credentials grant, and the OAuth `resource` parameter. Configuration
contains environment-variable names rather than client secrets. HTTPS is
required outside loopback, redirects are rejected, responses are bounded, and
scope escalation fails closed. Interactive authorization-code and PKCE flows
require a separate Auth Provider with user-interaction and durable grant state;
they are not silently approximated by this machine-to-machine Provider.

`lenso.agent.artifact@1` is the durable payload boundary. A producer writes
bytes with Session identity, media type, and a display name, then retains only
an opaque handle, digest, and size. Consumers read bounded byte ranges by
handle. The default `lenso.agent.artifact.file` Plugin uses per-Session,
content-addressed files under Agent Home, atomic writes, fail-closed path and
symlink checks, configured item and total capacity, and oldest-first retention.

The Agent Loop spills oversized Tool text, JSON, image, audio, and top-level
content through the Artifact Capability before projecting the Tool result into
Session, model, and surface events. The threshold is configurable; Artifact
storage limits remain independently configured and enforced by its Provider.

## Consequences

- MCP owns protocol mechanics but never owns OAuth credentials or durable
  payload storage.
- Another remote protocol can consume the same Auth Capability, and another
  storage backend can replace the file Artifact Plugin.
- Missing or rejected Auth and Artifact Providers fail closed at their explicit
  Capability boundary.
- Artifact retention may expire old handles; Session history remains bounded
  metadata rather than an implicit blob store.
- Browser/device authorization and remote object storage remain additive
  Providers instead of Host or Agent Loop branches.

## Proof

Capability conformance tests cover both contracts. Auth tests cover metadata
discovery, resource-bound token requests, caching, transport safety, and scope
rejection. Artifact tests cover content-addressed writes, bounded reads, unsafe
handles, capacity, and corrupt existing content. Host tests prove default
Artifact binding and opt-in MCP-to-Auth binding. A headless test proves that a
large Tool result is stored as bytes while Session events retain only its
Artifact handle.
