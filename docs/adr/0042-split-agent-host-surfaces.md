# ADR 0042: Split Agent Host surfaces

## Status

Accepted.

## Context

The original `lenso-agent-cli` Cargo package built the headless runner, TUI,
Telegram, Discord, and unified Channel Host together. Its shared Generation
implementation also imported every Native Plugin factory. A Profile could omit
an Instance at runtime, but a headless-only user still compiled every surface
and the executable still advertised their Plugins in its Host Catalog.

Terminal I/O and messaging transports are process-owning adapters. Their
consumer identities and replaceable contributions are Plugins, but forcing the
event loop itself through the request-only Plugin protocol would hide rather
than remove Host responsibility.

## Decision

The Harness publishes three independent distributions:

- `lenso-agent-cli` owns only the headless command-line surface;
- `lenso-agent-tui` produces `lenso-agent` and owns terminal I/O; and
- `lenso-agent-channel` produces the unified, Telegram, and Discord ingress
  executables.

`lenso-agent-host` owns Plugin Root resolution, Profiles, immutable
Generations, reconciliation, and provenance without importing concrete surface
Plugins. `lenso-agent-default-plugins` links the standard surface-neutral Agent
behavior. Each executable adds only its own consumer and contribution Plugins.

The Host Catalog is derived from factories actually linked into that
executable. Defaults, configurations, and bindings for absent Plugins are
omitted. Runtime Profiles therefore select among real Host capabilities; they
do not simulate install-time modularity by disabling compiled code.

TUI panels, suggestions, commands, Telegram identity, and Discord identity
remain Plugins. Ratatui, terminal raw mode, Bot polling, Gateway connections,
and delivery state remain in their optional process-owning surface packages.

## Consequences

- installing the headless CLI does not compile or install TUI or Channel code;
- a custom Host can link a different Plugin set without editing Generation
  mechanics;
- each surface keeps an independent durable Controller lineage;
- Surface Catalog tests prove exact Plugin presence and absence; and
- adding a new ingress requires a surface package plus its consumer Plugin,
  rather than another binary inside `lenso-agent-cli`.
