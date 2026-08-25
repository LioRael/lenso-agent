# Direct ChatGPT subscription Module cards

Status: experimental direct-provider baseline.

## `lenso.agent.auth.openai-codex`

- **Deletion boundary:** removes ChatGPT browser/device OAuth, refresh,
  credential profile persistence, status, and logout. Model, Agent Loop, Tool
  Runtime, Session, Runner, and Kernel are unchanged.
- **Owned facts:** OAuth issuer and client identity, PKCE/state generation,
  loopback callback verification, device polling, refresh margin, account-ID
  extraction, provider-keyed profile storage, credential locking, atomic
  replacement, file permissions, and redaction.
- **Provides:** private non-portable
  `lenso.agent.auth.openai-codex@1` (`access`, request).
- **Requires:** none.
- **Configuration:** official issuer, profile, refresh margin, and optional
  explicit credential path. The default store is `~/.lenso/agent/auth.json`;
  no token is configuration.
- **Lifecycle/resources:** endpoint-only; each access holds the store lock
  while reading and, when needed, refreshing the credential.
- **Failure policy:** missing or refresh-rejected credentials are Domain Errors;
  storage, transport, and malformed token failures are sanitized Runtime
  Failures.

## `lenso.agent.model.openai-codex-direct`

- **Deletion boundary:** removes direct Codex Responses request conversion,
  subscription headers, SSE decoding, and provider-error translation.
- **Owned facts:** allowed backend URL, selected model, Responses wire mapping,
  event bound, and sanitized status policy.
- **Provides:** `lenso.agent.model@1` (`complete`, stream).
- **Requires:** exactly one `lenso.agent.auth.openai-codex@1` provider selected
  by Composition.
- **Configuration:** official backend base URL, model, reasoning effort, and
  maximum SSE event bytes. The shipped Composition selects `gpt-5.6-luna` with
  medium reasoning.
- **Lifecycle/resources:** activation constructs the generated Auth client only
  from `ModuleDependencies`; every completion owns one HTTP response stream.
- **Failure policy:** invalid request and unsupported model are Domain Errors;
  auth, network, rate limit, malformed SSE, truncation, and provider failure
  remain sanitized Runtime Failures.
- **First behavior:** asks the direct provider to call
  `read`, sends the Tool result in a second Responses request,
  and streams the final text and usage through the existing Agent Loop. The
  wire mapper replaces provider-invalid Tool-name characters and reverses that
  alias before dispatch; alias collisions fail closed.

## Removal proof

Removing the direct Model and Auth Instances, their private binding, packages,
and Auth contract leaves the App valid after selecting the fixture Model.
Kernel, Driver, Native Adapter, Agent Loop, Tool Runtime, and Session contain no
ChatGPT-specific branch.
