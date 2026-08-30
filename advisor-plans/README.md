# Deep improvement implementation plans

Generated from the 2026-08-30 deep audit and executed on the
`codex/deep-improvements-20260830` delivery branch.

## Execution order and status

| Plan | Title | Priority | Depends on | Status |
|---|---|---:|---|---|
| F02 | Agent Web data-plane authorization | P0 | none | DONE |
| F04 | OpenAI-compatible stream bounds | P0 | none | DONE |
| C01 | Complete Web Session reads | P1 | none | DONE |
| P04 | Non-blocking SQLite Session runtime | P1 | none | DONE |
| P09 | Bounded Web actor backlog | P1 | none | DONE |
| P05 | Incremental TUI transcript layout | P2 | none | DONE |
| P11 | Host build identity startup cost | P2 | none | DONE |
| P12 | SQLite Session-list query plan | P2 | P04 | DONE |

## Dependency notes

- P12 was validated after P04 because the SQLite query evidence must be
  measured on the final Generation-owned worker implementation.
- The remaining plans affect independent ownership boundaries and were safe to
  implement and validate independently.

## Review state

Every plan file records its implementation-specific tests and measurements.
The affected packages pass formatting, check, all-target Clippy with warnings
denied, and their complete test suites. No finding in this repository remains
deferred or blocked.

The shared validation commands used the workspace wrapper throughout:

- `/Users/leosouthey/Projects/framework/.lenso-tools/bin/lenso-cargo fmt --all -- --check`
- `/Users/leosouthey/Projects/framework/.lenso-tools/bin/lenso-cargo check -p lenso-agent-web -p lenso-agent-model-openai-compatible-plugin -p lenso-agent-session-sqlite-plugin -p lenso-agent-tui -p lenso-agent-host`
- `/Users/leosouthey/Projects/framework/.lenso-tools/bin/lenso-cargo clippy -p lenso-agent-web -p lenso-agent-model-openai-compatible-plugin -p lenso-agent-session-sqlite-plugin -p lenso-agent-tui -p lenso-agent-host --all-targets -- -D warnings`
- `/Users/leosouthey/Projects/framework/.lenso-tools/bin/lenso-cargo test -p lenso-agent-web -p lenso-agent-model-openai-compatible-plugin -p lenso-agent-session-sqlite-plugin -p lenso-agent-tui -p lenso-agent-host`
