# ADR 0051: Compose MCP Prompts and Resources as Context Sources

- Status: accepted
- Date: 2026-08-29

## Context

MCP assigns different control semantics to its server features. Tools are
model-controlled, Prompts are user-controlled, and Resources are
application-controlled. Projecting every MCP feature into the Agent Tool
catalog would make Prompt selection and Resource attachment model-controlled
and would hide their provenance behind an unrelated Tool call.

The existing Harness Prompt Provider is also not a match. It contributes the
reviewed System Instrument installed at Session start. An MCP Prompt is selected
for a particular task and must never mutate that Session-wide instruction.
Workspace Read is likewise authority over local paths, not a generic remote
Resource catalog.

## Decision

Define portable `lenso.agent.context-source@1` with three bounded request
operations: snapshot Prompt and Resource metadata, render one explicitly named
Prompt with JSON arguments, and read one explicitly named Resource. Rendered
Prompts preserve user and assistant roles. Resource reads preserve URI and MIME
metadata. Version 1 admits UTF-8 text only and rejects binary or embedded
content it cannot represent faithfully.

The existing `lenso.agent.mcp-client` Plugin provides Context Source alongside
Tool Provider. One configured transport and protocol session therefore owns
Tools, Prompts, and Resources without introducing another Plugin type or MCP
connection. Its configured namespace is the source identity, list operations
are paginated and bounded, and catalogs refresh when requested. A server may
provide any non-empty combination of Tools, Prompts, and Resources.

CLI and TUI presentation Plugins bind Context Source with `many` cardinality.
The CLI accepts an explicit Prompt plus JSON arguments and repeated Resources.
The TUI snapshots metadata into its bounded `/` catalog; it offers no-argument
Prompts and text Resources, then resolves the selected item only when the user
submits a task. Both surfaces label the selected content before passing it as
task context. This keeps MCP Prompt/Resource content out of the System
Instrument and prevents silent model authority expansion.

MCP Elicitation and Sampling are not Context Source operations. They require a
separate continuation coordinator over User Interaction and Model capabilities;
they must not be represented as Tools or hidden inside a Resource read.

## Consequences

- MCP metadata and content have a typed Harness seam independent of transport.
- Native, Process, QuickJS, and Wasm adapters can implement the same portable
  contract.
- Text-only v1 fails explicitly for images, audio, blobs, and embedded
  resources instead of dropping information.
- MCP request continuations remain a separate authority review.
