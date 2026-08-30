# ADR 0050: Compose MCP Tools through a client Plugin

## Status

Accepted.

## Context

Third-party MCP servers are useful integration endpoints, but MCP is a
protocol with transport, negotiation, process lifecycle, cancellation, and
multiple feature families. Adding MCP branches to the Agent Loop or treating
an entire server as one opaque Tool would couple the Harness to protocol
mechanics and erase the existing Capability boundaries.

Modern MCP uses per-request metadata and Streamable HTTP while deployed legacy
stdio servers require an `initialize` handshake. A usable client must detect both eras without
poisoning the real legacy session, namespace untrusted remote names, bound
messages and catalogs, and clean up its child process when a Generation is
retired or a request is cancelled.

## Decision

The linked, opt-in `lenso.agent.mcp-client` Plugin occupies the existing
`tool-providers` root Slot and provides only
`lenso.agent.tool-provider@2`. One configured Instance owns either one exact
stdio program or one Streamable HTTP endpoint, plus protocol mode, Tool
namespace, and timeouts.

`protocol = "auto"` probes `server/discover` in a disposable child. A modern
response selects per-request metadata. Any non-modern error or timeout selects
a fresh child and the legacy `initialize` flow. The real child remains active
for the Instance so state and request ordering remain connection-local. An
unexpected protocol or process failure closes that session; the next Tool call
opens and initializes a clean replacement. Deactivation closes stdin, waits
briefly, then kills and reaps a child that does not exit. Cancellation emits
`notifications/cancelled` before the same cleanup.

The modern Streamable HTTP transport sends one POST per JSON-RPC request with
the required protocol, method, and name headers. It accepts bounded JSON or
request-scoped SSE responses, closes the request on cancellation, rejects
redirects, supports valid `x-mcp-header` parameter projection, and permits only
HTTPS endpoints or explicit loopback HTTP. Authorization can name an
environment variable containing the full header value, or request a
resource-bound token through `lenso.agent.oauth-access@1`. The latter keeps
discovery, credentials, caching, and refresh in a removable Auth Plugin. The MCP
Plugin never persists either credential form in Plugin TOML or Session state.

Activation and each Turn catalog request paginate `tools/list` behind bounded page, count, message, Schema,
and text limits. Remote names are normalized into lowercase snake case and
become `mcp__<namespace>__<remote_name>`; collisions fail readiness. Every projected
Tool is `exclusive`: the remote protocol does not supply a portable execution
safety classification. Invalid catalogs fail the Ready Gate. JSON-RPC errors,
MCP Tool errors, unsupported client requests, non-text results, and output
limits remain explicit Domain or Runtime failures rather than fallback data.

This Plugin does not flatten MCP Prompts, Resources, Elicitation, Sampling, or
Roots into Tools. Future adapters may project those features through their
matching typed Harness Capabilities. Agent Loop, Tool Runtime, Kernel, Profile,
and Plugin Root resolution contain no MCP-specific branch.

## Consequences

- One Profile can select an MCP Instance while another selects a differently
  configured Instance of the same Plugin.
- Removing the Instance removes its process, protocol state, catalog, and every
  namespaced Tool.
- Native MCP servers are trusted child processes, not a security sandbox.
- Tool-list changes are visible at the next Turn catalog request; the currently
  admitted Turn keeps its immutable Tool set and Generation.
- Non-Tool MCP feature families remain separate vertical slices because MCP
  gives Prompts, Resources, Elicitation, and Sampling different control
  authority from Tools.

## Proof

Tests launch real stdio fixtures and a Streamable HTTP endpoint and prove modern discovery, fallback
to a clean legacy session, Tool projection and invocation, malformed catalog
failure, Tool failure mapping, cancellation notification, process termination,
and reaping. Host tests prove the Plugin is linked but absent from defaults and
can be selected by an ordinary Plugin Root Instance.
