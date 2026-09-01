# Pi and OpenCode model-catalog design research (2026-09-01)

## Conclusion

Pi and OpenCode both reject a frontend-maintained model list, but they solve a
different problem from Lenso's selected-Provider readiness snapshot.

- **Pi** has the cleaner Provider contract. A Provider owns `getModels()`, an
  optional `refreshModels()`, credential-aware `filterModels()`, and request
  execution. It combines a generated built-in baseline with Provider-owned
  dynamic overlays, restores a persistent cache before attempting the network,
  and protects publications with per-Provider refresh generations.
- **OpenCode** has the richer catalog product. `models.opencode.ai` provides a
  broad normalized directory, including limits, modalities, status, cost, and
  structured `reasoning_options`. OpenCode derives selectable variants from
  those options, then layers plugins and user configuration over the directory.
  Its cache is intentionally stale-tolerant and is refreshed independently of a
  conversation.
- **Lenso should keep its stronger lifecycle boundary.** Neither upstream
  design gives one Turn an immutable Generation-owned catalog lease. Lenso's
  selected Provider should still acquire, validate, and freeze its effective
  catalog at the Ready Gate. The useful ideas to adopt are Pi's Provider
  refresh/publish contract and OpenCode's richer neutral reasoning schema,
  filtering vocabulary, provenance, and explicit freshness controls.

The main product correction is therefore not “show every model returned by a
global directory.” It is: **let the selected Provider own discovery, preserve
the last validated snapshot as an input to reconciliation, and give users
separate Provider/model visibility filters that do not masquerade as the source
of truth.**

## Scope and source baseline

This report uses only first-party source and documentation.

- Pi: [`badlogic/pi-mono` commit `853a80d26c90a14c1886f0ebb8ffaae133ca2185`](https://github.com/badlogic/pi-mono/tree/853a80d26c90a14c1886f0ebb8ffaae133ca2185).
- OpenCode: [`anomalyco/opencode` commit `1ead9e3d7f02661176fd46d7bcac7f6b7be3b52d`](https://github.com/anomalyco/opencode/tree/1ead9e3d7f02661176fd46d7bcac7f6b7be3b52d).
- models.dev: [`anomalyco/models.dev` commit `8a3ca0a93262a7ee8a85b91a2cbb6f97f20f7787`](https://github.com/anomalyco/models.dev/tree/8a3ca0a93262a7ee8a85b91a2cbb6f97f20f7787).
- Lenso baseline: [ADR-0092](../adr/0092-discover-selected-provider-models-at-generation-readiness.md).

“Observed” below means directly supported by those pinned sources. “Inference”
means an architectural interpretation, not an upstream guarantee.

## Pi

### Observed facts

#### Source of truth and Provider abstraction

Pi's runtime unit is a `Provider`. It owns identity, authentication, a
synchronous last-known catalog, optional dynamic refresh, optional
credential-specific filtering, and streaming. The contract explicitly says a
dynamic Provider returns its last refreshed list and should retain its previous
list on refresh failure. `Models.refresh()` can target Provider IDs, skips
static, unknown, and unconfigured Providers, refreshes selected Providers
concurrently, and returns per-Provider errors rather than rejecting the whole
operation. See [`models.ts`](https://github.com/badlogic/pi-mono/blob/853a80d26c90a14c1886f0ebb8ffaae133ca2185/packages/ai/src/models.ts#L90-L177) and its [refresh implementation](https://github.com/badlogic/pi-mono/blob/853a80d26c90a14c1886f0ebb8ffaae133ca2185/packages/ai/src/models.ts#L367-L440).

Built-in Provider catalogs are generated into the package and exposed through
Provider factories. Pi then wraps eligible built-ins with a remote `pi.dev`
overlay. Overlay entries replace the same model ID or append a new ID; a remote
entry older than the local generated timestamp is ignored. See
[`providers/all.ts`](https://github.com/badlogic/pi-mono/blob/853a80d26c90a14c1886f0ebb8ffaae133ca2185/packages/ai/src/providers/all.ts) and
[`remote-catalog-provider.ts`](https://github.com/badlogic/pi-mono/blob/853a80d26c90a14c1886f0ebb8ffaae133ca2185/packages/coding-agent/src/core/remote-catalog-provider.ts#L10-L55).

This is not universal live Provider discovery. The shared remote source is
`https://pi.dev/api/models/providers/{providerId}`; individual Providers can
also implement their own `refreshModels()`, as the Radius and llama.cpp
Providers do. The model execution API is separately selected by each model's
`api` field.

#### Cache, refresh, and failure behavior

Pi persists dynamic entries in `~/.pi/agent/models-store.json`. A refresh first
restores that Provider's stored entry without network access, then resolves the
credential, then optionally runs the network phase. A refresh generation and
serialized publication chain prevent a superseded or aborted refresh from
publishing stale state. See [`models-store.ts`](https://github.com/badlogic/pi-mono/blob/853a80d26c90a14c1886f0ebb8ffaae133ca2185/packages/coding-agent/src/core/models-store.ts) and
[`models.ts`](https://github.com/badlogic/pi-mono/blob/853a80d26c90a14c1886f0ebb8ffaae133ca2185/packages/ai/src/models.ts#L330-L440).

For the `pi.dev` overlay, the freshness window is four hours, each HTTP attempt
has a four-second timeout, and an ETag is sent only when a cached body exists.
`304` advances freshness without replacing the body; `404`/`501` records that
no overlay is available; other non-success responses retain the cached body and
validator while returning an error for this refresh. See
[`remote-catalog-provider.ts`](https://github.com/badlogic/pi-mono/blob/853a80d26c90a14c1886f0ebb8ffaae133ca2185/packages/coding-agent/src/core/remote-catalog-provider.ts#L45-L136).

Interactive all-catalog refreshes are coalesced per runtime. Each caller can
cancel its wait; the shared work is aborted only when no waiters remain. See
[`model-catalog-refresh.ts`](https://github.com/badlogic/pi-mono/blob/853a80d26c90a14c1886f0ebb8ffaae133ca2185/packages/coding-agent/src/modes/interactive/model-catalog-refresh.ts).

#### Capability normalization and thinking

Pi normalizes models to a typed record containing Provider/API identity,
reasoning support, `thinkingLevelMap`, input modalities, context/output limits,
cost, headers, sampling parameters, and API compatibility flags. Its neutral
thinking vocabulary is `off | minimal | low | medium | high | xhigh | max`.
`thinkingLevelMap` maps those names to Provider values; `null` removes a level,
while extended levels are available only when explicitly mapped. Unsupported
requests are clamped deterministically. See [`types.ts`](https://github.com/badlogic/pi-mono/blob/853a80d26c90a14c1886f0ebb8ffaae133ca2185/packages/ai/src/types.ts#L810-L850) and
[`models.ts`](https://github.com/badlogic/pi-mono/blob/853a80d26c90a14c1886f0ebb8ffaae133ca2185/packages/ai/src/models.ts#L900-L930).

Pi translates that neutral level in each API adapter. Its compatibility model
also represents Provider-specific reasoning encodings rather than forcing
every server into `reasoning_effort`; the supported forms include OpenAI,
OpenRouter, DeepSeek, Qwen, chat-template, and token-budget variants. See the
[`thinkingFormat` contract](https://github.com/badlogic/pi-mono/blob/853a80d26c90a14c1886f0ebb8ffaae133ca2185/packages/ai/src/types.ts#L555-L613).

Pi does not expose a general service-tier capability analogous to Lenso's
`fast/priority`. Its `cost.tiers` are pricing thresholds based on input token
count, not selectable request service tiers. Provider-specific request fields
can still be expressed through model `samplingParams` or compatibility data,
but those are not a normalized service-tier selector.

#### User control and custom Providers

`~/.pi/agent/models.json` can add Providers/models, add models to a built-in,
override built-in model metadata, or redirect a built-in Provider through a
proxy. It is reloaded when the user opens `/model`. Extension code can also
register a full Provider, including dynamic `refreshModels()`. The actual layer
order is built-in → `models.json` → extension, followed by topmost per-model
user overrides. See [Pi's model configuration documentation](https://github.com/badlogic/pi-mono/blob/853a80d26c90a14c1886f0ebb8ffaae133ca2185/packages/coding-agent/docs/models.md) and
[`provider-composer.ts`](https://github.com/badlogic/pi-mono/blob/853a80d26c90a14c1886f0ebb8ffaae133ca2185/packages/coding-agent/src/core/provider-composer.ts#L420-L510).

Availability is primarily credential-aware: only models whose Provider has
complete authentication appear in normal available-model views, and a Provider
may further filter the complete catalog for a credential. Model scopes accept
glob patterns such as Provider/model patterns and optional thinking suffixes;
they narrow cycling/selection rather than rewrite the Provider's source
catalog. Pi has no equivalent of OpenCode's global enabled/disabled Provider
lists plus per-Provider whitelist/blacklist.

#### CLI and UI consumption

`--list-models` reads `ModelRuntime.getAvailable()`, optionally fuzzy-filters,
sorts by Provider then model ID, and shows context, max output, thinking, and
image support. See [`list-models.ts`](https://github.com/badlogic/pi-mono/blob/853a80d26c90a14c1886f0ebb8ffaae133ca2185/packages/coding-agent/src/cli/list-models.ts).
The interactive selector searches model ID, Provider, `provider/id`, and display
name. Thinking choices come from the selected model's supported-level set, and
per-model startup thinking preferences are stored under `provider/modelId`.

### Inference

Pi treats catalog freshness as availability metadata for a mutable process,
not as part of a Turn's durable identity. Its refresh-generation check prevents
an older asynchronous fetch from overwriting a newer one, but it does not mean
an already-running Turn leases an immutable catalog snapshot.

Its strongest reusable idea for Lenso is the split between `getModels()`
(complete last-known facts), `filterModels()` (credential policy), and
`getAvailable()` (configured/authenticated product surface). That prevents a
user allowlist or missing credential from being confused with what the Provider
actually knows.

## OpenCode

### Observed facts

#### Source of truth and normalization

OpenCode's default catalog source is `https://models.opencode.ai/api.json`,
overridable by `OPENCODE_MODELS_URL`. The catalog schema includes Provider/API
metadata and, per model, release status/date, modalities, context/input/output
limits, costs, tool/attachment/temperature flags, interleaved reasoning, and a
structured `reasoning_options` union:

- `effort` with an array of nullable Provider values;
- `toggle`; or
- `budget_tokens` with optional minimum and maximum.

See [`models-dev.ts`](https://github.com/anomalyco/opencode/blob/1ead9e3d7f02661176fd46d7bcac7f6b7be3b52d/packages/core/src/models-dev.ts#L15-L126).

The catalog deliberately separates lab-owned model metadata from the way a
specific Provider hosts that model. Cost, lifecycle status, request shape, and
`reasoning_options` are Provider-host facts. Its authoring rules explicitly
reject a universal low/medium/high scale: a host may expose enumerated effort,
a binary toggle, a token budget, or no caller control. See the pinned
[models.dev authoring contract](https://github.com/anomalyco/models.dev/blob/8a3ca0a93262a7ee8a85b91a2cbb6f97f20f7787/AGENTS.md#L15-L25)
and [reasoning policy](https://github.com/anomalyco/models.dev/blob/8a3ca0a93262a7ee8a85b91a2cbb6f97f20f7787/AGENTS.md#L181-L217).

OpenCode converts this external schema to its internal Provider/Model records,
normalizes capabilities and limits, selects the concrete AI SDK package, and
derives selectable variants with `ProviderTransform.reasoningVariants()` (or a
legacy variant fallback). Experimental catalog modes can become additional
model IDs with their own cost, request body, and headers. See
[`provider.ts`](https://github.com/anomalyco/opencode/blob/1ead9e3d7f02661176fd46d7bcac7f6b7be3b52d/packages/opencode/src/provider/provider.ts#L1230-L1335) and
[`transform.ts`](https://github.com/anomalyco/opencode/blob/1ead9e3d7f02661176fd46d7bcac7f6b7be3b52d/packages/opencode/src/provider/transform.ts).

Variants are arbitrary option overlays merged into the final request after
base Provider-derived options, model options, and agent options. Thus OpenCode
can represent reasoning efforts and Provider-specific modes through one UI
concept. It does not define a dedicated normalized service-tier type: a tier or
mode is represented as Provider options/experimental modes when the catalog or
configuration supplies it. See [`request.ts`](https://github.com/anomalyco/opencode/blob/1ead9e3d7f02661176fd46d7bcac7f6b7be3b52d/packages/opencode/src/session/llm/request.ts#L55-L95).

#### Cache and refresh

OpenCode caches the models.dev JSON under its global cache directory. An
in-process read is cached indefinitely until invalidated. Disk data is preferred;
if absent, a catalog snapshot compiled into the binary is used; only if both are
absent does startup fetch. Cache writes use a temporary file plus rename and a
cross-process file lock. See [`models-dev.ts`](https://github.com/anomalyco/opencode/blob/1ead9e3d7f02661176fd46d7bcac7f6b7be3b52d/packages/core/src/models-dev.ts#L130-L215).

Refresh uses a five-minute disk TTL, rechecks freshness while holding the lock,
has a ten-second HTTP timeout plus transient retries, invalidates the in-process
snapshot after an atomic write, and publishes a refresh event. A background
task attempts refresh immediately and then every 60 minutes. `opencode models
--refresh` forces the fetch. Refresh errors are logged and ignored, so the
previous disk/in-memory/compiled snapshot remains usable. See
[`models-dev.ts`](https://github.com/anomalyco/opencode/blob/1ead9e3d7f02661176fd46d7bcac7f6b7be3b52d/packages/core/src/models-dev.ts#L145-L240) and
[`models` CLI](https://github.com/anomalyco/opencode/blob/1ead9e3d7f02661176fd46d7bcac7f6b7be3b52d/packages/opencode/src/cli/cmd/models.ts).

#### Provider composition, filtering, and custom Providers

The broad models.dev directory is not the connected runtime set. OpenCode
builds runtime Providers from catalog data plus plugins, configuration,
environment credentials, stored API/OAuth credentials, and built-in custom
loaders. Configuration can define a new Provider/model or overlay catalog
metadata and variants. Some custom loaders can perform Provider-specific model
discovery; at this commit GitLab uses that mechanism and discovery failure is
swallowed, retaining the existing list. See
[`provider.ts`](https://github.com/anomalyco/opencode/blob/1ead9e3d7f02661176fd46d7bcac7f6b7be3b52d/packages/opencode/src/provider/provider.ts#L1380-L1685).

Users can set `enabled_providers` and `disabled_providers`. A Provider config
can also use model `whitelist` or `blacklist`; deprecated models are removed,
and alpha models require the experimental-model flag. Config-defined variants
merge over derived variants, and `disabled: true` removes one. Providers with
no surviving models are removed. See
[`provider.ts`](https://github.com/anomalyco/opencode/blob/1ead9e3d7f02661176fd46d7bcac7f6b7be3b52d/packages/opencode/src/provider/provider.ts#L1435-L1455) and
[`provider.ts`](https://github.com/anomalyco/opencode/blob/1ead9e3d7f02661176fd46d7bcac7f6b7be3b52d/packages/opencode/src/provider/provider.ts#L1685-L1720).

#### CLI, HTTP, ACP, and UI consumption

`opencode models [provider]` lists `provider/model`; verbose mode emits the
normalized record and `--refresh` refreshes models.dev first. The Provider HTTP
surface returns all filtered catalog Providers, defaults, and the connected
subset; the server applies enabled/disabled Provider filters before combining
the broad directory with connected Providers. See
[`models.ts`](https://github.com/anomalyco/opencode/blob/1ead9e3d7f02661176fd46d7bcac7f6b7be3b52d/packages/opencode/src/cli/cmd/models.ts) and
[`provider HTTP handler`](https://github.com/anomalyco/opencode/blob/1ead9e3d7f02661176fd46d7bcac7f6b7be3b52d/packages/opencode/src/server/routes/instance/httpapi/handlers/provider.ts#L35-L75).

ACP exposes the selected model and variant separately, validates that a chosen
variant exists, and derives effort choices from the normalized Provider
snapshot. This confirms that variants are product controls consumed above the
transport rather than frontend-maintained model-name branches. See
[`acp/service.ts`](https://github.com/anomalyco/opencode/blob/1ead9e3d7f02661176fd46d7bcac7f6b7be3b52d/packages/opencode/src/acp/service.ts#L896-L932).

### Inference

OpenCode optimizes for a comprehensive, quickly updating marketplace-like
directory and graceful offline startup. Because refresh errors are deliberately
ignored and Providers are composed into mutable instance state, catalog
freshness is not a readiness invariant. That is appropriate for discovery UI,
but weaker than Lenso's guarantee that a candidate Generation cannot route a
Turn until its selected Provider catalog has been validated.

`reasoning_options` is more future-proof than a flat array of effort strings:
it distinguishes enumerated effort, simple enable/disable, and numeric budget.
OpenCode then deliberately compiles that richer source shape into UI/request
variants. This is the clearest design to learn from.

## Comparison with the pre-ADR-0093 Lenso baseline

| Concern | Pi | OpenCode | Lenso today |
| --- | --- | --- | --- |
| Primary ownership | Provider contract | Shared models.dev directory, then Provider composition | Selected Model Provider |
| Baseline | Generated built-ins | Compiled models.dev snapshot | Host/configured Provider facts |
| Live source | pi.dev overlay or Provider hook | models.opencode.ai plus a few Provider discoverers | Selected Provider endpoint |
| Refresh scope | Selected/all configured dynamic Providers | Global directory background refresh | Candidate Generation reconciliation |
| Failure | Keep prior list, return per-Provider error | Log/ignore and keep disk/compiled snapshot | Reject candidate; previous Generation remains routable |
| Stable Turn snapshot | No | No | Yes, Generation-frozen |
| Reasoning model | Seven neutral levels + Provider mapping | effort/toggle/budget source compiled to variants | Provider-normalized selectable levels |
| Service tiers | No first-class selector | Generic variants/modes | First-class normalized tiers |
| Filtering | Auth filter + model scopes | Provider enable/disable; model white/blacklist; variant disable | Provider facts plus explicit visibility policy |
| Custom Provider | `models.json` or extension Provider | config or plugin Provider | removable Provider Plugin |

## Recommendations for Lenso

These are recommendations, not descriptions of existing behavior.

1. **Keep the Generation freeze and Ready Gate.** Do not adopt mutable hourly
   catalog replacement inside a routable Generation. A refresh creates a
   candidate Generation; successful validation atomically advances routing;
   failure leaves the active Generation and its catalog unchanged.

2. **Give Model Providers a Pi-like acquisition contract internally.** Let a
   selected Provider receive `{ previous_validated_snapshot, credential,
   force, signal }` and return a candidate catalog plus provenance/freshness.
   Guard publication with candidate/Generation identity so aborted or
   superseded discovery cannot commit. Keep this lifecycle machinery out of the
   portable model Capability request.

3. **Separate facts, availability, and visibility.** Define three projections:
   Provider-known catalog; credential/account-available catalog; user-visible
   catalog. Replace the ambiguous Codex-only `allowed_models` behavior with an
   explicit visibility policy, ideally `include_models`/`exclude_models` glob
   patterns, while always validating the configured primary model. A filter
   must never change stored Provider facts.

4. **Generalize reasoning metadata before adding more model-name branches.** A
   compatible next contract should distinguish `effort { values }`, `toggle`,
   and `budget_tokens { min, max, default }`. Providers map those controls to
   transport fields. Existing discrete reasoning levels can remain a derived
   UI projection.

5. **Keep service tier distinct from reasoning.** OpenCode's generic variants
   are convenient but can mix reasoning, latency, price, and experimental API
   modes into an opaque map. Lenso should retain typed `service_tiers` and add a
   generic Provider variant only if a real control cannot fit a stable semantic
   category. A model may expose both a reasoning control and a service tier.

6. **Add snapshot provenance and freshness policy.** Record Provider instance,
   acquisition source, fetched/validated time, optional ETag/revision, and
   whether the candidate used live, cached, or configured facts. Permit a
   Provider to define a bounded stale-cache policy, but make use of stale data
   visible and deterministic. Never silently convert “network failed” into an
   unlabelled success.

7. **Refresh only selected Providers.** OpenCode's global directory is useful
   for a provider marketplace, but Lenso's App must not activate credentials or
   network access for unselected Plugins. An optional unauthenticated discovery
   directory can power installation UX later; it must remain separate from the
   effective Generation catalog.

8. **Make every consumer use the same immutable projection.** CLI, TUI, Web,
   ACP, and per-Turn validation should read one Generation catalog contract.
   Frontends may search, group, and hide entries, but must not reconstruct
   reasoning levels, tiers, or limits from model IDs.

## Suggested acceptance tests

- A discovered model added upstream appears after successful reconciliation
  without a Host release; a removed selected model rejects the candidate.
- An aborted/superseded refresh cannot publish into a newer candidate.
- A network failure with an admissible cached snapshot produces explicit stale
  provenance; without one, readiness fails and the active Generation survives.
- Include/exclude filters affect only visibility and cannot hide the configured
  primary model without a configuration error.
- `effort`, `toggle`, and token-budget reasoning Providers produce only valid
  controls, and UI/ACP submit the Provider-mapped value without model-name logic.
- Reasoning and service tier remain independently selectable when a model
  supports both.
- Unselected Providers perform no authenticated catalog request.
- CLI, TUI, Web, and ACP expose identical model/control sets for the same
  Generation digest.

## First implemented slice

[ADR-0093](../adr/0093-separate-provider-model-facts-from-visibility.md)
implements the first recommendation without changing the portable Model
Capability: the direct Codex Provider retains every valid discovered model,
projects exact-ID `include_models`/`exclude_models` through the existing
`hidden` fact, and treats legacy `allowed_models` as a deprecated no-op
migration input. Catalog admission and validation remain frozen with the
Generation; visibility no longer erases Provider facts.
