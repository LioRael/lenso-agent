# ADR 0023: Keep one base App and select optional Modules through Plugin state

## Status

Accepted.

## Context

After ADR-0022, users could enable some optional Tool Providers without another
App Definition, but Skills, process execution, and Model choices still required
named variants. That kept two competing authoring experiences and retained a
combinatorial `composition/` directory.

The Host already owns the exact enabled Plugin Releases, reviewed attachment
profiles, readiness transition, and immutable App Generation. The remaining
variants differ only by optional Module selection and configuration that the
Host can represent with the same authority.

## Decision

The repository keeps one source App Definition at `lenso.app.json`. It contains
the deterministic, read-only base graph plus both removable CLI and TUI surface
anchors. The two binaries keep separate Generation Controller namespaces while
resolving the same graph and Plugin Active Set. The product `composition/`
directory and named `--app` selection are removed.

Every optional built-in is selected through the persisted Plugin Active Set:

- `text-tools` and `workspace-edit` append Tool Providers;
- `skills` attaches one Module to both the Prompt and Tool `many` requirements;
- `local-process` adds the process Provider and its Tool projection as one
  atomic selection;
- `openai-compatible` replaces the fixture Model and adds its Env Secrets
  provider; and
- `codex-direct` replaces the fixture Model and adds its Auth provider.

The Host Profile Catalog owns exact configurations, attachment policy, and
admission risk. Enabling or disabling a selection resolves a candidate Plan,
requires the existing Ready Gate, and only then atomically commits Desired
State. Conflicting Model replacements fail closed.

The CLI resolves the immutable Plan in memory by default and does not write a
derived Plan into the project. Exact Plan replay through `--plan` remains
available for automation and diagnosis.
Provider-specific App Definitions needed by integration tests live under test
fixtures and are not product variants.

## Consequences

- Users compose supported capabilities with `plugins enable` and `plugins
  disable`; adding a combination never adds a JSON file.
- There is one human-owned App source, one persisted Plugin selection, and one
  generated Plan location.
- Multi-Capability attachment is explicit in the Host catalog rather than
  inferred by the Kernel or publisher Manifest.
- Native process and installed Skill code remain trusted, reviewed selections;
  this change does not make them sandboxes.
- The Kernel continues to receive one immutable Resolved App Plan and has no
  Plugin registry, package discovery, or live graph mutation.

## Rejected alternatives

Keeping a hidden generated App Definition for every selected set only moves the
combinatorial artifact. Letting Modules self-register or letting publisher
Manifests choose arbitrary consumers would create an authority beside the Host
catalog and immutable Plan.
