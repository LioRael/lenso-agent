# ADR-0085: Project deferred and structured Tools

Status: Accepted

## Context

Large MCP catalogs consume model context before any Tool is relevant. MCP Tool
results can also contain JSON, images, audio, links, and embedded Resources,
while the aggregate Tool contract previously preserved only text.

## Decision

When no explicit Run Scope exists and more than sixteen MCP Tools are present,
the Agent Loop withholds their definitions and exposes one synthetic
`tool_search` Tool. Search loads at most eight matching definitions into later
model steps. Discovery changes model visibility only; execution still requires
the immutable Plan-bound Tool catalog.

Tool Provider 2.1 and aggregate Tools 2.2 add optional structured content
blocks for text, JSON, image, audio, resource links, and artifacts. MCP projects
native result content into those blocks and keeps a text fallback. Session,
surface, and model projections use a bounded representation. Oversized payloads
are written through `lenso.agent.artifact@1`; the projections retain an opaque
handle, media type, digest metadata, and size instead of inline bytes.

MCP additionally supports configured Roots, Sampling Tools, binary Resources,
and an explicit per-remote-Tool parallel-safe allowlist. Unlisted MCP Tools stay
exclusive.

## Consequences

- large MCP installations no longer front-load every definition;
- binary and structured results survive the Tool boundary without pretending
  to be plain text;
- bounded projections do not copy arbitrary base64 payloads into prompts or
  Session event metadata; and
- concurrency remains opt-in and auditable.

## Proof

Capability freshness gates verify generated Descriptors, Schemas, and Rust
bindings. MCP tests cover namespacing, structured result projection, binary
Resources, Sampling Tools, Roots capability advertisement, and explicit
parallelism. Agent Loop tests cover deterministic Tool projection and bounded
surface metadata.
