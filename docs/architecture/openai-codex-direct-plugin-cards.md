# Direct ChatGPT subscription Plugin cards

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

- **Deletion boundary:** removes direct Codex model discovery, Responses request
  conversion, subscription headers, SSE decoding, and provider-error
  translation.
- **Owned facts:** allowed backend URL, discovered model metadata, Provider and
  App visibility, selected model, Responses wire mapping, event bound, and
  sanitized status policy.
- **Provides:** `lenso.agent.model@4.0` (`catalog`, request; `complete`, stream).
- **Requires:** exactly one `lenso.agent.auth.openai-codex@1` provider selected
  by the Host Profile.
- **Configuration:** official backend base URL, selected model, optional exact
  `include_models`/`exclude_models` visibility policy, reasoning effort, and
  maximum SSE event bytes. All valid Provider-discovered models remain in the
  frozen catalog. Legacy `allowed_models` is accepted only as a deprecated
  no-op migration input. The shipped Profile selects `gpt-5.6-luna` with medium
  reasoning.
- **Lifecycle/resources:** activation constructs the generated Auth client only
  from `PluginDependencies`, fetches and validates the authenticated model
  catalog, then freezes it for the candidate Generation. Every completion owns
  one HTTP response stream.
- **Failure policy:** catalog authentication, network, status, or validation
  failure rejects the candidate Ready Gate. Invalid completion requests and
  unsupported models are Domain Errors; rate limit, malformed SSE, truncation,
  and provider failure remain sanitized Runtime Failures.
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
