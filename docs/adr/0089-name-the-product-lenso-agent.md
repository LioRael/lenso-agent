# ADR-0089: Name the product Lenso Agent

Status: Accepted

## Context

The repository began as an Agent Harness architecture experiment, but its
public product identity has converged on `lenso-agent`. Users run
`lenso-agent`, `lenso-agent-cli`, `lenso-agent-acp`, and other surfaces; keep
state under `~/.lenso/agent`; and configure Plugins with stable
`lenso.agent.*` identities. The repository now owns the complete local coding
Agent product, including its Host, surfaces, Profiles, Sessions, Memory, Tools,
Plugin authoring path, and release packaging.

Keeping `lenso-agent-harness` as the repository and product name makes the
implementation category more prominent than the product users install. It
also leaves the repository name inconsistent with every stable user-facing
identity before the first versioned binary and ACP Registry release.

## Decision

- The product name is **Lenso Agent** and its repository is
  `LioRael/lenso-agent`.
- `agent harness` remains a generic architecture description, not the product
  proper name.
- Existing Cargo package and binary names remain `lenso-agent-*`.
- `LENSO_AGENT_HOME`, `~/.lenso/agent`, Plugin IDs, Capability IDs, schemas,
  Session facts, and artifact identities do not change.
- Active source, documentation, package metadata, release templates, and
  cross-repository dependencies use the new repository identity.
- Accepted ADRs preserve their historical wording. They are not rewritten to
  make earlier decisions appear to have used the new name.

## Consequences

- Product discovery, installation, executable names, and repository links use
  one identity.
- Hosts may still describe their implementation as an agent harness without
  creating a second product brand.
- Git consumers must update their source URL and regenerate lockfiles instead
  of relying indefinitely on repository redirects.
- This rename does not authorize Cargo package, Plugin ID, or protocol identity
  changes.

## Proof

Repository checks reject active references to the former repository identity.
Affected sibling repositories resolve their exact Git dependencies from
`LioRael/lenso-agent`. ACP packaging renders the new release URL, and the
bounded GitHub Tool allowlist accepts the renamed repository.
