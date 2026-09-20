# ADR 0114: Separate the blank Agent foundation from the legacy Host

Status: Accepted

## Context

The current Agent Host Catalog is a compatibility product composition. Its
generation code supplies a default Agent Loop, concrete model and auth Plugins,
memory, session storage, instructions, tools, coding-oriented configuration,
and named official surface bindings. Linking those Plugins and merely marking
some instances disabled would still make their implementation and policy part of
the default foundation.

The Agent foundation implementation requires a blank entry that can inspect and
validate composition without implicitly selecting a model, Loop, coding Tool,
memory implementation, system instruction, or channel. Existing users must
retain the current coding-oriented distribution and their existing Agent Home
data while that new entry is introduced.

## Decision

The current Agent Host and coding-oriented executables remain an explicit
compatibility composition. They are not renamed into the blank foundation and
are not silently changed to remove their existing defaults.

The new foundation entry will own only the composition and diagnostic boundary
needed to build an Agent from explicitly selected Plugins and Capabilities. It
will start from an empty Host Catalog: no product Plugin is linked, selected,
configured, or run unless the caller explicitly chooses a composition that
contains it. An empty foundation can inspect and validate its configuration.
Attempting to start an interactive Agent before an Agent provider and its
required dependencies are selected must fail with a missing-dependency
diagnostic before any external model call.

Official conversational and coding experiences become separately named
compositions. They can reuse existing Plugin packages and Profile semantics,
but their dependency closure, configuration, model selection, and instructions
are visible selection inputs rather than foundation defaults. Compatibility
migration preserves existing Homes and executable behavior until an explicit
user-selected migration occurs.

Host-facing data required by alternate Loops and Providers belongs to the
appropriate public Capability owner, not the foundation entry or the default
Loop. Runtime generation, leases, readiness, and Adapter mechanics remain with
their current Host and Runtime owners.

## Consequences

The blank foundation cannot reuse the current legacy catalog constructor, since
that constructor encodes product defaults and official surface bindings. The
implementation must introduce a separate generic catalog/composition path and
test its dependency closure independently.

The initial foundation does not invent a second Plugin identity, central App
definition, service locator, or runtime graph mutation mechanism. App choices
remain visible Plugin Root and composition inputs, and ordinary Plan resolution
continues to derive immutable runtime state.

The exact package and command names are implementation details. This ADR fixes
the ownership and compatibility boundary, not a temporary scaffold API.
