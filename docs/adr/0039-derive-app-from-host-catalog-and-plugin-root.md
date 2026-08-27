# ADR 0039: Derive the App from the Host Catalog and Plugin Root

## Status

Accepted.

Supersedes the App authoring and local-selection portions of ADRs 0001, 0003,
0004, 0007–0010, 0019–0023, 0025–0031, 0034, 0036, and 0038. Their Capability,
Ready Gate, immutable Generation, isolation, authorization, and provenance
decisions remain in force where they do not require a central App Definition,
Module contribution, or Active Set authoring surface.

## Context

The Harness accumulated two overlapping extension models. A user called a
package a Plugin, but implementing or configuring behavior required learning
Module packages, Module contributions, App Definitions, binding decisions,
local enabled lists, sidecars, Active Sets, and generated Plans. Moving the
enabled list between `lenso.app.json`, `lenso.local.toml`, and private state did
not remove that conceptual duplication.

The immutable runtime invariant is still valuable: Kernel must receive one
complete Plan, and a long-lived Host must prove a candidate ready before it
changes routing. Those internal requirements do not require users to author the
lowered graph.

## Decision

Plugin is the only public removable behavior unit. The public Module concept,
`#[lenso::module]`, Module Descriptor, Module contribution, and `*-module`
package naming are retired without compatibility aliases.

Each Host build exposes one immutable generated Host Catalog. It contains its
linked Plugin Descriptors, root Slots, default Plugin Instances, Host-owned
configuration, and private attachments for repeated Capability providers. The
catalog is availability and default-composition authority; linking a Plugin
does not independently mutate a running App.

An App owner expresses only differences under `plugins/`:

```text
plugins/<plugin-id>/plugin.lenso-plugin/
plugins/<plugin-id>/<instance>.toml
plugins/<plugin-id>/<instance>.disabled
```

The package entry is absent for Plugins already linked into the Host. A missing
or empty Plugin Root selects the exact Host defaults. There is no
`lenso.app.json`, `lenso.app.toml`, `lenso.local.toml`, central enabled list,
or user-authored binding document.

Resolution is a pure operation over an immutable Host Catalog and a strict
filesystem snapshot. It merges package defaults, Host configuration, and the
Instance TOML patch; validates the complete configuration; selects root Slots;
derives Capability bindings; and produces one exact Resolved App Plan. Kernel
receives only that Plan.

Host-private attachments may select a provider Instance or provider Slot for a
known consumer. They exist only to distinguish repeated providers such as the
root and restricted Tool runtimes. They are generated Host policy and never
appear in the Plugin Root.

Generation Controller, Supervisor, receipts, artifact storage, recovery, and
provenance remain derived Host internals. Long-lived reconciliation snapshots
the complete Plugin Root, stages the candidate behind the ordinary Ready Gate,
and switches new routes only after readiness. Existing Turns keep their exact
Generation lease.

## Consequences

- A user can understand and reproduce the App by inspecting `plugins/`.
- A Host boots usefully with no App file and an empty Plugin Root.
- A built-in behavior and an external behavior use the same Plugin Instance
  configuration shape.
- Replacing a one-Slot Plugin or adding to a many-Slot remains Host policy, not
  publisher-selected binding authority.
- The Plan remains deterministic execution evidence without becoming another
  authoring document.
- Legacy Definition, Module, sidecar, Active Set command, and `plugin verify`
  paths must be deleted rather than kept as hidden compatibility modes.

## Proof

Repository tests must show that an absent Plugin Root resolves and runs the Host
defaults; adding `plugins/lenso.agent.text-tools/default.toml` adds the linked
Tool Plugin and its derived binding; invalid configuration or disabling a
required default changes no current Generation; and a clean-room external
Plugin can be packed, added, configured, run, disabled, enabled, and removed
without authoring a Module or App Definition.
