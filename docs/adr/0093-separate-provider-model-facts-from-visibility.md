# ADR-0093: Separate Provider model facts from visibility

Status: Accepted

## Context

ADR-0092 made the selected Model Provider responsible for discovering and
freezing model facts at Generation readiness. The direct Codex Provider still
used `allowed_models` to remove discovered entries before publishing its
catalog. That made a local presentation choice indistinguishable from the
Provider's source of truth: catalog consumers could not tell whether a model
was unavailable, undiscovered, or merely omitted by configuration.

Model visibility is not an authorization boundary. A secondary Plugin or an
explicit per-Turn selection may legitimately use a discovered model that an
interactive picker does not need to promote. Conversely, a visibility filter
must not erase the limits and controls required to validate such a request.

## Decision

The selected direct Codex Provider publishes every valid discovered model
except entries whose Provider visibility is `none`. Provider visibility and
App-owned visibility are projected through the existing `hidden` model fact;
they do not remove an otherwise discovered model from the Generation catalog.

The direct Provider accepts two optional, exact-ID visibility controls:

- `include_models` makes the configured primary model plus the listed models
  visible; and
- `exclude_models` hides listed models.

The configured primary model is always visible. Include and exclude sets are
bounded, unique, disjoint, and cannot contain the primary model. Provider
`hide` remains authoritative even when an App includes that ID.

The former `allowed_models` field remains accepted for one migration window but
is deprecated and has no filtering effect. This prevents an older local
configuration from continuing to truncate a newly discovered Provider catalog.
App owners should remove it or replace it with the explicit visibility fields.

The frozen Generation catalog remains the only admission and validation
authority. CLI, TUI, Web, ACP, and Console consumers may omit `hidden` entries
from ordinary selectors, but explicit model IDs and model-selection Plugins
continue to resolve against the complete selected-Provider catalog.

## Consequences

- Provider discovery, account availability, and App visibility are no longer
  represented by one allowlist;
- `lenso-agent-cli models` can explain the complete Generation catalog through
  existing model entries and their `hidden` values;
- visibility changes still reconcile through a candidate Generation and never
  mutate the catalog leased by an existing Turn;
- frontends do not reconstruct visibility from model names; and
- removing the direct Provider Plugin removes discovery and its visibility
  policy without adding a Host or Kernel branch.

## Proof

Provider tests prove that legacy `allowed_models` does not remove new Provider
models, include/exclude policy changes only `hidden`, Provider `none` remains
absent, Provider `hide` remains hidden, and invalid or overlapping policies
fail configuration validation. Host tests cover configured `include_models`
projection for unselected Providers. The direct CLI integration continues to
acquire the catalog before Responses traffic, and repository checks lock the
Plugin configuration Schema and linked descriptor.
