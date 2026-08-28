# OpenAI-compatible Model Plugin card

Status: implementation baseline for the first real provider slice.

## `lenso.agent.model.openai-compatible`

- **Deletion boundary:** removes OpenAI-compatible Chat Completions transport,
  request conversion, SSE decoding, and provider-error translation. The Agent
  Loop, fixture Model, Tool Runtime, Session, Runner, and Kernel are unchanged.
- **Owned facts:** selected provider base URL, one allowed model, logical API
  key reference, Chat Completions wire mapping, stream assembly, and sanitized
  provider failure policy.
- **Provides:** `lenso.agent.model@2` (`complete`, stream).
- **Requires:** exactly one `lenso.secrets@1` provider selected by Composition.
- **Configuration:** HTTPS base URL (or loopback HTTP for tests), model name,
  and logical secret reference. No resolved credential is configuration.
- **Lifecycle/resources:** activation constructs the generated Secrets client
  only from `PluginDependencies`; deactivation drops it. Each completion owns
  its HTTP response stream and is cancelled when the Kernel drops/cancels the
  stream future.
- **Failure policy:** invalid model/request is a Domain Error; missing Secrets,
  network loss, authentication rejection, rate limiting, malformed provider
  events, and provider failure remain sanitized Runtime Failures. Provider
  bodies and credentials never enter errors or diagnostics.
- **First behavior:** the Agent asks an OpenAI-compatible provider to call
  `read`, returns the Tool result in a second completion, and
  emits streamed text and usage.

## Secrets selection

The distributed Host links environment, macOS Keychain, age-encrypted-file,
and bounded-command Providers from the Secrets Plugins repository. A Session
Profile selects exactly one configured Provider Instance from `plugins/`; the
Profile itself contains no mapping or credential. The OpenAI-compatible Model
continues to request only `model/openai-api-key`, so switching Provider or using
different mappings per Profile does not change the Model Plugin.

The Harness imports the owner-local `lenso-capability-secrets` crate and does
not vendor a second contract copy. Resolved values never enter Plugin
configuration, Profiles, Plans, errors, or Session facts.

## Removal proof

Removing the OpenAI-compatible Model Instance and its selected Secrets
Provider Instance leaves the default direct-Codex App valid. Kernel has no
provider-specific branch or runtime plugin registry.
