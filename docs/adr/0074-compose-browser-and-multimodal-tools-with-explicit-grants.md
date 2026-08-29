# ADR-0074: Compose browser and multimodal Tools with explicit grants

Status: Accepted

## Context

Mainstream Harnesses expose browser interaction, image reading, and audio
transcription. Treating a URL, local media path, CDP endpoint, API credential,
or arbitrary browser script as an ordinary string would bypass the Harness's
composition and approval boundaries. The existing text Tool result also cannot
truthfully make a model see image bytes merely by labeling base64 as text.

## Decision

Two optional Tool Provider Plugins deliver the first bounded slice.

`lenso.agent.browser.playwright`:

- binds to one selected Process Provider that explicitly authorizes `node`;
- attaches only to one loopback CDP endpoint and never launches an ambient
  browser itself;
- accepts a sorted exact origin allowlist and installs request interception
  before navigation or interaction;
- exposes fixed navigate, visible-text snapshot, CSS click/fill, and screenshot
  actions through a bundled static Playwright driver; and
- derives screenshot paths from a configured Workspace-relative directory and
  a restricted filename, with no arbitrary JavaScript or output path.

`lenso.agent.multimodal-tools`:

- binds to one selected Secrets Provider and resolves one configured credential
  reference per call;
- accepts only root-bounded regular PNG, JPEG, WebP, WAV, or MP3 files within a
  configured byte limit;
- sends typed image or input-audio content blocks to one HTTPS or loopback
  OpenAI-compatible endpoint with redirects disabled; and
- returns the provider's bounded textual image understanding or transcription
  as the ordinary Tool result. It does not pretend that the Agent Model received
  raw media.

Both Plugins are absent from the default App. Their Tool availability, roots,
origins, endpoint, executable, credential owner, and models become immutable
Generation authority through the Plugin Root and Host bindings.

## Consequences

- Browser state is owned by the explicitly attached CDP browser; the Plugin
  remains removable and the Kernel remains unaware of browser concepts.
- Browser screenshots can be composed with `read_image` without widening
  either Plugin's authority.
- Media bytes are not stored in Sessions or returned as misleading text; the
  durable Tool result contains only the bounded derived text and non-secret
  provenance.
- A future native content-block Tool/Model contract can carry raw media end to
  end, but is not claimed by this slice.

## Proof

Browser tests reject off-allowlist origins and path traversal in screenshots.
Multimodal tests prove root-relative path policy and the typed provider request
shapes for image and audio. Host resolution tests prove both Plugins are opt-in,
bind to the selected Process or Secrets Provider, and enter the ordinary Tool
Provider aggregation path.
