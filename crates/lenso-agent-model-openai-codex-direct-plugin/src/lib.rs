//! Experimental direct `ChatGPT` subscription Model Plugin.

mod websocket;

use std::{
    any::Any,
    cell::{Cell, RefCell},
    collections::{BTreeMap, BTreeSet, VecDeque},
    fmt, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures::{
    StreamExt,
    future::{LocalBoxFuture, ready},
    stream::LocalBoxStream,
};
use lenso::prelude::*;
use lenso_capability_agent_auth_openai_codex::{
    self as auth_contract, AccessRequest, OpenaiCodexInvocationError,
};
use lenso_capability_agent_model::{
    self as model_contract, CAPABILITY_ID, CatalogControl, CatalogControlMode,
    CatalogControlOption, CatalogControlStatus, CatalogFreshness, CatalogInputModality,
    CatalogModel, CatalogModelLimits, CatalogProvenance, CatalogRequest, CatalogResponse,
    CatalogSource, CatalogWireProtocol, CompleteError, CompleteMessage, CompleteMessageInput,
    CompleteMessageKind, CompleteMessageRole, CompleteOpen, ModelCatalog,
    ModelCompleteInvocationError, ModelProvider, ProviderFailurePayload,
};
use lenso_kernel::{InvocationContext, NativeStreamItem, NativeStreamSession, RuntimeFailure};
use reqwest::{StatusCode, header};
use sha2::{Digest as _, Sha256};

const DEFAULT_BASE_URL: &str = "https://chatgpt.com/backend-api";
// Codex's own catalog refresh uses this compatibility ceiling to request the
// complete Provider catalog rather than filtering it by the caller's release.
const CODEX_CATALOG_CLIENT_VERSION: &str = "99.99.99";
const MAX_EVENT_BYTES: usize = 1024 * 1024;
const MAX_CATALOG_BYTES: usize = 4 * 1024 * 1024;
const MAX_CACHE_BYTES: u64 = 8 * 1024 * 1024;
const MAX_STALE_SECONDS: u64 = 7 * 24 * 60 * 60;
const MAX_REFRESH_SECONDS: u64 = 24 * 60 * 60;
const CACHE_SCHEMA: &str = "lenso.agent.model-catalog-cache.v1";
const SNAPSHOT_SCHEMA: &str = "lenso.agent.model-catalog-provider-snapshot.v1";

static NEXT_REFRESH_PUBLISHER: AtomicU64 = AtomicU64::new(1);

thread_local! {
    static REFRESH_PUBLISHERS: RefCell<BTreeMap<PathBuf, u64>> = const { RefCell::new(BTreeMap::new()) };
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectModelConfig {
    #[serde(default)]
    transport: websocket::Transport,
    base_url: String,
    model: String,
    /// Deprecated migration input. Catalog admission is now Provider-owned.
    #[serde(default)]
    allowed_models: Option<Vec<String>>,
    #[serde(default)]
    include_models: Option<Vec<String>>,
    #[serde(default)]
    exclude_models: Vec<String>,
    #[serde(default)]
    catalog_cache_path: Option<PathBuf>,
    #[serde(default)]
    catalog_max_stale_seconds: u64,
    #[serde(default)]
    /// Compatibility name for the Provider-owned effective publication path.
    catalog_snapshot_path: Option<PathBuf>,
    #[serde(default)]
    catalog_refresh_seconds: u64,
    reasoning_effort: String,
    max_event_bytes: usize,
}

impl DirectModelConfig {
    fn validate(self) -> Result<Self, RuntimeFailure> {
        let allowed_models = self.allowed_models.as_deref().unwrap_or_default();
        let include_models = self.include_models.as_deref().unwrap_or_default();
        let exclude_models = self.exclude_models.as_slice();
        let include_ids = include_models.iter().collect::<BTreeSet<_>>();
        let exclude_ids = exclude_models.iter().collect::<BTreeSet<_>>();
        if !valid_model_id(&self.model)
            || allowed_models.len() > 128
            || include_models.len() > 128
            || exclude_models.len() > 128
            || allowed_models
                .iter()
                .any(|model| !valid_model_id(model) || model == &self.model)
            || allowed_models.iter().collect::<BTreeSet<_>>().len() != allowed_models.len()
            || include_models
                .iter()
                .any(|model| !valid_model_id(model) || model == &self.model)
            || include_ids.len() != include_models.len()
            || exclude_models
                .iter()
                .any(|model| !valid_model_id(model) || model == &self.model)
            || exclude_ids.len() != exclude_models.len()
            || !include_ids.is_disjoint(&exclude_ids)
            || self.catalog_max_stale_seconds > MAX_STALE_SECONDS
            || (self.catalog_max_stale_seconds > 0 && self.catalog_cache_path.is_none())
            || self.catalog_refresh_seconds > MAX_REFRESH_SECONDS
            || (self.catalog_refresh_seconds > 0 && self.catalog_snapshot_path.is_none())
            || [
                self.catalog_cache_path.as_ref(),
                self.catalog_snapshot_path.as_ref(),
            ]
            .into_iter()
            .flatten()
            .any(|path| !path.is_absolute() || path.to_str().is_none() || path.parent().is_none())
            || self.reasoning_effort.is_empty()
            || self.reasoning_effort.len() > 32
            || self.max_event_bytes == 0
            || self.max_event_bytes > MAX_EVENT_BYTES
        {
            return Err(invalid_plan(
                "direct Codex model, visibility policy, catalog cache, or max_event_bytes is invalid",
            ));
        }
        let endpoint = self.endpoint()?;
        let official = self.base_url.trim_end_matches('/') == DEFAULT_BASE_URL;
        let loopback = endpoint.scheme() == "http"
            && endpoint
                .host_str()
                .is_some_and(|host| matches!(host, "127.0.0.1" | "localhost" | "::1"));
        if (!official && !loopback)
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(invalid_plan(
                "direct Codex base_url must be chatgpt.com/backend-api or loopback HTTP",
            ));
        }
        Ok(self)
    }

    fn endpoint(&self) -> Result<reqwest::Url, RuntimeFailure> {
        reqwest::Url::parse(&format!(
            "{}/codex/responses",
            self.base_url.trim_end_matches('/')
        ))
        .map_err(|_| invalid_plan("direct Codex base_url is invalid"))
    }

    fn catalog_endpoint(&self) -> Result<reqwest::Url, RuntimeFailure> {
        reqwest::Url::parse(&format!(
            "{}/codex/models?client_version={}",
            self.base_url.trim_end_matches('/'),
            CODEX_CATALOG_CLIENT_VERSION
        ))
        .map_err(|_| invalid_plan("direct Codex catalog URL is invalid"))
    }

    fn model_is_visible(&self, model: &str) -> bool {
        if model == self.model {
            return true;
        }
        let included = self
            .include_models
            .as_ref()
            .is_none_or(|models| models.iter().any(|candidate| candidate == model));
        included
            && !self
                .exclude_models
                .iter()
                .any(|candidate| candidate == model)
    }
}

fn valid_model_id(model: &str) -> bool {
    model.trim() == model && !model.is_empty() && model.len() <= 256
}

fn validate_config(config: &DirectModelConfig) -> Result<(), RuntimeFailure> {
    config.clone().validate().map(|_| ())
}

#[lenso::plugin(
    lifecycle,
    configuration_schema = "config.schema.json",
    validate = validate_config
)]
#[derive(Clone, Debug)]
struct DirectModel {
    #[config]
    config: DirectModelConfig,
    client: reqwest::Client,
    websocket: websocket::Pool,
    auth: Port<auth_contract::OpenaiCodexClient>,
    catalog: Rc<RefCell<Option<CatalogResponse>>>,
    #[tasks]
    tasks: ManagedTasks,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
struct CodexModelsResponse {
    models: Vec<CodexModel>,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
struct CodexModel {
    slug: String,
    display_name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    default_reasoning_level: Option<String>,
    #[serde(default)]
    supported_reasoning_levels: Vec<CodexReasoningEffort>,
    #[serde(default = "listed_visibility")]
    visibility: String,
    #[serde(default)]
    additional_speed_tiers: Vec<String>,
    #[serde(default)]
    service_tiers: Vec<CodexServiceTier>,
    #[serde(default)]
    default_service_tier: Option<String>,
    #[serde(default)]
    supports_parallel_tool_calls: bool,
    #[serde(default)]
    context_window: Option<i64>,
    #[serde(default)]
    max_context_window: Option<i64>,
    #[serde(default = "default_effective_context_window_percent")]
    effective_context_window_percent: i64,
    #[serde(default)]
    comp_hash: Option<String>,
    #[serde(default = "default_codex_input_modalities")]
    input_modalities: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
struct CodexReasoningEffort {
    effort: String,
    description: String,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
struct CodexServiceTier {
    id: String,
    name: String,
    description: String,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct CachedCatalog {
    schema: String,
    source_key: String,
    fetched_at_unix_seconds: u64,
    revision: String,
    etag: Option<String>,
    response: CodexModelsResponse,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct ProviderCatalogSnapshot {
    schema: String,
    source_key: String,
    fetched_at_unix_seconds: u64,
    revision: String,
    response: CodexModelsResponse,
}

struct AcquiredCatalog {
    catalog: CatalogResponse,
    snapshot: ProviderCatalogSnapshot,
}

fn listed_visibility() -> String {
    "list".to_owned()
}

const fn default_effective_context_window_percent() -> i64 {
    95
}

fn default_codex_input_modalities() -> Vec<String> {
    vec!["text".to_owned(), "image".to_owned()]
}

#[lenso::provides(model_contract::Model)]
impl ModelProvider for DirectModel {
    fn catalog(
        &self,
        _context: InvocationContext,
        _request: CatalogRequest,
    ) -> lenso_kernel::NativeRequestFuture<ModelCatalog> {
        let result = self
            .catalog
            .borrow()
            .clone()
            .ok_or(RuntimeFailure::Unavailable {
                capability: model_contract::CAPABILITY_ID,
            });
        Box::pin(ready(result.map(Ok)))
    }

    fn complete(
        &self,
        context: InvocationContext,
        request: CompleteOpen,
    ) -> LocalBoxFuture<'static, Result<Box<dyn NativeStreamSession>, ModelCompleteInvocationError>>
    {
        let catalog = self.catalog.borrow();
        let selected_model = catalog.as_ref().and_then(|catalog| {
            catalog
                .models
                .iter()
                .find(|model| model.id == request.model)
        });
        let Some(selected_model) = selected_model else {
            return Box::pin(ready(Err(ModelCompleteInvocationError::Domain(
                CompleteError::UnsupportedModel,
            ))));
        };
        let reasoning_effort = request
            .reasoning_effort
            .as_deref()
            .unwrap_or(&self.config.reasoning_effort);
        if request.reasoning_enabled.is_some()
            || request.reasoning_budget_tokens.is_some()
            || !control_supports(&selected_model.reasoning, reasoning_effort)
        {
            return Box::pin(ready(Err(ModelCompleteInvocationError::Domain(
                CompleteError::InvalidRequest,
            ))));
        }
        drop(catalog);
        let wire_request = match responses_request(&request, reasoning_effort) {
            Ok(body) => body,
            Err(error) => return Box::pin(ready(Err(ModelCompleteInvocationError::Domain(error)))),
        };
        let auth = self.auth.clone();
        let config = self.config.clone();
        let client = self.client.clone();
        let websocket = self.websocket.clone();
        Box::pin(async move {
            let credential = auth
                .access_with_context(context, AccessRequest {})
                .await
                .map_err(map_auth_error)?;
            if config.transport != websocket::Transport::Sse {
                match websocket.open(&config, &credential, &wire_request).await {
                    Ok(stream) => return Ok(Box::new(stream) as Box<dyn NativeStreamSession>),
                    Err(websocket::OpenError::Unsupported)
                        if config.transport == websocket::Transport::Auto => {}
                    Err(error) => return Err(error.into_model_error()),
                }
            }
            let response = client
                .post(
                    config
                        .endpoint()
                        .map_err(ModelCompleteInvocationError::Runtime)?,
                )
                .bearer_auth(credential.access_token)
                .header("chatgpt-account-id", credential.account_id)
                .header("originator", "lenso")
                .header("User-Agent", "lenso-agent/0.1.0")
                .header("OpenAI-Beta", "responses=experimental")
                .header("Accept", "text/event-stream")
                .json(&wire_request.body)
                .send()
                .await
                .map_err(|error| {
                    provider_failure(
                        "transport_error",
                        "direct Codex request failed",
                        error.is_connect(),
                    )
                })?;
            if !response.status().is_success() {
                return Err(map_status(response.status()));
            }
            let chunks = response.bytes_stream().boxed_local();
            Ok(Box::new(DirectCodexStream::new(
                chunks,
                config.max_event_bytes,
                wire_request.provider_to_lenso_tool_names,
            )) as Box<dyn NativeStreamSession>)
        })
    }
}

impl DirectModel {
    async fn acquire_catalog(
        &self,
        access_token: &str,
        account_id: &str,
    ) -> Result<AcquiredCatalog, RuntimeFailure> {
        let now = unix_now()?;
        let source_key = cache_source_key(&self.config.base_url, account_id);
        let cached = self
            .config
            .catalog_cache_path
            .as_deref()
            .and_then(|path| read_cache(path, &source_key, now).ok().flatten());
        let mut request = self
            .client
            .get(self.config.catalog_endpoint()?)
            .bearer_auth(access_token)
            .header("chatgpt-account-id", account_id)
            .header("originator", "lenso")
            .header(
                "User-Agent",
                concat!("lenso-agent/", env!("CARGO_PKG_VERSION")),
            )
            .header("Accept", "application/json");
        if let Some(etag) = cached.as_ref().and_then(|cached| cached.etag.as_deref()) {
            request = request.header(header::IF_NONE_MATCH, etag);
        }
        let Ok(response) = request.send().await else {
            let catalog = stale_catalog(&self.config, cached.clone(), now, "request failed")?;
            let snapshot = snapshot_from_catalog(&source_key, &catalog, cached.as_ref())?;
            return Ok(AcquiredCatalog { catalog, snapshot });
        };
        self.accept_catalog_response(response, cached, source_key, now)
            .await
    }

    async fn accept_catalog_response(
        &self,
        response: reqwest::Response,
        cached: Option<CachedCatalog>,
        source_key: String,
        now: u64,
    ) -> Result<AcquiredCatalog, RuntimeFailure> {
        if response.status() == StatusCode::NOT_MODIFIED {
            let mut cached = cached.ok_or_else(|| RuntimeFailure::PluginFailure {
                detail: "direct Codex model catalog returned 304 without a validated cache"
                    .to_owned(),
            })?;
            cached.fetched_at_unix_seconds = now;
            if let Some(etag) = response_etag(&response) {
                cached.revision.clone_from(&etag);
                cached.etag = Some(etag);
            }
            let catalog = project_codex_catalog(
                &self.config,
                cached.response.clone(),
                cache_provenance(&self.config, &cached, now, CatalogFreshness::Revalidated),
            )?;
            if let Some(path) = self.config.catalog_cache_path.as_deref() {
                write_cache(path, &cached)?;
            }
            let snapshot = snapshot_from_catalog(&source_key, &catalog, Some(&cached))?;
            return Ok(AcquiredCatalog { catalog, snapshot });
        }
        if !response.status().is_success() {
            if transient_catalog_status(response.status()) {
                let detail = format!("returned HTTP {}", response.status().as_u16());
                let catalog = stale_catalog(&self.config, cached.clone(), now, &detail)?;
                let snapshot = snapshot_from_catalog(&source_key, &catalog, cached.as_ref())?;
                return Ok(AcquiredCatalog { catalog, snapshot });
            }
            return Err(RuntimeFailure::PluginFailure {
                detail: format!(
                    "direct Codex model catalog returned HTTP {}",
                    response.status().as_u16()
                ),
            });
        }
        self.accept_fresh_catalog_response(response, source_key, now)
            .await
    }

    async fn accept_fresh_catalog_response(
        &self,
        response: reqwest::Response,
        source_key: String,
        now: u64,
    ) -> Result<AcquiredCatalog, RuntimeFailure> {
        let etag = response_etag(&response);
        if response
            .content_length()
            .is_some_and(|length| length > MAX_CATALOG_BYTES as u64)
        {
            return Err(invalid_plan("direct Codex model catalog is too large"));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|_| RuntimeFailure::PluginFailure {
                detail: "direct Codex model catalog body failed".to_owned(),
            })?;
        if bytes.len() > MAX_CATALOG_BYTES {
            return Err(invalid_plan("direct Codex model catalog is too large"));
        }
        let response = serde_json::from_slice::<CodexModelsResponse>(&bytes)
            .map_err(|_| invalid_plan("direct Codex model catalog response is invalid"))?;
        let acquisition_revision = etag
            .clone()
            .unwrap_or_else(|| format!("sha256:{:x}", Sha256::digest(&bytes)));
        let provenance = live_provenance(&self.config, now, &acquisition_revision);
        let catalog = project_codex_catalog(&self.config, response.clone(), provenance)?;
        let snapshot = ProviderCatalogSnapshot {
            schema: SNAPSHOT_SCHEMA.to_owned(),
            source_key: source_key.clone(),
            fetched_at_unix_seconds: now,
            revision: projected_catalog_revision(&catalog)?,
            response: response.clone(),
        };
        if let Some(path) = self.config.catalog_cache_path.as_deref() {
            write_cache(
                path,
                &CachedCatalog {
                    schema: CACHE_SCHEMA.to_owned(),
                    source_key,
                    fetched_at_unix_seconds: now,
                    revision: acquisition_revision,
                    etag,
                    response,
                },
            )?;
        }
        Ok(AcquiredCatalog { catalog, snapshot })
    }
}

impl Lifecycle for DirectModel {
    async fn activate(&self, _context: ActivateContext) -> Result<(), RuntimeFailure> {
        if self.config.transport != websocket::Transport::Sse {
            let pool = self.websocket.clone();
            let cancellation = self
                .tasks
                .cancellation()
                .map_err(|_| protocol_failure("direct Codex WebSocket lifecycle is unavailable"))?;
            self.tasks
                .spawn_local(async move {
                    let mut ticks = tokio::time::interval(Duration::from_secs(30));
                    loop {
                        tokio::select! {
                            () = cancellation.cancelled() => { pool.shutdown(); break; }
                            _ = ticks.tick() => pool.prune_idle(),
                        }
                    }
                })
                .map_err(|_| {
                    protocol_failure("direct Codex WebSocket maintenance failed to start")
                })?;
        }
        let credential = self.auth.access(AccessRequest {}).await.map_err(|error| {
            RuntimeFailure::PluginFailure {
                detail: format!("direct Codex model catalog authentication failed: {error:?}"),
            }
        })?;
        let acquired = self
            .acquire_catalog(&credential.access_token, &credential.account_id)
            .await?;
        let publisher = self
            .config
            .catalog_snapshot_path
            .as_deref()
            .map(claim_refresh_publisher);
        if let (Some(path), Some(publisher)) =
            (self.config.catalog_snapshot_path.clone(), publisher)
        {
            publish_provider_snapshot(&path, &acquired.snapshot, publisher)?;
            self.catalog.replace(Some(acquired.catalog));
            if self.config.catalog_refresh_seconds > 0 {
                let plugin = self.clone();
                let interval = Duration::from_secs(self.config.catalog_refresh_seconds);
                let cancellation = self.tasks.cancellation().map_err(|error| {
                    RuntimeFailure::PluginFailure {
                        detail: format!(
                            "direct Codex catalog refresh cancellation is unavailable: {error:?}"
                        ),
                    }
                })?;
                self.tasks
                    .spawn_local(async move {
                        let mut ticks = tokio::time::interval(interval);
                        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                        ticks.tick().await;
                        loop {
                            tokio::select! {
                                () = cancellation.cancelled() => break,
                                _ = ticks.tick() => {}
                            }
                            if !is_refresh_publisher(&path, publisher) {
                                break;
                            }
                            let credential = tokio::select! {
                                () = cancellation.cancelled() => break,
                                credential = plugin.auth.access(AccessRequest {}) => credential,
                            };
                            let Ok(credential) = credential else {
                                continue;
                            };
                            let acquired = tokio::select! {
                                () = cancellation.cancelled() => break,
                                acquired = plugin.acquire_catalog(
                                    &credential.access_token,
                                    &credential.account_id,
                                ) => acquired,
                            };
                            let Ok(acquired) = acquired else {
                                continue;
                            };
                            if publish_provider_snapshot(&path, &acquired.snapshot, publisher)
                                .is_ok()
                                && is_refresh_publisher(&path, publisher)
                            {
                                plugin.catalog.replace(Some(acquired.catalog));
                            }
                        }
                    })
                    .map_err(|error| RuntimeFailure::PluginFailure {
                        detail: format!(
                            "direct Codex catalog refresh task failed to start: {error:?}"
                        ),
                    })?;
            }
        } else {
            self.catalog.replace(Some(acquired.catalog));
        }
        Ok(())
    }

    #[allow(clippy::unused_async_trait_impl)]
    async fn deactivate(&self, _context: DeactivateContext) -> Result<(), RuntimeFailure> {
        self.websocket.shutdown();
        self.catalog.replace(None);
        Ok(())
    }
}

fn snapshot_from_catalog(
    source_key: &str,
    catalog: &CatalogResponse,
    cache: Option<&CachedCatalog>,
) -> Result<ProviderCatalogSnapshot, RuntimeFailure> {
    let cache = cache.ok_or_else(|| RuntimeFailure::PluginFailure {
        detail: format!("direct Codex catalog cache for `{source_key}` is unavailable"),
    })?;
    Ok(ProviderCatalogSnapshot {
        schema: SNAPSHOT_SCHEMA.to_owned(),
        source_key: cache.source_key.clone(),
        fetched_at_unix_seconds: cache.fetched_at_unix_seconds,
        revision: projected_catalog_revision(catalog)?,
        response: cache.response.clone(),
    })
}

fn claim_refresh_publisher(path: &Path) -> u64 {
    let publisher = NEXT_REFRESH_PUBLISHER.fetch_add(1, Ordering::Relaxed);
    REFRESH_PUBLISHERS.with(|publishers| {
        publishers
            .borrow_mut()
            .insert(path.to_path_buf(), publisher);
    });
    publisher
}

fn is_refresh_publisher(path: &Path, publisher: u64) -> bool {
    REFRESH_PUBLISHERS.with(|publishers| publishers.borrow().get(path) == Some(&publisher))
}

fn publish_provider_snapshot(
    path: &Path,
    snapshot: &ProviderCatalogSnapshot,
    publisher: u64,
) -> Result<bool, RuntimeFailure> {
    if !is_refresh_publisher(path, publisher) {
        return Ok(false);
    }
    if let Ok(bytes) = fs::read(path)
        && let Ok(current) = serde_json::from_slice::<ProviderCatalogSnapshot>(&bytes)
        && current.schema == SNAPSHOT_SCHEMA
        && current.source_key == snapshot.source_key
        && current.revision == snapshot.revision
    {
        return Ok(false);
    }
    write_json_atomically(path, snapshot, "Provider catalog snapshot")?;
    Ok(true)
}

fn write_json_atomically(
    path: &Path,
    value: &impl serde::Serialize,
    label: &str,
) -> Result<(), RuntimeFailure> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid_plan(format!("direct Codex {label} path has no parent")))?;
    fs::create_dir_all(parent).map_err(|error| cache_io_failure(&error))?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| cache_io_failure(&error))?;
    serde_json::to_writer(temporary.as_file_mut(), value).map_err(|error| {
        RuntimeFailure::PluginFailure {
            detail: format!("direct Codex {label} serialization failed: {error}"),
        }
    })?;
    temporary
        .as_file_mut()
        .flush()
        .map_err(|error| cache_io_failure(&error))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| cache_io_failure(&error))?;
    temporary
        .persist(path)
        .map_err(|error| cache_io_failure(&error.error))?;
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| cache_io_failure(&error))
}

fn unix_now() -> Result<u64, RuntimeFailure> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| RuntimeFailure::PluginFailure {
            detail: "system clock is before the Unix epoch".to_owned(),
        })
}

fn cache_source_key(base_url: &str, account_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(base_url.trim_end_matches('/').as_bytes());
    digest.update([0]);
    digest.update(account_id.as_bytes());
    let digest = digest.finalize();
    format!("sha256:{digest:x}")
}

fn read_cache(path: &Path, source_key: &str, now: u64) -> Result<Option<CachedCatalog>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("failed to inspect catalog cache: {error}")),
    };
    if !metadata.file_type().is_file() || metadata.len() > MAX_CACHE_BYTES {
        return Err("catalog cache is not a bounded regular file".to_owned());
    }
    let bytes = fs::read(path).map_err(|error| format!("failed to read catalog cache: {error}"))?;
    let cache = serde_json::from_slice::<CachedCatalog>(&bytes)
        .map_err(|_| "catalog cache document is invalid".to_owned())?;
    if cache.schema != CACHE_SCHEMA
        || cache.source_key != source_key
        || cache.fetched_at_unix_seconds == 0
        || cache.fetched_at_unix_seconds > now
        || cache.revision.is_empty()
        || cache.revision.len() > 256
        || cache
            .etag
            .as_deref()
            .is_some_and(|etag| etag.is_empty() || etag.len() > 256)
    {
        return Err("catalog cache identity or metadata is invalid".to_owned());
    }
    Ok(Some(cache))
}

fn write_cache(path: &Path, cache: &CachedCatalog) -> Result<(), RuntimeFailure> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid_plan("direct Codex catalog cache path has no parent"))?;
    fs::create_dir_all(parent).map_err(|error| cache_io_failure(&error))?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| cache_io_failure(&error))?;
    serde_json::to_writer(temporary.as_file_mut(), cache).map_err(|error| {
        RuntimeFailure::PluginFailure {
            detail: format!("direct Codex model catalog cache serialization failed: {error}"),
        }
    })?;
    temporary
        .as_file_mut()
        .flush()
        .map_err(|error| cache_io_failure(&error))?;
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| cache_io_failure(&error))?;
    temporary
        .persist(path)
        .map_err(|error| cache_io_failure(&error.error))?;
    fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| cache_io_failure(&error))
}

fn cache_io_failure(error: &io::Error) -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: format!("direct Codex model catalog cache write failed: {error}"),
    }
}

fn response_etag(response: &reqwest::Response) -> Option<String> {
    response
        .headers()
        .get(header::ETAG)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty() && value.len() <= 256)
        .map(str::to_owned)
}

fn transient_catalog_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_MANY_REQUESTS
    ) || status.is_server_error()
}

fn stale_catalog(
    config: &DirectModelConfig,
    cached: Option<CachedCatalog>,
    now: u64,
    failure: &str,
) -> Result<CatalogResponse, RuntimeFailure> {
    let cached = cached.ok_or_else(|| RuntimeFailure::PluginFailure {
        detail: format!("direct Codex model catalog {failure} and no validated cache is available"),
    })?;
    let age = now.saturating_sub(cached.fetched_at_unix_seconds);
    if config.catalog_max_stale_seconds == 0 || age > config.catalog_max_stale_seconds {
        return Err(RuntimeFailure::PluginFailure {
            detail: format!(
                "direct Codex model catalog {failure} and cached snapshot age {age}s exceeds the {}s policy",
                config.catalog_max_stale_seconds
            ),
        });
    }
    project_codex_catalog(
        config,
        cached.response.clone(),
        cache_provenance(config, &cached, now, CatalogFreshness::Stale),
    )
}

fn live_provenance(config: &DirectModelConfig, now: u64, revision: &str) -> CatalogProvenance {
    CatalogProvenance {
        source: CatalogSource::Live,
        freshness: CatalogFreshness::Fresh,
        fetched_at_unix_seconds: Some(Some(now.to_string())),
        validated_at_unix_seconds: Some(Some(now.to_string())),
        revision: Some(Some(revision.to_owned())),
        max_stale_seconds: Some(Some(config.catalog_max_stale_seconds.to_string())),
    }
}

fn cache_provenance(
    config: &DirectModelConfig,
    cached: &CachedCatalog,
    now: u64,
    freshness: CatalogFreshness,
) -> CatalogProvenance {
    CatalogProvenance {
        source: CatalogSource::Cache,
        freshness,
        fetched_at_unix_seconds: Some(Some(cached.fetched_at_unix_seconds.to_string())),
        validated_at_unix_seconds: Some(Some(now.to_string())),
        revision: Some(Some(cached.revision.clone())),
        max_stale_seconds: Some(Some(config.catalog_max_stale_seconds.to_string())),
    }
}

fn project_codex_catalog(
    config: &DirectModelConfig,
    response: CodexModelsResponse,
    provenance: CatalogProvenance,
) -> Result<CatalogResponse, RuntimeFailure> {
    if response.models.is_empty() || response.models.len() > 128 {
        return Err(invalid_plan(
            "direct Codex model catalog must contain one to 128 models",
        ));
    }
    let mut ids = BTreeSet::new();
    let mut models = Vec::new();
    for model in response.models {
        if model.visibility == "none" {
            continue;
        }
        let mut projected = project_codex_model(model)?;
        projected.hidden |= !config.model_is_visible(&projected.id);
        if !ids.insert(projected.id.clone()) {
            return Err(invalid_plan(
                "direct Codex model catalog contains duplicate models",
            ));
        }
        models.push(projected);
    }
    models.sort_by(|left, right| left.id.cmp(&right.id));
    if models.is_empty() || !models.iter().any(|model| model.id == config.model) {
        return Err(invalid_plan(
            "configured direct Codex model is absent from the Provider catalog",
        ));
    }
    let selected = models
        .iter()
        .find(|model| model.id == config.model)
        .expect("configured model presence was checked");
    if !control_supports(&selected.reasoning, &config.reasoning_effort) {
        return Err(invalid_plan(
            "configured reasoning effort is absent from the Provider catalog",
        ));
    }
    Ok(CatalogResponse { models, provenance })
}

fn projected_catalog_revision(catalog: &CatalogResponse) -> Result<String, RuntimeFailure> {
    serde_json::to_vec(&catalog.models)
        .map(|bytes| format!("sha256:{:x}", Sha256::digest(bytes)))
        .map_err(|_| invalid_plan("direct Codex model catalog revision is unavailable"))
}

fn project_codex_model(model: CodexModel) -> Result<CatalogModel, RuntimeFailure> {
    if !valid_model_id(&model.slug)
        || model.display_name.is_empty()
        || model.display_name.len() > 256
        || model
            .description
            .as_deref()
            .is_some_and(|value| value.len() > 4_096)
        || !matches!(model.visibility.as_str(), "list" | "hide" | "none")
        || !(1..=100).contains(&model.effective_context_window_percent)
        || !model.input_modalities.iter().any(|value| value == "text")
    {
        return Err(invalid_plan("direct Codex model metadata is invalid"));
    }
    let context_window = model.context_window.or(model.max_context_window);
    let max_input_tokens = context_window
        .map(|tokens| tokens.saturating_mul(model.effective_context_window_percent) / 100);
    let reasoning = control(
        model.default_reasoning_level,
        model
            .supported_reasoning_levels
            .into_iter()
            .map(|item| CatalogControlOption {
                name: item.effort.clone(),
                id: item.effort,
                description: item.description,
            })
            .collect(),
        true,
    )?;
    let mut tiers = Vec::new();
    for tier in model.service_tiers {
        tiers.push(CatalogControlOption {
            id: tier.id,
            name: tier.name,
            description: tier.description,
        });
    }
    for tier in model.additional_speed_tiers {
        tiers.push(CatalogControlOption {
            id: tier.clone(),
            name: tier,
            description: "Provider speed tier".to_owned(),
        });
    }
    let service_tiers = control(model.default_service_tier, tiers, false)?;
    let compaction_compatibility = model
        .comp_hash
        .unwrap_or_else(|| "generic-text-v1".to_owned());
    if compaction_compatibility.is_empty() || compaction_compatibility.len() > 128 {
        return Err(invalid_plan(
            "direct Codex compaction compatibility is invalid",
        ));
    }
    Ok(CatalogModel {
        id: model.slug,
        display_name: model.display_name,
        description: model.description.unwrap_or_default(),
        hidden: model.visibility != "list",
        limits: CatalogModelLimits {
            context_window_tokens: optional_tokens(context_window)?,
            max_input_tokens: optional_tokens(max_input_tokens)?,
            max_output_tokens: None,
        },
        input_modalities: vec![CatalogInputModality::Text],
        text_output: true,
        tool_calls: true,
        parallel_tool_calls: model.supports_parallel_tool_calls,
        reasoning,
        service_tiers,
        wire_protocol: CatalogWireProtocol::OpenaiResponses,
        compaction_compatibility,
    })
}

fn control(
    default: Option<String>,
    mut options: Vec<CatalogControlOption>,
    reasoning: bool,
) -> Result<CatalogControl, RuntimeFailure> {
    let unique_ids = options
        .iter()
        .map(|option| option.id.as_str())
        .collect::<BTreeSet<_>>();
    if options.len() > 16
        || unique_ids.len() != options.len()
        || options.iter().any(|option| {
            option.id.is_empty()
                || option.id.len() > 32
                || option.name.is_empty()
                || option.name.len() > 128
                || option.description.len() > 1_024
        })
        || default
            .as_deref()
            .is_some_and(|value| !options.iter().any(|option| option.id == value))
    {
        return Err(invalid_plan(
            "direct Codex model control metadata is invalid",
        ));
    }
    options.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(CatalogControl {
        status: if options.is_empty() {
            CatalogControlStatus::Unsupported
        } else {
            CatalogControlStatus::Selectable
        },
        mode: (reasoning && !options.is_empty())
            .then_some(CatalogControlMode::Effort)
            .map(Some),
        options,
        default: default.map(Some),
        budget_tokens: None,
    })
}

fn control_supports(control: &CatalogControl, selected: &str) -> bool {
    control.status == CatalogControlStatus::Selectable
        && control.options.iter().any(|option| option.id == selected)
}

#[allow(
    clippy::option_option,
    reason = "generated portable optional fields distinguish omitted from explicit null"
)]
fn optional_tokens(value: Option<i64>) -> Result<Option<Option<String>>, RuntimeFailure> {
    value
        .map(|value| {
            u64::try_from(value)
                .ok()
                .filter(|value| *value > 0)
                .map(|value| Some(value.to_string()))
                .ok_or_else(|| invalid_plan("direct Codex model token limit is invalid"))
        })
        .transpose()
}

struct ResponsesRequest {
    body: serde_json::Value,
    continuation_scope: Option<String>,
    provider_to_lenso_tool_names: BTreeMap<String, String>,
}

fn responses_request(
    request: &CompleteOpen,
    reasoning_effort: &str,
) -> Result<ResponsesRequest, CompleteError> {
    if request.max_output_tokens <= 0 || !request.temperature.is_finite() {
        return Err(CompleteError::InvalidRequest);
    }
    let mut lenso_to_provider_tool_names = BTreeMap::new();
    let mut provider_to_lenso_tool_names = BTreeMap::new();
    for tool in &request.tools {
        let provider_name = provider_tool_name(&tool.name)?;
        if provider_to_lenso_tool_names
            .insert(provider_name.clone(), tool.name.clone())
            .is_some()
        {
            return Err(CompleteError::InvalidRequest);
        }
        lenso_to_provider_tool_names.insert(tool.name.clone(), provider_name);
    }
    let instructions = request
        .messages
        .iter()
        .filter(|message| message.role == CompleteMessageRole::System)
        .map(|message| message.content.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let input = request
        .messages
        .iter()
        .filter(|message| message.role != CompleteMessageRole::System)
        .map(|message| responses_message(message, &lenso_to_provider_tool_names))
        .collect::<Result<Vec<_>, _>>()?;
    let tools = request
        .tools
        .iter()
        .map(|tool| {
            let parameters = serde_json::from_str::<serde_json::Value>(tool.input_schema_json.as_str())
                .map_err(|_| CompleteError::InvalidRequest)?;
            Ok(serde_json::json!({
                "type": "function",
                "name": lenso_to_provider_tool_names.get(&tool.name).ok_or(CompleteError::InvalidRequest)?,
                "description": tool.description,
                "parameters": parameters,
                "strict": false
            }))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut body = serde_json::json!({
        "model": request.model,
        "store": false,
        "stream": true,
        "instructions": if instructions.is_empty() { "You are a helpful assistant." } else { &instructions },
        "input": input,
        "tools": tools,
        "tool_choice": "auto",
        "parallel_tool_calls": true,
        "include": ["reasoning.encrypted_content"],
        "reasoning": { "effort": reasoning_effort, "summary": "auto" },
        "text": { "verbosity": "low" },
    });
    // The ChatGPT Codex endpoint rejects the public Responses API
    // `max_output_tokens` field, so its service owns the wire-level limit.
    if request.temperature != 0.0 {
        body["temperature"] = serde_json::json!(request.temperature);
    }
    if let Some(service_tier) = request.service_tier.as_deref() {
        body["service_tier"] = serde_json::json!(service_tier);
    }
    Ok(ResponsesRequest {
        body,
        continuation_scope: request.continuation_scope.clone(),
        provider_to_lenso_tool_names,
    })
}

fn provider_tool_name(name: &str) -> Result<String, CompleteError> {
    if name.is_empty() {
        return Err(CompleteError::InvalidRequest);
    }
    let name = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .take(128)
        .collect::<String>();
    (!name.is_empty())
        .then_some(name)
        .ok_or(CompleteError::InvalidRequest)
}

fn responses_message(
    message: &CompleteMessageInput,
    lenso_to_provider_tool_names: &BTreeMap<String, String>,
) -> Result<serde_json::Value, CompleteError> {
    match message.role {
        CompleteMessageRole::User => {
            require_no_tool_fields(message)?;
            Ok(serde_json::json!({
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": message.content }]
            }))
        }
        CompleteMessageRole::Assistant => match (
            message.tool_call_id.as_deref(),
            message.tool_name.as_deref(),
            message.arguments_json.as_deref(),
        ) {
            (None, None, None) => Ok(serde_json::json!({
                "type": "message",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": message.content }]
            })),
            (Some(call_id), Some(name), Some(arguments))
                if !call_id.is_empty()
                    && !name.is_empty()
                    && serde_json::from_str::<serde_json::Value>(arguments).is_ok() =>
            {
                let name = lenso_to_provider_tool_names
                    .get(name)
                    .ok_or(CompleteError::InvalidRequest)?;
                Ok(serde_json::json!({
                    "type": "function_call",
                    "call_id": call_id,
                    "name": name,
                    "arguments": arguments
                }))
            }
            _ => Err(CompleteError::InvalidRequest),
        },
        CompleteMessageRole::Tool => {
            let Some(call_id) = message.tool_call_id.as_deref().filter(|id| !id.is_empty()) else {
                return Err(CompleteError::InvalidRequest);
            };
            if message.tool_name.is_some() || message.arguments_json.is_some() {
                return Err(CompleteError::InvalidRequest);
            }
            Ok(serde_json::json!({
                "type": "function_call_output",
                "call_id": call_id,
                "output": message.content
            }))
        }
        CompleteMessageRole::System => Err(CompleteError::InvalidRequest),
    }
}

fn require_no_tool_fields(message: &CompleteMessageInput) -> Result<(), CompleteError> {
    if message.tool_call_id.is_some()
        || message.tool_name.is_some()
        || message.arguments_json.is_some()
    {
        Err(CompleteError::InvalidRequest)
    } else {
        Ok(())
    }
}

fn map_auth_error(_error: OpenaiCodexInvocationError) -> ModelCompleteInvocationError {
    provider_failure(
        "authentication_required",
        "direct Codex authentication failed; run direct login",
        false,
    )
}

fn map_status(status: reqwest::StatusCode) -> ModelCompleteInvocationError {
    match status {
        reqwest::StatusCode::BAD_REQUEST | reqwest::StatusCode::UNPROCESSABLE_ENTITY => {
            ModelCompleteInvocationError::Domain(CompleteError::InvalidRequest)
        }
        reqwest::StatusCode::NOT_FOUND => {
            ModelCompleteInvocationError::Domain(CompleteError::UnsupportedModel)
        }
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => provider_failure(
            "credential_rejected",
            "direct Codex credential was rejected",
            false,
        ),
        reqwest::StatusCode::TOO_MANY_REQUESTS => {
            ModelCompleteInvocationError::Domain(CompleteError::RateLimited)
        }
        reqwest::StatusCode::PAYLOAD_TOO_LARGE => {
            ModelCompleteInvocationError::Domain(CompleteError::ContextOverflow)
        }
        reqwest::StatusCode::SERVICE_UNAVAILABLE => {
            ModelCompleteInvocationError::Domain(CompleteError::Overloaded)
        }
        _ => provider_failure(
            "provider_error",
            "direct Codex provider returned an unsuccessful status",
            status.is_server_error(),
        ),
    }
}

fn provider_failure(
    reason_code: &str,
    message: &str,
    retryable: bool,
) -> ModelCompleteInvocationError {
    ModelCompleteInvocationError::Domain(CompleteError::ProviderFailure {
        payload: ProviderFailurePayload {
            message: message.to_owned(),
            reason_code: reason_code.to_owned(),
            retryable,
        },
    })
}

fn invalid_plan(detail: impl Into<String>) -> RuntimeFailure {
    RuntimeFailure::InvalidResolvedPlan {
        detail: detail.into(),
    }
}

type ProviderChunks = LocalBoxStream<'static, Result<bytes::Bytes, reqwest::Error>>;

struct DirectCodexStream {
    chunks: Rc<futures::lock::Mutex<ProviderChunks>>,
    decoder: Rc<RefCell<ResponsesDecoder>>,
    events: Rc<RefCell<VecDeque<NativeStreamItem>>>,
    cancelled: Rc<Cell<bool>>,
    send_closed: Rc<Cell<bool>>,
}

impl fmt::Debug for DirectCodexStream {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DirectCodexStream")
            .field("cancelled", &self.cancelled.get())
            .field("send_closed", &self.send_closed.get())
            .finish_non_exhaustive()
    }
}

impl DirectCodexStream {
    fn new(
        chunks: ProviderChunks,
        max_event_bytes: usize,
        provider_to_lenso_tool_names: BTreeMap<String, String>,
    ) -> Self {
        Self {
            chunks: Rc::new(futures::lock::Mutex::new(chunks)),
            decoder: Rc::new(RefCell::new(ResponsesDecoder::new(
                max_event_bytes,
                provider_to_lenso_tool_names,
            ))),
            events: Rc::new(RefCell::new(VecDeque::new())),
            cancelled: Rc::new(Cell::new(false)),
            send_closed: Rc::new(Cell::new(false)),
        }
    }
}

impl NativeStreamSession for DirectCodexStream {
    fn send(&self, _message: Box<dyn Any>) -> LocalBoxFuture<'static, Result<(), RuntimeFailure>> {
        Box::pin(ready(Err(RuntimeFailure::ProtocolViolation {
            capability: CAPABILITY_ID,
        })))
    }

    fn receive(&self) -> LocalBoxFuture<'static, Result<NativeStreamItem, RuntimeFailure>> {
        let chunks = self.chunks.clone();
        let decoder = self.decoder.clone();
        let events = self.events.clone();
        let cancelled = self.cancelled.clone();
        Box::pin(async move {
            loop {
                if cancelled.get() {
                    return Err(RuntimeFailure::AdmissionClosed);
                }
                if let Some(event) = events.borrow_mut().pop_front() {
                    return Ok(event);
                }
                if decoder.borrow().terminal {
                    return Err(RuntimeFailure::ProtocolViolation {
                        capability: CAPABILITY_ID,
                    });
                }
                let chunk = chunks.lock().await.next().await;
                let output = match chunk {
                    Some(Ok(bytes)) => decoder.borrow_mut().push(&bytes)?,
                    Some(Err(_)) => return Err(protocol_failure("direct Codex stream failed")),
                    None => decoder.borrow_mut().finish()?,
                };
                events.borrow_mut().extend(output);
            }
        })
    }

    fn close_send(&self) -> LocalBoxFuture<'static, Result<(), RuntimeFailure>> {
        let result = if self.send_closed.replace(true) {
            Err(RuntimeFailure::ProtocolViolation {
                capability: CAPABILITY_ID,
            })
        } else {
            Ok(())
        };
        Box::pin(ready(result))
    }

    fn cancel(&self) {
        self.cancelled.set(true);
        self.events.borrow_mut().clear();
    }
}

#[derive(Debug)]
struct ResponsesDecoder {
    buffer: Vec<u8>,
    sequence: u64,
    terminal: bool,
    max_event_bytes: usize,
    provider_to_lenso_tool_names: BTreeMap<String, String>,
}

impl ResponsesDecoder {
    fn new(max_event_bytes: usize, provider_to_lenso_tool_names: BTreeMap<String, String>) -> Self {
        Self {
            buffer: Vec::new(),
            sequence: 0,
            terminal: false,
            max_event_bytes,
            provider_to_lenso_tool_names,
        }
    }

    fn push(&mut self, bytes: &[u8]) -> Result<Vec<NativeStreamItem>, RuntimeFailure> {
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() > self.max_event_bytes && frame_boundary(&self.buffer).is_none() {
            return Err(protocol_failure("direct Codex event exceeded its bound"));
        }
        let mut output = Vec::new();
        while let Some((end, delimiter)) = frame_boundary(&self.buffer) {
            let frame = self.buffer.drain(..end).collect::<Vec<_>>();
            self.buffer.drain(..delimiter);
            self.decode_frame(&frame, &mut output)?;
        }
        Ok(output)
    }

    fn finish(&mut self) -> Result<Vec<NativeStreamItem>, RuntimeFailure> {
        let mut output = Vec::new();
        if !self.buffer.iter().all(u8::is_ascii_whitespace) {
            let frame = std::mem::take(&mut self.buffer);
            self.decode_frame(&frame, &mut output)?;
        }
        if !self.terminal {
            return Err(protocol_failure(
                "direct Codex stream ended without response.completed",
            ));
        }
        Ok(output)
    }

    fn decode_frame(
        &mut self,
        frame: &[u8],
        output: &mut Vec<NativeStreamItem>,
    ) -> Result<(), RuntimeFailure> {
        if frame.len() > self.max_event_bytes {
            return Err(protocol_failure("direct Codex event exceeded its bound"));
        }
        let frame = std::str::from_utf8(frame)
            .map_err(|_| protocol_failure("direct Codex emitted non-UTF-8 SSE"))?;
        let data = frame
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() || data == "[DONE]" {
            return Ok(());
        }
        let event = serde_json::from_str::<serde_json::Value>(&data)
            .map_err(|_| protocol_failure("direct Codex emitted invalid SSE JSON"))?;
        self.decode_event(&event, output)
    }

    fn decode_event(
        &mut self,
        event: &serde_json::Value,
        output: &mut Vec<NativeStreamItem>,
    ) -> Result<(), RuntimeFailure> {
        if self.terminal {
            return Err(protocol_failure(
                "direct Codex emitted an event after completion",
            ));
        }
        match event.get("type").and_then(serde_json::Value::as_str) {
            Some("response.reasoning_summary_text.delta") => {
                self.emit_text_delta(CompleteMessageKind::ReasoningSummaryDelta, event, output);
            }
            Some("response.output_text.delta") => {
                self.emit_text_delta(CompleteMessageKind::TextDelta, event, output);
            }
            Some("response.output_item.done") => {
                let item = event
                    .get("item")
                    .ok_or_else(|| protocol_failure("direct Codex omitted output item"))?;
                if item.get("type").and_then(serde_json::Value::as_str) == Some("function_call") {
                    let call_id = string_field(item, "call_id")?;
                    let provider_name = string_field(item, "name")?;
                    let name = self
                        .provider_to_lenso_tool_names
                        .get(provider_name)
                        .ok_or_else(|| protocol_failure("direct Codex returned an unknown Tool"))?
                        .clone();
                    let arguments = string_field(item, "arguments")?;
                    serde_json::from_str::<serde_json::Value>(arguments).map_err(|_| {
                        protocol_failure("direct Codex emitted invalid Tool arguments")
                    })?;
                    output.push(self.message(
                        CompleteMessageKind::ToolCall,
                        "",
                        call_id,
                        &name,
                        arguments,
                        0,
                        0,
                    ));
                }
            }
            Some("response.completed" | "response.done") => {
                let usage = event
                    .get("response")
                    .and_then(|response| response.get("usage"));
                if let Some(usage) = usage {
                    output.push(
                        self.message(
                            CompleteMessageKind::Usage,
                            "",
                            "",
                            "",
                            "{}",
                            usage
                                .get("input_tokens")
                                .and_then(serde_json::Value::as_u64)
                                .unwrap_or(0),
                            usage
                                .get("output_tokens")
                                .and_then(serde_json::Value::as_u64)
                                .unwrap_or(0),
                        ),
                    );
                }
                self.terminal = true;
                output.push(NativeStreamItem::PeerHalfClosed);
                output.push(NativeStreamItem::Terminal(Ok(())));
            }
            Some("error" | "response.failed" | "response.incomplete") => {
                return Err(protocol_failure("direct Codex response failed"));
            }
            _ => {}
        }
        Ok(())
    }

    fn emit_text_delta(
        &mut self,
        kind: CompleteMessageKind,
        event: &serde_json::Value,
        output: &mut Vec<NativeStreamItem>,
    ) {
        if let Some(delta) = event.get("delta").and_then(serde_json::Value::as_str)
            && !delta.is_empty()
        {
            output.push(self.message(kind, delta, "", "", "{}", 0, 0));
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn message(
        &mut self,
        kind: CompleteMessageKind,
        text: &str,
        tool_call_id: &str,
        tool_name: &str,
        arguments_json: &str,
        input_tokens: u64,
        output_tokens: u64,
    ) -> NativeStreamItem {
        self.sequence = self.sequence.saturating_add(1);
        NativeStreamItem::Message(Box::new(CompleteMessage {
            sequence: self.sequence.to_string(),
            kind,
            text: text.to_owned(),
            tool_call_id: tool_call_id.to_owned(),
            tool_name: tool_name.to_owned(),
            arguments_json: arguments_json
                .to_owned()
                .try_into()
                .expect("provider Tool arguments must be valid JSON"),
            input_tokens: input_tokens.to_string(),
            output_tokens: output_tokens.to_string(),
        }))
    }
}

fn string_field<'a>(value: &'a serde_json::Value, field: &str) -> Result<&'a str, RuntimeFailure> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| protocol_failure("direct Codex output item is incomplete"))
}

fn frame_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    buffer
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|position| (position, 2))
        .or_else(|| {
            buffer
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|position| (position, 4))
        })
}

fn protocol_failure(detail: &str) -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: detail.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_request_uses_the_codex_compatibility_ceiling() {
        let config = DirectModelConfig {
            base_url: DEFAULT_BASE_URL.to_owned(),
            model: "gpt-current".to_owned(),
            allowed_models: None,
            include_models: None,
            exclude_models: Vec::new(),
            catalog_cache_path: None,
            catalog_max_stale_seconds: 0,
            catalog_snapshot_path: None,
            catalog_refresh_seconds: 0,
            reasoning_effort: "medium".to_owned(),
            max_event_bytes: MAX_EVENT_BYTES,
            transport: websocket::Transport::Sse,
        };

        assert_eq!(
            config.catalog_endpoint().unwrap().as_str(),
            "https://chatgpt.com/backend-api/codex/models?client_version=99.99.99"
        );
        assert_ne!(CODEX_CATALOG_CLIENT_VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn visibility_policy_keeps_the_primary_visible_and_separates_include_from_exclude() {
        let config = DirectModelConfig {
            base_url: DEFAULT_BASE_URL.to_owned(),
            model: "main-model".to_owned(),
            allowed_models: None,
            include_models: Some(vec!["presentation-model".to_owned()]),
            exclude_models: vec!["retired-model".to_owned()],
            catalog_cache_path: None,
            catalog_max_stale_seconds: 0,
            catalog_snapshot_path: None,
            catalog_refresh_seconds: 0,
            reasoning_effort: "medium".to_owned(),
            max_event_bytes: MAX_EVENT_BYTES,
            transport: websocket::Transport::Sse,
        }
        .validate()
        .unwrap();
        assert!(config.model_is_visible("main-model"));
        assert!(config.model_is_visible("presentation-model"));
        assert!(!config.model_is_visible("retired-model"));
        assert!(!config.model_is_visible("unreviewed-model"));
    }

    #[test]
    fn visibility_policy_rejects_overlap_and_hiding_the_primary() {
        let overlapping = DirectModelConfig {
            base_url: DEFAULT_BASE_URL.to_owned(),
            model: "main-model".to_owned(),
            allowed_models: None,
            include_models: Some(vec!["shared-model".to_owned()]),
            exclude_models: vec!["shared-model".to_owned()],
            catalog_cache_path: None,
            catalog_max_stale_seconds: 0,
            catalog_snapshot_path: None,
            catalog_refresh_seconds: 0,
            reasoning_effort: "medium".to_owned(),
            max_event_bytes: MAX_EVENT_BYTES,
            transport: websocket::Transport::Sse,
        };
        assert!(overlapping.validate().is_err());

        let primary_hidden = DirectModelConfig {
            base_url: DEFAULT_BASE_URL.to_owned(),
            model: "main-model".to_owned(),
            allowed_models: None,
            include_models: None,
            exclude_models: vec!["main-model".to_owned()],
            catalog_cache_path: None,
            catalog_max_stale_seconds: 0,
            catalog_snapshot_path: None,
            catalog_refresh_seconds: 0,
            reasoning_effort: "medium".to_owned(),
            max_event_bytes: MAX_EVENT_BYTES,
            transport: websocket::Transport::Sse,
        };
        assert!(primary_hidden.validate().is_err());
    }

    #[test]
    fn legacy_allowed_models_no_longer_filters_provider_facts() {
        let config = DirectModelConfig {
            base_url: DEFAULT_BASE_URL.to_owned(),
            model: "main-model".to_owned(),
            allowed_models: Some(vec!["legacy-auxiliary".to_owned()]),
            include_models: None,
            exclude_models: Vec::new(),
            catalog_cache_path: None,
            catalog_max_stale_seconds: 0,
            catalog_snapshot_path: None,
            catalog_refresh_seconds: 0,
            reasoning_effort: "medium".to_owned(),
            max_event_bytes: MAX_EVENT_BYTES,
            transport: websocket::Transport::Sse,
        }
        .validate()
        .unwrap();

        assert!(config.model_is_visible("new-provider-model"));
    }

    #[test]
    fn catalog_retains_discovered_models_and_projects_visibility_separately() {
        let config = DirectModelConfig {
            base_url: DEFAULT_BASE_URL.to_owned(),
            model: "main-model".to_owned(),
            allowed_models: Some(vec!["legacy-model".to_owned()]),
            include_models: Some(vec!["visible-model".to_owned()]),
            exclude_models: vec!["excluded-model".to_owned()],
            catalog_cache_path: None,
            catalog_max_stale_seconds: 0,
            catalog_snapshot_path: None,
            catalog_refresh_seconds: 0,
            reasoning_effort: "medium".to_owned(),
            max_event_bytes: MAX_EVENT_BYTES,
            transport: websocket::Transport::Sse,
        }
        .validate()
        .unwrap();
        let catalog = project_codex_catalog(
            &config,
            CodexModelsResponse {
                models: vec![
                    catalog_model("main-model", "list"),
                    catalog_model("visible-model", "list"),
                    catalog_model("excluded-model", "list"),
                    catalog_model("provider-hidden-model", "hide"),
                    catalog_model("provider-absent-model", "none"),
                ],
            },
            test_provenance(),
        )
        .unwrap();

        assert_eq!(
            catalog
                .models
                .iter()
                .map(|model| (model.id.as_str(), model.hidden))
                .collect::<Vec<_>>(),
            [
                ("excluded-model", true),
                ("main-model", false),
                ("provider-hidden-model", true),
                ("visible-model", false),
            ]
        );
    }

    #[test]
    fn catalog_revision_tracks_normalized_provider_content_only() {
        let config = DirectModelConfig {
            base_url: DEFAULT_BASE_URL.to_owned(),
            model: "main-model".to_owned(),
            allowed_models: None,
            include_models: None,
            exclude_models: Vec::new(),
            catalog_cache_path: None,
            catalog_max_stale_seconds: 0,
            catalog_snapshot_path: None,
            catalog_refresh_seconds: 0,
            reasoning_effort: "medium".to_owned(),
            max_event_bytes: MAX_EVENT_BYTES,
            transport: websocket::Transport::Sse,
        }
        .validate()
        .unwrap();
        let first = project_codex_catalog(
            &config,
            CodexModelsResponse {
                models: vec![
                    catalog_model("second-model", "list"),
                    catalog_model("main-model", "list"),
                ],
            },
            live_provenance(&config, 1, "etag-one"),
        )
        .unwrap();
        let equivalent = project_codex_catalog(
            &config,
            CodexModelsResponse {
                models: vec![
                    catalog_model("main-model", "list"),
                    catalog_model("second-model", "list"),
                ],
            },
            live_provenance(&config, 2, "etag-two"),
        )
        .unwrap();
        let changed = project_codex_catalog(
            &config,
            CodexModelsResponse {
                models: vec![catalog_model("main-model", "list")],
            },
            live_provenance(&config, 3, "etag-three"),
        )
        .unwrap();

        assert_eq!(
            projected_catalog_revision(&first).unwrap(),
            projected_catalog_revision(&equivalent).unwrap()
        );
        assert_ne!(
            projected_catalog_revision(&first).unwrap(),
            projected_catalog_revision(&changed).unwrap()
        );
        assert_ne!(first.provenance, equivalent.provenance);
    }

    fn catalog_model(slug: &str, visibility: &str) -> CodexModel {
        CodexModel {
            slug: slug.to_owned(),
            display_name: slug.to_owned(),
            description: None,
            default_reasoning_level: Some("medium".to_owned()),
            supported_reasoning_levels: vec![CodexReasoningEffort {
                effort: "medium".to_owned(),
                description: "Balanced".to_owned(),
            }],
            visibility: visibility.to_owned(),
            additional_speed_tiers: Vec::new(),
            service_tiers: Vec::new(),
            default_service_tier: None,
            supports_parallel_tool_calls: true,
            context_window: Some(272_000),
            max_context_window: None,
            effective_context_window_percent: 95,
            comp_hash: None,
            input_modalities: vec!["text".to_owned()],
        }
    }

    #[test]
    fn provider_catalog_owns_models_reasoning_tiers_and_limits() {
        let config = DirectModelConfig {
            base_url: DEFAULT_BASE_URL.to_owned(),
            model: "gpt-current".to_owned(),
            allowed_models: None,
            include_models: None,
            exclude_models: Vec::new(),
            catalog_cache_path: None,
            catalog_max_stale_seconds: 0,
            catalog_snapshot_path: None,
            catalog_refresh_seconds: 0,
            reasoning_effort: "high".to_owned(),
            max_event_bytes: MAX_EVENT_BYTES,
            transport: websocket::Transport::Sse,
        }
        .validate()
        .unwrap();
        let catalog = project_codex_catalog(
            &config,
            CodexModelsResponse {
                models: vec![CodexModel {
                    slug: "gpt-current".to_owned(),
                    display_name: "GPT Current".to_owned(),
                    description: Some("Provider-discovered model".to_owned()),
                    default_reasoning_level: Some("medium".to_owned()),
                    supported_reasoning_levels: vec![
                        CodexReasoningEffort {
                            effort: "medium".to_owned(),
                            description: "Balanced".to_owned(),
                        },
                        CodexReasoningEffort {
                            effort: "high".to_owned(),
                            description: "Deep".to_owned(),
                        },
                    ],
                    visibility: "list".to_owned(),
                    additional_speed_tiers: vec!["fast".to_owned()],
                    service_tiers: vec![CodexServiceTier {
                        id: "priority".to_owned(),
                        name: "Priority".to_owned(),
                        description: "Provider priority processing".to_owned(),
                    }],
                    default_service_tier: None,
                    supports_parallel_tool_calls: true,
                    context_window: Some(272_000),
                    max_context_window: None,
                    effective_context_window_percent: 95,
                    comp_hash: Some("codex-current".to_owned()),
                    input_modalities: vec!["text".to_owned(), "image".to_owned()],
                }],
            },
            test_provenance(),
        )
        .unwrap();
        let model = &catalog.models[0];
        assert_eq!(model.id, "gpt-current");
        assert_eq!(
            model.limits.context_window_tokens,
            Some(Some("272000".to_owned()))
        );
        assert_eq!(
            model.limits.max_input_tokens,
            Some(Some("258400".to_owned()))
        );
        assert_eq!(
            model
                .reasoning
                .options
                .iter()
                .map(|option| option.id.as_str())
                .collect::<Vec<_>>(),
            ["high", "medium"]
        );
        assert_eq!(
            model
                .service_tiers
                .options
                .iter()
                .map(|option| option.id.as_str())
                .collect::<Vec<_>>(),
            ["fast", "priority"]
        );
        assert_eq!(model.input_modalities, [CatalogInputModality::Text]);

        let duplicate = CatalogControlOption {
            id: "high".to_owned(),
            name: "High".to_owned(),
            description: String::new(),
        };
        assert!(control(None, vec![duplicate.clone(), duplicate], true).is_err());
    }

    fn test_provenance() -> CatalogProvenance {
        CatalogProvenance {
            source: CatalogSource::Live,
            freshness: CatalogFreshness::Fresh,
            fetched_at_unix_seconds: Some(Some("1".to_owned())),
            validated_at_unix_seconds: Some(Some("1".to_owned())),
            revision: Some(Some("test".to_owned())),
            max_stale_seconds: Some(Some("0".to_owned())),
        }
    }

    #[test]
    fn validated_cache_is_identity_bound_and_stale_use_is_bounded() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("catalog.json");
        let config = DirectModelConfig {
            base_url: DEFAULT_BASE_URL.to_owned(),
            model: "main-model".to_owned(),
            allowed_models: None,
            include_models: None,
            exclude_models: Vec::new(),
            catalog_cache_path: Some(path.clone()),
            catalog_max_stale_seconds: 60,
            catalog_snapshot_path: None,
            catalog_refresh_seconds: 0,
            reasoning_effort: "medium".to_owned(),
            max_event_bytes: MAX_EVENT_BYTES,
            transport: websocket::Transport::Sse,
        }
        .validate()
        .unwrap();
        let source_key = cache_source_key(&config.base_url, "account-a");
        let cached = CachedCatalog {
            schema: CACHE_SCHEMA.to_owned(),
            source_key: source_key.clone(),
            fetched_at_unix_seconds: 100,
            revision: "\"catalog-v1\"".to_owned(),
            etag: Some("\"catalog-v1\"".to_owned()),
            response: CodexModelsResponse {
                models: vec![catalog_model("main-model", "list")],
            },
        };
        write_cache(&path, &cached).unwrap();

        assert_eq!(
            read_cache(&path, &source_key, 160).unwrap(),
            Some(cached.clone())
        );
        assert!(read_cache(&path, "sha256:other", 160).is_err());
        let stale = stale_catalog(&config, Some(cached.clone()), 160, "request failed").unwrap();
        assert_eq!(stale.provenance.source, CatalogSource::Cache);
        assert_eq!(stale.provenance.freshness, CatalogFreshness::Stale);
        assert!(stale_catalog(&config, Some(cached), 161, "request failed").is_err());
    }
    use lenso_capability_agent_model::CompleteTool;

    #[test]
    fn request_preserves_tool_call_and_result() {
        let request = CompleteOpen {
            continuation_scope: None,
            model: "gpt-test".to_owned(),
            reasoning_effort: None,
            reasoning_enabled: None,
            reasoning_budget_tokens: None,
            service_tier: None,
            messages: vec![
                CompleteMessageInput {
                    role: CompleteMessageRole::Assistant,
                    content: String::new(),
                    tool_call_id: Some("call-1".to_owned()),
                    tool_name: Some("read".to_owned()),
                    arguments_json: Some(r#"{"path":"README.md"}"#.to_owned().try_into().unwrap()),
                },
                CompleteMessageInput {
                    role: CompleteMessageRole::Tool,
                    content: "fixture".to_owned(),
                    tool_call_id: Some("call-1".to_owned()),
                    tool_name: None,
                    arguments_json: None,
                },
            ],
            tools: vec![CompleteTool {
                name: "read".to_owned(),
                description: "Read text".to_owned(),
                input_schema_json: r#"{"type":"object"}"#.to_owned().try_into().unwrap(),
            }],
            temperature: 0.0,
            max_output_tokens: 128,
        };
        let wire_request = responses_request(&request, "medium").unwrap();
        let body = wire_request.body;
        assert_eq!(body["input"][0]["type"], "function_call");
        assert_eq!(body["input"][0]["name"], "read");
        assert_eq!(body["input"][1]["type"], "function_call_output");
        assert_eq!(body["tools"][0]["name"], "read");
        assert_eq!(body["parallel_tool_calls"], true);
        assert_eq!(body["reasoning"]["effort"], "medium");
        assert!(body.get("temperature").is_none());
        assert!(body.get("max_output_tokens").is_none());
        assert_eq!(
            wire_request.provider_to_lenso_tool_names.get("read"),
            Some(&"read".to_owned())
        );
    }

    #[test]
    fn request_rejects_provider_tool_name_collisions() {
        let request = CompleteOpen {
            continuation_scope: None,
            model: "gpt-test".to_owned(),
            reasoning_effort: None,
            reasoning_enabled: None,
            reasoning_budget_tokens: None,
            service_tier: None,
            messages: Vec::new(),
            tools: vec![
                CompleteTool {
                    name: "workspace.read".to_owned(),
                    description: "Read text".to_owned(),
                    input_schema_json: r#"{"type":"object"}"#.to_owned().try_into().unwrap(),
                },
                CompleteTool {
                    name: "workspace_read".to_owned(),
                    description: "Read other text".to_owned(),
                    input_schema_json: r#"{"type":"object"}"#.to_owned().try_into().unwrap(),
                },
            ],
            temperature: 0.0,
            max_output_tokens: 128,
        };
        assert!(matches!(
            responses_request(&request, "medium"),
            Err(CompleteError::InvalidRequest)
        ));
    }

    #[test]
    fn decoder_streams_reasoning_text_tool_call_and_usage() {
        let mut decoder = ResponsesDecoder::new(
            4096,
            BTreeMap::from([("read".to_owned(), "read".to_owned())]),
        );
        let frames = concat!(
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"Checking the workspace.\"}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n",
            "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"call_id\":\"call-1\",\"name\":\"read\",\"arguments\":\"{\\\"path\\\":\\\"README.md\\\"}\"}}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":3}}}\n\n"
        );
        let events = decoder.push(frames.as_bytes()).unwrap();
        assert_eq!(events.len(), 6);
        let first = match &events[0] {
            NativeStreamItem::Message(message) => message
                .downcast_ref::<CompleteMessage>()
                .expect("reasoning message must keep its generated type"),
            other => panic!("expected reasoning message, got {other:?}"),
        };
        assert_eq!(first.kind, CompleteMessageKind::ReasoningSummaryDelta);
        assert_eq!(first.text, "Checking the workspace.");
        assert!(decoder.terminal);
    }

    #[test]
    fn generation_snapshot_publication_deduplicates_facts_and_fences_old_publishers() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("effective/catalog.json");
        let first = ProviderCatalogSnapshot {
            schema: SNAPSHOT_SCHEMA.to_owned(),
            source_key: "account".to_owned(),
            fetched_at_unix_seconds: 1,
            revision: "one".to_owned(),
            response: CodexModelsResponse {
                models: vec![catalog_model("main-model", "list")],
            },
        };
        let first_publisher = claim_refresh_publisher(&path);
        assert!(publish_provider_snapshot(&path, &first, first_publisher).unwrap());

        let mut revalidated = first.clone();
        revalidated.fetched_at_unix_seconds = 2;
        assert!(!publish_provider_snapshot(&path, &revalidated, first_publisher).unwrap());
        let unchanged: ProviderCatalogSnapshot =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(unchanged.fetched_at_unix_seconds, 1);
        assert_eq!(unchanged.revision, "one");

        let second_publisher = claim_refresh_publisher(&path);
        let mut changed = revalidated;
        changed.revision = "two".to_owned();
        changed
            .response
            .models
            .push(catalog_model("new-model", "list"));
        assert!(!publish_provider_snapshot(&path, &changed, first_publisher).unwrap());
        assert!(publish_provider_snapshot(&path, &changed, second_publisher).unwrap());
        let published: ProviderCatalogSnapshot =
            serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(published.response, changed.response);
    }
}
