# ADR-0072: Expose a searchable local Plugin catalog

Status: Accepted

## Context

The Harness can already admit a reviewed Plugin Bundle, configure or disable
Instances through the visible Plugin Root, reconcile the resulting immutable
Generation, and remove a root-supplied Plugin. The control surface nevertheless
requires an operator to know an exact package identity or absolute Bundle path
before it can discover what the current Host can use.

A remote package marketplace would add a new distribution and trust authority.
That authority is not implied by the existing content-addressed Store and must
not be introduced by labeling locally linked or admitted code as remotely
trusted.

## Decision

- The authorized Web control surface exposes a searchable local Plugin catalog
  at `GET /api/console/v1/agent/control/plugins/catalog?query=...`.
- Catalog entries are projected from the same Host Catalog and Plugin Root
  authoring state used by install, configuration, enablement, removal, and App
  resolution.
- Each entry reports exact package identity and revision, Host-build or
  Plugin-root source, active or available state, Instance selection, and only
  the actions admitted for that entry.
- Installation remains the existing explicit local-Bundle operation. The
  catalog declares that it requires an absolute path so a Console can render a
  file-selection flow without inventing a URL installer.
- Catalog reads and every mutation require the Host-selected control
  authorization seam. Search is bounded and performs no filesystem or network
  discovery.

## Consequences

- A Console can render browse, search, configure, enable/disable, install, and
  remove flows without maintaining a second Plugin inventory.
- Linked built-ins and root-supplied packages are visibly distinct.
- This is a local Marketplace UX contract, not a public remote registry. A
  future registry requires its own signed metadata, download, integrity,
  provenance, and revocation decision.

## Proof

The Web integration test proves unauthorized discovery fails, an authorized
bounded query returns only matching packages, and installation requirements and
source-aware actions are explicit. Existing Plugin control tests retain the
install, configuration publication, enablement, reconciliation, and removal
proof on the same authority.
