# ADR 0022: Compose optional built-ins through Plugin state

## Status

Accepted.

## Context

The Harness had reviewed App Definitions for model and Tool combinations such
as `headless-readonly`, `headless-coding`, and
`openai-codex-direct-coding`. Requiring another tracked App Definition for
every optional Tool Provider combination produces a combinatorial authoring
surface even though the Host already persists exact enabled Plugin Releases in
one Active Set and resolves them into an immutable App Generation.

The existing `plugins install --bundle` path proves third-party Bundle
admission, but it is unnecessarily low-level for exact Releases already
shipped with the product. Users should select those Releases by a stable name
without locating a Bundle directory or editing Composition JSON.

## Decision

The Agent CLI exposes product-owned `plugins available`, `plugins enable`, and
`plugins disable` commands for exact bundled Releases. `enable` reuses the
existing Bundle parsing, Profile Catalog matching, admission Receipt, and
Plugin Set lock path. `disable` removes the selected Release and its Instances
from the same exact authority. Both commands then resolve the candidate against
the selected base App, require the Runtime maintenance Ready Gate, retain the
old and candidate authorities by digest, and only then atomically commit.
Failed readiness leaves the current Active Set unchanged. The enabled
selection remains in `.lenso/plugins/active-set.json`.

The first newly composable built-in is `workspace-edit`:

- it contributes the linked `lenso.agent.workspace-edit@0.1.0` Module factory;
- it provides exactly `lenso.agent.tool-provider@1` and appends to the existing
  `tools` Instance's `many` requirement;
- its canonical configuration is workspace-rooted and bounded;
- it is experimental and requires explicit review evidence because it adds
  mutation authority; and
- it can coexist with independently enabled Tool Plugins such as `text-tools`.

The Kernel still receives only one immutable Resolved App Plan. Enabling or
disabling a Plugin does not register a Module in a running graph. The Host
resolves the persisted Plugin Set into an App Generation and uses the existing
Ready, switch, drain, provenance, and rollback authorities.

## Consequences

- A user can add workspace mutation to the default read-only App without
  creating a `*-coding.app.json` file.
- Multiple supported Plugins compose through one persisted selection rather
  than through named files for every combination.
- The redundant `*-coding.app.json` definitions are removed. ADR-0023 extends
  the same mechanism to all remaining optional built-ins and removes the
  product Composition directory.
- Resolved Plans are generated in ignored Host state instead of being committed
  beside source App Definitions.
- `available` is a product catalog, not runtime discovery. Arbitrary linked
  Modules, publisher-selected bindings, permission grants, and configuration
  remain unsupported until the Host exposes reviewed selection policy for
  them.

## Rejected alternatives

Generating another App Definition for every enabled set preserves the
combinatorial UX. Letting linked factories self-register would bypass product
admission and create a second dependency authority beside the immutable Plan.
