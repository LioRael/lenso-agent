# ADR 0020: Enter a composed TUI from the product entrypoint

- Status: accepted
- Date: 2026-08-26
- Extends: ADR 0001, ADR 0003, ADR 0005, and Lenso ADRs 0043, 0045, 0057

## Context

The first Harness executable is headless-first and multiplexes one-shot Agent
Turns, authentication, Plugin control, Generation inspection, and Session
inspection through subcommands and hand-written argument parsing. The desired
product interaction is simpler: running `lenso-agent` should immediately open
the interactive Agent interface.

The TUI must remain removable and composable without adding a widget registry,
terminal concepts, or graph mutation to Kernel. Module authors also need a
stable extension seam that does not force every package to depend on Rust,
`ratatui`, or the Shell's event-loop implementation.

## Decision

`lenso-agent` is a dedicated clap entrypoint with global options and no
subcommands. With no arguments it loads the reviewed `tui-readonly` Resolved
App Plan and enters the TUI. Exact Plan replay, Session resume, and Turn-local
Tool narrowing remain global options.

The existing `lenso-agent-cli` binary remains the companion headless and Host
maintenance surface during migration. App startup, auth recovery, Plugin
authority, and Generation inspection therefore remain usable when the TUI App
cannot become ready without adding maintenance subcommands to `lenso-agent`.
Because these entrypoints select distinct App Compositions, the TUI owns a
separate durable Controller namespace while sharing the Plugin Store, retained
exact Plugin authority, and immutable Generation records with the companion
CLI. Neither surface attempts to recover the other's Controller lineage.

The interactive surface is an ordinary consumer-only `lenso.agent.tui` Module.
It requires exactly one `lenso.agent@1` provider and `many`
`lenso.agent.tui-contribution@1` providers. The current authoring facade cannot
derive a consumer-only Module without a fake provided Capability, so the TUI
Shell uses the same explicit compatibility factory shape as the CLI Module.

TUI Contribution v1 exposes bounded read-only semantic panel snapshots. The
Shell invokes all providers in resolved order, rejects duplicate panel IDs,
and renders their title/body content. `ratatui` and `crossterm` remain Shell
implementation details. Contribution providers cannot inject Widget trait
objects, closures, terminal escape sequences, event handlers, Capability
lookups, or layout mutation.

Each submitted Turn receives its own active App Generation lease. Dropping an
active stream on Esc cancels that stream; the lease is released only with the
stream. The TUI restores raw mode, the alternate screen, and cursor visibility
on normal return and error unwinding.

## Consequences

- `lenso-agent` opens a useful interface without requiring command discovery.
- Headless automation and recovery operations remain available through the
  companion binary while their future product surface is considered
  independently.
- App Composition, not runtime registration, selects TUI Contributions.
- A package may ship backend and TUI entrypoints while keeping their Module
  Instances and dependencies independently removable.
- Removing every Contribution leaves the TUI conversation operational.
- Interactive Contribution actions, forms, and writable view models require a
  later compatible contract slice with explicit authorization and error
  semantics; v1 does not smuggle them through string callbacks.
- The first contract is native-only and does not claim Bun/Wasm portability.

## Rejected alternatives

### Put a `tui` subcommand under `lenso-agent`

This preserves an implementation-oriented mode selector even though the
interactive TUI is the product's default job.

### Let Modules register `ratatui::Widget` values

This couples every provider to one Rust UI library and version, exposes Shell
lifecycle and layout internals, and prevents a portable Capability projection.

### Add a global UI registry

This creates a second mutable dependency graph beside the immutable Resolved
App Plan. Explicit `many` bindings already express selection and order.
