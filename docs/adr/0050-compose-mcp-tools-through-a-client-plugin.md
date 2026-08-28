# ADR 0050: Compose MCP Tools through a client Plugin

## Status

Accepted.

## Context

Third-party MCP servers are useful integration endpoints, but MCP is a
protocol with transport, negotiation, process lifecycle, cancellation, and
multiple feature families. Adding MCP branches to the Agent Loop or treating
an entire server as one opaque Tool would couple the Harness to protocol
mechanics and erase the existing Capability boundaries.

Modern MCP uses per-request metadata while deployed legacy servers require an
`initialize` handshake. A usable stdio client must detect both eras without
poisoning the real legacy session, namespace untrusted remote names, bound
messages and catalogs, and clean up its child process when a Generation is
retired or a request is cancelled.

## Decision

The linked, opt-in `lenso.agent.mcp-client` Plugin occupies the existing
`tool-providers` root Slot and provides only
`lenso.agent.tool-provider@2`. One configured Instance owns one exact stdio
program, its arguments, working directory, allowlisted inherited environment,
protocol mode, Tool namespace, and timeouts.

`protocol = "auto"` probes `server/discover` in a disposable child. A modern
response selects per-request metadata. Any non-modern error or timeout selects
a fresh child and the legacy `initialize` flow. The real child remains active
for the Instance so state and request ordering remain connection-local. An
unexpected protocol or process failure closes that session; the next Tool call
opens and initializes a clean replacement. Deactivation closes stdin, waits
briefly, then kills and reaps a child that does not exit. Cancellation emits
`notifications/cancelled` before the same cleanup.

Activation paginates `tools/list` behind bounded page, count, message, Schema,
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
- Tool-list change notifications do not mutate a running immutable Generation;
  refreshing a catalog requires ordinary Plugin reconciliation.
- Streamable HTTP and the non-Tool MCP feature families remain separate future
  vertical slices.

## Proof

Tests launch real stdio fixture processes and prove modern discovery, fallback
to a clean legacy session, Tool projection and invocation, malformed catalog
failure, Tool failure mapping, cancellation notification, process termination,
and reaping. Host tests prove the Plugin is linked but absent from defaults and
can be selected by an ordinary Plugin Root Instance.
