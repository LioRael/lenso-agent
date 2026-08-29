use std::{fmt, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use lenso_app_authoring::{
    PluginConfigurationApplication, PluginConfigurationAuthority, PluginConfigurationProposal,
    PluginConfigurationProposalStatus, PluginRootRevision,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::limit::ConcurrencyLimitLayer;

use crate::{PluginConfigurationPublicationRecord, SqlitePluginConfigurationAuthority};

pub const CONFIGURATION_SERVICE_READ_TOKEN_ENV: &str =
    "LENSO_PLUGIN_CONFIGURATION_SERVICE_READ_TOKEN";
pub const CONFIGURATION_SERVICE_WRITE_TOKEN_ENV: &str =
    "LENSO_PLUGIN_CONFIGURATION_SERVICE_WRITE_TOKEN";

const MAX_CHANGE_BATCH: usize = 64;
const MAX_CONCURRENT_REQUESTS: usize = 128;
const MAX_HISTORY_LIMIT: usize = 50;
const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_WAIT: Duration = Duration::from_secs(30);
const WATCH_POLL_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PluginConfigurationServiceResource {
    app: String,
    environment: String,
}

impl PluginConfigurationServiceResource {
    pub fn new(app: impl Into<String>, environment: impl Into<String>) -> anyhow::Result<Self> {
        Ok(Self {
            app: validate_resource_segment("App", app.into())?,
            environment: validate_resource_segment("environment", environment.into())?,
        })
    }

    pub fn reference(&self) -> String {
        format!("{}/{}", self.app, self.environment)
    }
}

#[derive(Clone)]
pub struct PluginConfigurationServiceAccess {
    read: TokenDigest,
    write: TokenDigest,
}

impl PluginConfigurationServiceAccess {
    pub fn new(read_token: impl AsRef<str>, write_token: impl AsRef<str>) -> anyhow::Result<Self> {
        let read_token = validate_token("read", read_token.as_ref())?;
        let write_token = validate_token("write", write_token.as_ref())?;
        if constant_time_eq(read_token.as_bytes(), write_token.as_bytes()) {
            anyhow::bail!("remote Plugin configuration read and write tokens must be distinct");
        }
        Ok(Self {
            read: TokenDigest::new(read_token),
            write: TokenDigest::new(write_token),
        })
    }
}

impl fmt::Debug for PluginConfigurationServiceAccess {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PluginConfigurationServiceAccess")
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
struct TokenDigest([u8; 32]);

impl TokenDigest {
    fn new(token: &str) -> Self {
        Self(Sha256::digest(token.as_bytes()).into())
    }

    fn matches(&self, token: &str) -> bool {
        let candidate: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        constant_time_eq(&self.0, &candidate)
    }
}

#[derive(Clone)]
struct ServiceState {
    access: PluginConfigurationServiceAccess,
    authority: Arc<SqlitePluginConfigurationAuthority>,
    resource: PluginConfigurationServiceResource,
}

/// One durable, explicitly identified remote Plugin configuration resource.
#[derive(Clone)]
pub struct PluginConfigurationService {
    state: ServiceState,
}

impl fmt::Debug for PluginConfigurationService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PluginConfigurationService")
            .field("resource", &self.state.resource)
            .finish_non_exhaustive()
    }
}

impl PluginConfigurationService {
    pub fn new(
        authority: SqlitePluginConfigurationAuthority,
        resource: PluginConfigurationServiceResource,
        access: PluginConfigurationServiceAccess,
    ) -> Self {
        Self {
            state: ServiceState {
                access,
                authority: Arc::new(authority),
                resource,
            },
        }
    }

    pub fn router(&self) -> Router {
        Router::new()
            .route(
                "/v1/apps/{app}/environments/{environment}/plugins",
                get(inspect),
            )
            .route(
                "/v1/apps/{app}/environments/{environment}/changes",
                get(changes),
            )
            .route(
                "/v1/apps/{app}/environments/{environment}/plugins/{plugin}/{instance}/configuration",
                put(publish),
            )
            .route(
                "/v1/apps/{app}/environments/{environment}/plugins/{plugin}/{instance}/configuration/proposals",
                post(propose),
            )
            .route(
                "/v1/apps/{app}/environments/{environment}/plugins/{plugin}/{instance}/configuration/publications",
                get(history),
            )
            .route(
                "/v1/apps/{app}/environments/{environment}/plugins/{plugin}/{instance}/configuration/rollback-proposals",
                post(rollback),
            )
            .layer(DefaultBodyLimit::max(MAX_REQUEST_BYTES))
            .layer(ConcurrencyLimitLayer::new(MAX_CONCURRENT_REQUESTS))
            .with_state(self.state.clone())
    }

    #[cfg(test)]
    pub(crate) fn authority(&self) -> &SqlitePluginConfigurationAuthority {
        &self.state.authority
    }
}

#[derive(Clone, Copy)]
enum RequiredAccess {
    Read,
    Write,
}

#[derive(Debug, Serialize)]
struct Problem {
    detail: String,
}

#[derive(Debug)]
struct ServiceProblem {
    detail: String,
    status: StatusCode,
}

impl IntoResponse for ServiceProblem {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(Problem {
                detail: self.detail,
            }),
        )
            .into_response()
    }
}

type ServiceReply = Result<Json<Value>, ServiceProblem>;

async fn inspect(
    State(state): State<ServiceState>,
    Path((app, environment)): Path<(String, String)>,
    headers: HeaderMap,
) -> ServiceReply {
    authorize(&state.access, &headers, RequiredAccess::Read)?;
    require_identity(&state.resource, &app, &environment)?;
    let authority = Arc::clone(&state.authority);
    let current = blocking_operation(move || authority.inspect()).await?;
    bounded_json(json!({
        "revision": current.revision().as_str(),
        "schema": "lenso.configuration.plugin-management.v1",
    }))
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChangesQuery {
    after_cursor: Option<String>,
    after_revision: String,
    limit: usize,
    wait_ms: u64,
}

async fn changes(
    State(state): State<ServiceState>,
    Path((app, environment)): Path<(String, String)>,
    Query(query): Query<ChangesQuery>,
    headers: HeaderMap,
) -> ServiceReply {
    authorize(&state.access, &headers, RequiredAccess::Read)?;
    require_identity(&state.resource, &app, &environment)?;
    validate_changes_query(&query)?;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(query.wait_ms);
    loop {
        let authority = Arc::clone(&state.authority);
        let request = query.clone();
        let batch = blocking_operation(move || build_change_batch(&authority, &request)).await?;
        if batch.changed || query.wait_ms == 0 || tokio::time::Instant::now() >= deadline {
            return bounded_json(batch.body);
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        tokio::time::sleep(WATCH_POLL_INTERVAL.min(remaining)).await;
    }
}

struct ChangeBatch {
    body: Value,
    changed: bool,
}

fn build_change_batch(
    authority: &SqlitePluginConfigurationAuthority,
    query: &ChangesQuery,
) -> anyhow::Result<ChangeBatch> {
    let batch = authority.publication_changes(
        &query.after_revision,
        query.after_cursor.as_deref(),
        query.limit,
    )?;
    let mut selected = batch.publications.iter().collect::<Vec<_>>();
    let changed = !selected.is_empty();
    loop {
        let revision = selected
            .last()
            .map_or(batch.desired_revision.as_str(), |record| {
                record.revision.as_str()
            });
        let cursor = selected
            .last()
            .map_or(batch.head_cursor.as_str(), |record| {
                record.proposal_digest.as_str()
            });
        let body = changes_json(&query.after_revision, revision, cursor, &selected);
        if response_fits(&body) {
            return Ok(ChangeBatch {
                body,
                changed: !selected.is_empty(),
            });
        }
        selected.pop();
        if changed && selected.is_empty() {
            anyhow::bail!("one Plugin configuration change exceeds the response limit");
        }
    }
}

fn changes_json(
    base_revision: &str,
    revision: &str,
    cursor: &str,
    changes: &[&PluginConfigurationPublicationRecord],
) -> Value {
    json!({
        "baseRevision": base_revision,
        "changes": changes.iter().map(|record| json!({
            "baseRevision": record.base_revision,
            "configurationToml": record.configuration_toml,
            "instanceKey": record.instance_key,
            "pluginId": record.plugin_id,
            "proposalDigest": record.proposal_digest,
            "revision": record.revision,
        })).collect::<Vec<_>>(),
        "cursor": cursor,
        "revision": revision,
        "schema": "lenso.configuration.plugin-changes.v1",
    })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProposalBody {
    expected_revision: String,
    toml: String,
}

async fn propose(
    State(state): State<ServiceState>,
    Path((app, environment, plugin, instance)): Path<(String, String, String, String)>,
    headers: HeaderMap,
    Json(body): Json<ProposalBody>,
) -> ServiceReply {
    authorize(&state.access, &headers, RequiredAccess::Write)?;
    require_identity(&state.resource, &app, &environment)?;
    let revision = parse_revision(&body.expected_revision)?;
    let authority = Arc::clone(&state.authority);
    let proposal = blocking_operation(move || {
        authority.propose(&revision, &plugin, &instance, body.toml.as_bytes())
    })
    .await?;
    bounded_json(proposal_json(&proposal))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublicationBody {
    expected_revision: String,
    proposal_digest: String,
    rollback_of_proposal_digest: Option<String>,
    toml: String,
}

async fn publish(
    State(state): State<ServiceState>,
    Path((app, environment, plugin, instance)): Path<(String, String, String, String)>,
    headers: HeaderMap,
    Json(body): Json<PublicationBody>,
) -> ServiceReply {
    authorize(&state.access, &headers, RequiredAccess::Write)?;
    require_identity(&state.resource, &app, &environment)?;
    let revision = parse_revision(&body.expected_revision)?;
    let authority = Arc::clone(&state.authority);
    let publication = blocking_operation(move || {
        let proposal = if let Some(rollback_of) = body.rollback_of_proposal_digest.as_deref() {
            let Some((proposal, published_toml)) =
                authority.propose_rollback(&revision, &plugin, &instance, rollback_of)?
            else {
                anyhow::bail!("rollback publication not found");
            };
            if published_toml != body.toml {
                anyhow::bail!("rollback configuration mismatch");
            }
            proposal
        } else {
            authority.propose(&revision, &plugin, &instance, body.toml.as_bytes())?
        };
        if proposal.digest() != body.proposal_digest {
            anyhow::bail!("proposal digest mismatch");
        }
        authority.publish(&proposal)
    })
    .await?;
    bounded_json(json!({
        "baseRevision": publication.base_revision().as_str(),
        "proposalDigest": publication.proposal_digest(),
        "revision": publication.revision().as_str(),
        "schema": publication.schema(),
        "status": "published",
    }))
}

#[derive(Debug, Deserialize)]
struct HistoryQuery {
    limit: Option<usize>,
}

async fn history(
    State(state): State<ServiceState>,
    Path((app, environment, plugin, instance)): Path<(String, String, String, String)>,
    Query(query): Query<HistoryQuery>,
    headers: HeaderMap,
) -> ServiceReply {
    authorize(&state.access, &headers, RequiredAccess::Read)?;
    require_identity(&state.resource, &app, &environment)?;
    let limit = query.limit.unwrap_or(20);
    if !(1..=MAX_HISTORY_LIMIT).contains(&limit) {
        return Err(bad_request("history limit is out of bounds"));
    }
    let requested_plugin = plugin.clone();
    let requested_instance = instance.clone();
    let authority = Arc::clone(&state.authority);
    let mut publications = blocking_operation(move || {
        authority.publications(&requested_plugin, &requested_instance, limit)
    })
    .await?;
    loop {
        let body = json!({
            "instanceKey": instance,
            "pluginId": plugin,
            "publications": publications.iter().map(|record| json!({
                "baseRevision": record.base_revision,
                "baseSourceDigest": record.base_source_digest,
                "configurationToml": record.configuration_toml,
                "proposalDigest": record.proposal_digest,
                "publishedAtUnixMs": record.published_at_unix_ms,
                "revision": record.revision,
                "rollbackOfProposalDigest": record.rollback_of_proposal_digest,
            })).collect::<Vec<_>>(),
            "schema": "lenso.configuration.plugin-history.v1",
        });
        if response_fits(&body) {
            return Ok(Json(body));
        }
        publications.pop();
        if publications.is_empty() {
            return Err(internal_problem());
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RollbackBody {
    expected_revision: String,
    publication_proposal_digest: String,
}

async fn rollback(
    State(state): State<ServiceState>,
    Path((app, environment, plugin, instance)): Path<(String, String, String, String)>,
    headers: HeaderMap,
    Json(body): Json<RollbackBody>,
) -> ServiceReply {
    authorize(&state.access, &headers, RequiredAccess::Write)?;
    require_identity(&state.resource, &app, &environment)?;
    let revision = parse_revision(&body.expected_revision)?;
    let rollback_of = body.publication_proposal_digest;
    let rollback_source = rollback_of.clone();
    let authority = Arc::clone(&state.authority);
    let proposed = blocking_operation(move || {
        authority.propose_rollback(&revision, &plugin, &instance, &rollback_source)
    })
    .await?;
    let Some((proposal, toml)) = proposed else {
        return Err(ServiceProblem {
            detail: "publication not found".to_owned(),
            status: StatusCode::NOT_FOUND,
        });
    };
    bounded_json(json!({
        "configurationToml": toml,
        "proposal": proposal_json(&proposal),
        "rollbackOfProposalDigest": rollback_of,
        "schema": "lenso.configuration.plugin-rollback-proposal.v1",
    }))
}

fn proposal_json(proposal: &PluginConfigurationProposal) -> Value {
    json!({
        "application": proposal_application(proposal.application()),
        "baseRevision": proposal.base_revision().as_str(),
        "candidateRevision": proposal.candidate_revision().as_str(),
        "diagnostics": proposal.diagnostics().iter().map(|item| json!({
            "code": item.code(),
            "detail": item.detail(),
        })).collect::<Vec<_>>(),
        "instanceKey": proposal.instance_key(),
        "pluginId": proposal.plugin_id(),
        "proposalDigest": proposal.digest(),
        "schema": proposal.schema(),
        "status": proposal_status(proposal.status()),
    })
}

fn proposal_status(status: PluginConfigurationProposalStatus) -> &'static str {
    match status {
        PluginConfigurationProposalStatus::Ready => "ready",
        PluginConfigurationProposalStatus::NeedsDecision => "needs_decision",
        PluginConfigurationProposalStatus::Rejected => "rejected",
    }
}

fn proposal_application(application: PluginConfigurationApplication) -> &'static str {
    match application {
        PluginConfigurationApplication::Noop => "noop",
        PluginConfigurationApplication::AppGeneration => "app_generation",
        PluginConfigurationApplication::Blocked => "blocked",
    }
}

fn bounded_json(body: Value) -> ServiceReply {
    if response_fits(&body) {
        Ok(Json(body))
    } else {
        Err(internal_problem())
    }
}

fn response_fits(body: &Value) -> bool {
    serde_json::to_vec(body).is_ok_and(|bytes| bytes.len() <= MAX_RESPONSE_BYTES)
}

async fn blocking_operation<T: Send + 'static>(
    operation: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> Result<T, ServiceProblem> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| internal_problem())?
        .map_err(|error| operation_problem(&error))
}

fn authorize(
    access: &PluginConfigurationServiceAccess,
    headers: &HeaderMap,
    required: RequiredAccess,
) -> Result<(), ServiceProblem> {
    let token = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or_else(unauthorized)?;
    if access.write.matches(token) {
        return Ok(());
    }
    if access.read.matches(token) {
        return match required {
            RequiredAccess::Read => Ok(()),
            RequiredAccess::Write => Err(ServiceProblem {
                detail: "write access is required".to_owned(),
                status: StatusCode::FORBIDDEN,
            }),
        };
    }
    Err(unauthorized())
}

fn require_identity(
    resource: &PluginConfigurationServiceResource,
    app: &str,
    environment: &str,
) -> Result<(), ServiceProblem> {
    if resource.app == app && resource.environment == environment {
        Ok(())
    } else {
        Err(ServiceProblem {
            detail: "resource not found".to_owned(),
            status: StatusCode::NOT_FOUND,
        })
    }
}

fn validate_changes_query(query: &ChangesQuery) -> Result<(), ServiceProblem> {
    if query.limit == 0
        || query.limit > MAX_CHANGE_BATCH
        || Duration::from_millis(query.wait_ms) > MAX_WAIT
        || query.after_revision.parse::<PluginRootRevision>().is_err()
        || query.after_cursor.as_ref().is_some_and(|cursor| {
            cursor.is_empty() || cursor.len() > 256 || cursor.chars().any(char::is_control)
        })
    {
        return Err(bad_request("invalid change query"));
    }
    Ok(())
}

fn parse_revision(value: &str) -> Result<PluginRootRevision, ServiceProblem> {
    value
        .parse()
        .map_err(|_| bad_request("invalid Plugin Root revision"))
}

fn operation_problem(error: &anyhow::Error) -> ServiceProblem {
    let detail = error.to_string();
    if detail.contains("revision conflict")
        || detail.contains("proposal digest mismatch")
        || detail.contains("rollback publication not found")
        || detail.contains("rollback configuration mismatch")
        || detail.contains("revision history gap")
        || detail.contains("change cursor is unavailable")
        || detail.contains("cursor and materialized revision do not align")
    {
        ServiceProblem {
            detail,
            status: StatusCode::CONFLICT,
        }
    } else {
        internal_problem()
    }
}

fn validate_resource_segment(label: &str, value: String) -> anyhow::Result<String> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        anyhow::bail!("remote Plugin configuration {label} identity is invalid");
    }
    Ok(value)
}

fn validate_token<'a>(label: &str, token: &'a str) -> anyhow::Result<&'a str> {
    if token.len() < 16 || token.len() > 4_096 || token.chars().any(char::is_control) {
        anyhow::bail!("remote Plugin configuration {label} token is invalid");
    }
    Ok(token)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn unauthorized() -> ServiceProblem {
    ServiceProblem {
        detail: "unauthorized".to_owned(),
        status: StatusCode::UNAUTHORIZED,
    }
}

fn bad_request(detail: &str) -> ServiceProblem {
    ServiceProblem {
        detail: detail.to_owned(),
        status: StatusCode::BAD_REQUEST,
    }
}

fn internal_problem() -> ServiceProblem {
    ServiceProblem {
        detail: "Plugin configuration service operation failed".to_owned(),
        status: StatusCode::INTERNAL_SERVER_ERROR,
    }
}
