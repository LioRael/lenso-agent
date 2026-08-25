# OpenAI-compatible Model Module card

Status: implementation baseline for the first real provider slice.

## `lenso.agent.model.openai-compatible`

- **Deletion boundary:** removes OpenAI-compatible Chat Completions transport,
  request conversion, SSE decoding, and provider-error translation. The Agent
  Loop, fixture Model, Tool Runtime, Session, Runner, and Kernel are unchanged.
- **Owned facts:** selected provider base URL, one allowed model, logical API
  key reference, Chat Completions wire mapping, stream assembly, and sanitized
  provider failure policy.
- **Provides:** `lenso.agent.model@1` (`complete`, stream).
- **Requires:** exactly one `lenso.secrets@1` provider selected by Composition.
- **Configuration:** HTTPS base URL (or loopback HTTP for tests), model name,
  and logical secret reference. No resolved credential is configuration.
- **Lifecycle/resources:** activation constructs the generated Secrets client
  only from `ModuleDependencies`; deactivation drops it. Each completion owns
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

The development profile selects external package `lenso.secrets.env` pinned to
repository commit `8fcef31d2f27b3b1bb8785855613b14e273a3e96`. App Composition
maps `model/openai-api-key` to `OPENAI_API_KEY`; the resolved value never enters
the project document or Plan. The Composition fragment selects the external
`lenso-capability-secrets` Cargo contract package; `lenso compose` reads its
owner-local Descriptor and generated Rust binding through Cargo metadata during
authoring validation. The Harness does not vendor a second contract copy.

## Removal proof

Removing the OpenAI-compatible Model Instance, Env Secrets Instance, their
binding, package selections, and Cargo contract selection leaves the existing
`headless-readonly` fixture Composition valid and executable. Kernel has no
provider-specific branch or runtime plugin registry.
