# ADR 0002: Own direct ChatGPT subscription access

- Status: experimental
- Date: 2026-08-24
- Relates to: ADR 0001

## Context

The Agent Harness needs a ChatGPT subscription Model provider with the same
essential architecture used by Pi and OpenCode: the host owns OAuth token
lifecycle and directly invokes the Codex Responses backend. Delegating to the
Codex CLI would instead delegate the complete Agent loop, tool policy, thread
state, and transport, preventing the Lenso Agent Loop from retaining those
product boundaries.

ChatGPT subscription OAuth and the Codex backend are distinct from the public
OpenAI API-key surface. Their endpoints and request details can evolve outside
this repository, so they must remain isolated behind removable Modules rather
than entering Kernel, Runner, or the portable Model Capability.

## Decision

Add the private request Capability `lenso.agent.auth.openai-codex@1`. The Auth
Module provides short-lived access material to exactly one explicitly bound
consumer. It owns browser PKCE OAuth with a state-checked loopback callback,
headless device OAuth, refresh, profile selection, file locking, private
credential persistence, and redaction. Tokens and account identifiers are
sensitive generated-contract fields and never enter the App Plan or Session
events.

Add `lenso.agent.model.openai-codex-direct`, which continues to provide the
portable `lenso.agent.model@1` stream Capability. It requires exactly one Auth
provider and directly maps Model requests, Tool calls/results, text deltas, and
usage to the Codex Responses backend. Production egress is restricted to
`https://chatgpt.com/backend-api`; loopback HTTP is allowed only for tests.

The CLI's `auth login`, `auth status`, and `auth logout` commands operate on
the app-local direct-auth profile. Browser PKCE is the default;
`auth login --device-auth` is the headless fallback. Profiles use a Pi-style
provider-keyed structure in `~/.lenso/agent/auth.json`. The Harness neither
invokes the Codex CLI nor imports its credential store.

## Consequences

- Lenso retains Agent Loop, Tool Runtime, Session, admission, and cancellation
  ownership while the provider remains replaceable through Composition.
- Auth and Model wire compatibility can evolve together without widening the
  portable Model contract.
- Refresh credentials are stored outside the repository in a private app
  directory with mode `0600` on Unix, guarded by a store lock and atomic
  replacement.
- Authentication rejection, refresh loss, network loss, malformed events, and
  provider failure are sanitized Runtime Failures; unsupported model and
  invalid request remain Model Domain Errors.
- The profile is experimental because it depends on a subscription-specific
  upstream surface. The API-key provider remains the stable documented option.

## Rejected alternatives

### Delegate to `codex exec`

This is operationally simple but makes Codex, not Lenso, own the Agent loop and
tools. It is not a Model provider seam and is therefore not retained.

### Reuse `lenso.secrets@1` for refresh credentials

Secrets resolution does not own interactive OAuth, token expiry, rotation,
profile locks, or logout. Those behaviors form a removable Auth Module.

### Put OAuth behavior in the Model Module

Combining credential lifecycle and provider wire mapping would make either
concern harder to test, replace, and remove independently.
