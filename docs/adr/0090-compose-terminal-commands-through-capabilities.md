# ADR 0090: Compose terminal commands through Capabilities

- Status: accepted
- Date: 2026-08-31
- Extends: ADR 0042 and ADR 0076
- Supersedes: the Agent-specific terminal extension contracts in ADR 0020 and
  ADR 0032

## Context

Lenso Agent had a composed TUI, but command ownership remained split between
hand-written CLI parsing, Shell-local slash commands, and an Agent-specific
suggestion provider. A feature Plugin could expose backend behavior without
exposing the same operation to the CLI and TUI. Reusing that mechanism in the
Lenso CLI would either copy parser code or turn `Contribution` into an
unbounded framework registry.

A terminal command needs a real contract because independently removable
Plugins must agree on discovery, argument semantics, output, errors,
cancellation, limits, and command-path ownership. That contract must not make
Clap, Ratatui, argv, stdout, raw terminal mode, or an event loop part of Kernel
or part of every feature Plugin.

## Decision

Define two source-first, portable Capabilities:

- `lenso.terminal.command-provider@1` is implemented by feature Plugins. Its
  `catalog` Operation advertises stable command IDs, nested paths, parameters,
  and supported output formats. Its `execute` Operation is a cancellable
  stream of typed text or JSON output.
- `lenso.terminal.command@1` is the validated aggregate consumed by terminal
  surfaces. `lenso.terminal.command` binds many providers, snapshots and
  validates them during activation, rejects duplicate IDs, duplicate paths,
  prefix ambiguity, and aggregate-limit violations, then routes execution to
  the owning provider.

The two contracts intentionally share a wire shape but have different role
IDs. A feature provider cannot impersonate the aggregate, and a surface does
not need to discover or iterate arbitrary Plugins.

`lenso.terminal.cli` and `lenso.terminal.tui` are ordinary consumer Plugins.
The CLI consumer requires one command aggregate. The TUI consumer requires one
command aggregate plus many `lenso.tui.panel@1` and
`lenso.tui.suggestion@1` providers. These named contracts replace the vague
Agent-specific `TUI Contribution` name; there is no framework-wide
`Contribution` trait.

The process-owned CLI adapter uses `lenso-terminal-cli-surface` to construct a
Clap tree from the validated catalog and to translate argv into the provider's
JSON argument envelope. The process-owned TUI adapter projects the same catalog
into composer suggestions, parses accepted command lines through the same
adapter, and renders the output stream in its transcript. Raw terminal mode,
keyboard handling, stdout/stderr, process exit codes, and event-loop policy
remain Host surface responsibilities.

Catalog discovery and command execution use one `TerminalGeneration` lease.
An online Plugin transition may affect only a later lease; it cannot route a
command discovered in one immutable Generation into another. Catalog requests
are bounded by a startup timeout. Execution has no arbitrary wall-clock
deadline and ends through provider completion, caller cancellation, or
Generation shutdown.

An App may contain multiple CLI or TUI consumer Instances. Each process or
embedded surface selects an explicit `plugin-id/instance-name` and leases it;
the terminal contracts do not introduce a singleton CLI. Different binaries
can use the same parser crate and Capability contracts without sharing process
I/O or product-specific maintenance commands.

The first feature slice is `lenso.agent.session-terminal`. It contributes
`sessions list` and `sessions show`, depends only on the existing Session
Capability, and is consumed unchanged by both the headless CLI and TUI.

The generic terminal packages incubate in the Agent Harness until another
repository consumes a released version. A future Lenso CLI integration should
depend on published terminal crates or move them through an explicit repository
extraction; sibling-repository path dependencies and an umbrella `lenso` crate
are not part of this decision.

## Consequences

- feature authors register commands by implementing one concrete Capability,
  not by mutating a global registry or depending on Clap;
- CLI, TUI, and future Console surfaces can discover the same selected command
  set while presenting it differently;
- removing a feature provider removes only its commands, while removing a
  surface removes only that presentation and process I/O;
- invalid command combinations fail the App readiness gate instead of becoming
  order-dependent at invocation time;
- command providers retain final authorization for their domain operations;
  discovery grants no new authority; and
- Kernel remains unchanged and executes only the already resolved Plan and its
  existing request, stream, lifecycle, and cancellation contracts.

## Proof

Contract snapshot checks lock all four terminal/TUI contracts. Validator tests
cover IDs, paths, parameters, output formats, duplicate ownership, and prefix
ambiguity. Aggregate tests lock provider/consumer schema parity. CLI parser
tests cover nested commands, namespace ownership, quoting, JSON selection, and
group help. Distribution tests prove that CLI and TUI link only their own
surface consumer. End-to-end CLI tests discover and execute the Session
commands from a fresh Agent Home. TUI tests cover catalog-to-suggestion
projection and command-state rendering; the state machine retains the catalog
lease through stream completion or cancellation and refreshes it only after a
Generation transition.
