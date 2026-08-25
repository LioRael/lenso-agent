# ADR 0019: Bind reviewed Wasm Tools to one Host-selected workspace reader

## Status

Accepted.

## Context

ADR 0018 deliberately admitted only Wasm Tool Providers with no Host imports. A useful third-party
Tool sometimes needs workspace context, but importing the existing generic Tool Provider contract
would make the authority hard to name and would re-enter the same single-concurrency Capability
while the Plugin Tool is executing. Direct WASI filesystem access would also bypass Composition.

## Decision

The Harness owns a portable `lenso.agent.workspace-read@1` Capability, Descriptor `1.0.0`, with one
request Operation: `read_text`. A dedicated `lenso.agent.workspace-import-read` Module provides it
without changing the existing Agent Tool Provider Module. The contract keeps workspace-root resolution,
symlink rejection, byte limits, storage, and filesystem implementation private.

The Plugin Profile Catalog admits one additional package-independent Wasm Tool shape. It is the ADR
0018 shape plus exactly one `lenso.agent.workspace-read@1` requirement. The Host Profile, not the
Bundle, binds that requirement to base Instance `workspace-import-read` only when its package identity is
`lenso.agent.workspace-import-read`. The resulting Capability binding is included in the immutable
Generation Plan. Explicit review evidence remains mandatory.

The shape still rejects permission requests, state, Data mounts, custom binding templates, extra
Capability requirements, and arbitrary provider selection. It grants no process, network, Secrets,
workspace write, or ambient filesystem authority.

The standalone example depends on the pinned guest SDK rather than Harness paths. Its lifecycle
test builds outside the workspace and proves authority-expansion rejection, install, a real imported
`read_text` call, upgrade, rollback with another call, removal, and loss of the Tool.

## Consequences

- Product code can accurately describe this as a reviewed, Host-selected read-only workspace import.
- A Bundle cannot redirect the import to another provider or turn the fixed profile into a general
  permission request.
- The dedicated Capability avoids recursive Tool Provider admission and can be generated for guest
  consumers from one Descriptor and Schema source.
- Adding list/search, another provider, optional/many cardinality, workspace write, process, network,
  Secrets, state, or Data mounts requires another explicit contract and Host Profile decision.
- Kernel and Runtime Adapter policy remain unchanged; the Kernel receives only resolved bindings.

## Rejected alternatives

Binding the Plugin back to `lenso.agent.tool-provider@1` would expose a generic role and deadlock on
its single-concurrency nested invocation. Adding a second provided Capability to the existing Tool
Provider Module also caused descriptor registration to bleed into another multi-Capability Module,
so the Host role remains a separate Module Instance. Giving the component WASI filesystem mounts would create
ambient authority outside the Plan. Allowing the Manifest to name its provider would delegate Host
Composition policy to the publisher.
