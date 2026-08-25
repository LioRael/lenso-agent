# 0024 — Derive typed Agent Tool Providers

## Status

Accepted.

## Context

Writing one Agent Tool still required authors to repeat its name, description, input Schema,
catalog entry, JSON decoding, dispatch branch, and `InvalidArguments` mapping. That repetition sat
above the portable Tool Provider Capability and obscured the actual product behavior.

The generic `lenso` facade cannot own an Agent-specific Tool annotation without making the Runtime
depend outward on this product Capability. The Agent Harness also must not restore a parallel
Module factory, Descriptor, endpoint, or registration authority.

## Decision

The product owns a focused, workspace-local `lenso-agent-tool-sdk`. Its `#[tool_provider]` macro
wraps the canonical `#[lenso::provides(ToolProvider)]` facade and derives only Tool-level catalog
and dispatch code from typed methods marked with `#[tool(name = ..., description = ...)]`.

Argument types derive `JsonSchema` and `Deserialize`. The generated dispatcher maps malformed or
schema-incompatible JSON to the portable `InvalidArguments` Domain Error, preserves Tool-returned
Domain Errors, and returns `NotFound` for unknown names. Synchronous and asynchronous methods and
multiple Tools per Provider are supported. Provider-local duplicate Tool names fail compilation.

The SDK does not own Capability contracts, Module Descriptors, factories, Host registration,
global discovery, or graph mutation. Those remain with the source-first Capability and `lenso`
facade. The SDK stays unpublished until its product Capability packages have a registry baseline.

## Consequences

The text-tools fixture becomes a typed behavior method plus metadata, while its generated Module
Descriptor and portable Capability remain unchanged. Agent Tool authors use a product-level
extension rather than adding Agent concepts to the generic Runtime facade.
