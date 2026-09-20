# ADR 0113: Compose App-authored Agent DX through verified deployment resources

Status: Accepted, amended 2026-09-20

## Context

An App Plugin can own Agent-facing behavior such as Tool Providers, a named
Profile, and instructions. Requiring every App author to hand-assemble an Agent
Home, bundle extraction, configuration sidecars, and Profile policy makes that
composition inaccessible. Allowing Agent Host startup to scan `app/**/agent/`
would make source layout an implicit runtime authority and would couple the
Agent to a particular source language or convention package.

The Engine has no Agent dependency. The Agent also has a separate identity from
Console: Console integration does not authorize a Tool Provider, select a
Profile, or grant a model permission.

## Decision

`lenso.agent.tool-conventions` remains an optional App convention support
Plugin. A selected `agent/` entry contains at most one `tools.ts` or `tools.rs`;
it may contain `profile.toml` and `instructions.md`. The entry must contribute a
Tool, a Profile, or both. `instructions.md` without `profile.toml` fails with a
specific diagnostic. A Tool source lowers to the ordinary Agent Tool Provider
Bundle and a `lenso.agent.deployment@1` document. A Profile-only entry lowers to
the generic Engine resource-only convention output and a
`lenso.agent.deployment@2` document. It does not create an empty Provider,
Bundle, runtime artifact, or Plugin Root directory.

Both forms use the Engine's generic published-resource inventory. Engine copies
and hashes declared bytes but does not interpret Agent semantics. For a
resource-only convention output, Engine assigns the selected surface's
contribution identity as the inventory owner; that identity is not a runtime
Plugin identity or an activation mechanism.

The v1 deployment document contains the generated Plugin ID and default
instance, plus an optional named Profile source and Markdown instructions. The
v2 Profile-only document contains the Engine-assigned contribution identity and
one named Profile source. The Profile source ends with an authoring-only
`[lenso]` table containing exactly its profile name, and explicitly contains
`instances` and `allowed_tools`. The Agent injects a generated Tool Provider
only for a v1 Tool contribution. A v2 Profile-only import preserves its
declared instances unchanged. Markdown cannot silently replace a nonempty inline
instruction field.

`lenso-agent dx check --from <app-dist>` is read-only. It verifies the generic
resource inventory digest, resource schema and contribution owner, deployment
identity, and Profile source shape. It verifies a matching Bundle manifest only
when the deployment actually contributes a Tool Provider. `lenso-agent dx apply
--from <app-dist>` is an explicit local installation operation. It is available
only for an unmanaged Agent Home. It holds the existing Plugin Root mutation
fence, rechecks the unmanaged authority, stages copies of the Plugin Root and
Profiles, securely extracts and verifies Bundles, renders Profiles, resolves
them through the normal Host Ready Gate, and then publishes each new Plugin and
Profile without overwriting an existing path.

The installed Tool Provider has `default.disabled`. A generated Profile
explicitly selects that instance and narrows the Tool allowlist. A Tool
contribution without a Profile remains disabled until a user configures it using
the normal Agent Plugin lifecycle. A Profile-only contribution selects only its
author-declared existing instances and cannot inherit tools, instructions, or
permission from its source directory. App source, a successful build, `dx
check`, and `dx apply` therefore never grant default-model access.

Managed SQLite/remote configuration authority is not bypassed. The operation
fails instead of falling back to filesystem writes; managed import can be added
as a separate authority-owned operation later.

## Ownership

The optional convention owns source lowering and its deployment schema. Engine
owns generic resource publication and byte integrity only. Agent Host owns
Agent Home mutation fencing, Bundle admission, Profile resolution, Runtime
readiness, and Tool policy. Tool Providers own Tool implementation. A selected
Profile owns which configured instances and instructions appear for one Agent
run. Credentials, model selection, Session identity, and final business
authorization remain with their existing owners.

## Consequences

The author experience is a small language-neutral directory convention while
the runtime remains based on normal Plugins, Profiles, Bundles, and Capability
contracts. A future Python, Go, Swift, or UI convention can publish its own
schema through the same generic Engine inventory; it does not require a new
Agent source scanner. Tool Bundle dependency closure remains per selected
Plugin, and resource-only results bypass all Tool language dependency setup.

`dx apply` is intentionally not a deployment system for managed or remote
Agent Homes, an upgrade operation, or a substitute for Plugin policy review.
It rejects duplicate targets rather than replacing an installed contribution.
Cross-directory publication is individually atomic after complete staged
validation; recovery or replacement remains an explicit Agent lifecycle
operation.

## Verification

Compiler tests prove that Tool source is not executed during convention
lowering, ambiguous language entries fail, Rust lowers to portable Wasm and
Process implementations, and Profile-only lowering emits only generic
resources. Engine tests cover bounded resource-only outputs and exact inventory
publication without an implicit Bundle. Host tests cover Profile preservation,
policy validation, v1 Bundle admission, v2 no-Bundle inspection, and restored
Process execution permissions after secure Bundle extraction. The App examples
exercise real Bun and Rust Tool Provider Bundles, `dx check`, `dx apply`,
Profile-only visibility, denied invocation, persisted Session evidence, and
removed-provider failure. These tests do not prove a remote managed-authority
import, registry publication, or third-party App compatibility.
