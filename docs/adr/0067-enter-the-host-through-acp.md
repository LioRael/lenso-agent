# ADR-0067: Enter the Host through ACP

Status: Accepted

## Context

The Harness already exposes the same Agent through headless, TUI, channel, and
Web surfaces, but editors need a standard bidirectional protocol instead of a
Lenso-specific HTTP or terminal integration. An editor entrypoint must not
build a second Agent graph, accept a client-authored Plan, or bypass the
Profile's Tool and approval policy.

ACP is a process-owning transport. Its JSON-RPC lifecycle, stdio framing, and
editor permission requests are Host surface concerns; they are not portable
Kernel behavior or a new Agent Capability.

## Decision

The Harness publishes `lenso-agent-acp`, an independent ACP protocol-v1 stdio
distribution built on the official Rust SDK.

- `lenso.agent.acp` is an endpoint-free consumer Plugin requiring exactly one
  Agent, Session, and User Interaction provider. The executable links this
  consumer beside the surface-neutral default Plugins.
- `AcpSurface` owns one durable `acp` Controller lineage. `--profile` is applied
  before immutable Generation resolution; `--plan` remains an exact diagnostic
  override and conflicts with a Profile.
- `session/new` opens a durable Lenso Session. Its absolute `cwd` must resolve
  to the process Workspace. Additional directories and client-supplied MCP
  servers fail closed because this version does not advertise them.
- `session/prompt` leases the active Generation, validates the durable Session,
  attaches the same Generation and Workspace provenance as other surfaces, and
  preserves the Profile's composed Tool set. `--allow-tool` and `--no-tools`
  can only narrow that set.
- ACP text and resource-link prompt blocks become one bounded textual Agent
  input. Images, audio, and embedded resources are rejected until the explicit
  multimodal Tool Provider slice.
- Agent text, reasoning, and Tool lifecycle messages become typed
  `session/update` notifications. `session/cancel` cancels the exact active
  Lenso invocation and returns ACP `cancelled`.
- The existing portable User Interaction broker remains the sole approval
  authority. Its exact one-shot approve-or-deny Tool interaction is projected
  as `session/request_permission`; the selected ACP option answers that exact
  Generation-bound interaction. General questions and any other interaction
  shape fail closed rather than being flattened into a permission prompt.

The entrypoint advertises only stable ACP protocol-v1 behavior. Session load,
ACP-provided MCP servers, additional roots, rich prompt media, and protocol-v2
draft features are not advertised.

## Consequences

- Editors can start the same Host and Profile over standard ACP stdio without
  adding editor or JSON-RPC concepts to Kernel.
- Session `turn_started` facts retain the exact Generation Spec digest, so ACP
  provenance is comparable with terminal, Web, and channel Turns.
- Tool approval remains one composed policy and cannot be widened by the ACP
  client.
- First-party VS Code and Zed package manifests can wrap this binary without
  owning App resolution or execution authority; that packaging remains the
  next delivery slice.

## Proof

Descriptor and Host Catalog tests prove the ACP-only consumer and Agent binding.
The stdio integration test negotiates ACP v1, opens a durable Session, streams
Tool and Agent updates, completes a real one-shot permission request, observes
the approved Workspace mutation, and reads the persisted Generation digest
from the Session event store.
