# ADR 0052: Coordinate MCP request continuations in the client Plugin

- Status: accepted
- Date: 2026-08-29

## Context

MCP 2026-07-28 can pause `tools/call`, `prompts/get`, or `resources/read` with
an `input_required` result. Elicitation requests need user-owned interaction;
Sampling requests need model authority. Treating either as a Tool would make a
nested server request model-controlled and would conceal which MCP Instance
requested the action. Adding either to the Agent Loop would make an optional
integration a permanent Host concern.

Sampling is deprecated in MCP 2026-07-28. It remains useful for compatibility
with existing servers, but a new Profile must not obtain model-spend authority
merely by adding an MCP transport.

## Decision

The `lenso.agent.mcp-client` Plugin coordinates request continuations while
requiring the existing portable User Interaction and Model capabilities. Both
families are disabled by default and advertised to a modern MCP server only
when their Profile policy enables them.

Elicitation supports form mode and HTTPS URL mode. It identifies the requesting
MCP interaction, always offers decline/cancel, never fetches a URL, and validates
accepted form JSON against the requested schema before returning it. The active
surface owns presentation and consent.

Sampling is an opt-in compatibility path requiring one exact configured model,
a token ceiling, text-only messages, no Tools, and no context inclusion. Server
model hints are advisory and cannot override the Profile-owned model. Outputs
are bounded text responses. Profiles should prefer a direct model integration
for new designs.

The coordinator supports at most eight inputs per round and a configurable
one-to-eight continuation rounds. It retries with a new JSON-RPC request while
echoing `requestState` byte-for-byte and never interpreting that state. Legacy
protocol sessions reject continuation results.

## Consequences

- MCP remains one removable Plugin Instance with explicit Capability edges.
- Headless Profiles can leave Elicitation disabled; interactive surfaces can
  provide it without an MCP-specific UI branch.
- Enabling Sampling makes model usage visible in reviewed Profile configuration
  and does not grant nested Tool execution.
- `roots/list`, binary Sampling content, Sampling Tools, and deprecated context
  inclusion are not advertised and fail closed if requested.
- Removing the MCP Instance removes its transport, projected features, and
  continuation authority without changing Kernel or Agent Loop behavior.
