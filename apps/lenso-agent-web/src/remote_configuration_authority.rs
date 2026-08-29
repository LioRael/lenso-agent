use std::{
    collections::BTreeMap,
    fmt,
    io::Read,
    net::IpAddr,
    path::PathBuf,
    str::FromStr,
    sync::{Mutex, mpsc},
    thread::{self, JoinHandle},
    time::Duration,
};

use anyhow::{Context, bail};
use lenso_app_authoring::{
    LocalPluginRootAuthority, PluginConfigurationAuthority, PluginConfigurationAuthoritySource,
    PluginConfigurationProposal, PluginConfigurationPublication, PluginRootAuthoringState,
    PluginRootRevision,
};
use reqwest::{Method, StatusCode, Url, blocking::Client};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{PluginConfigurationHistoryAuthority, PluginConfigurationPublicationRecord};

const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_RETAINED_PROPOSALS: usize = 64;
const MAX_SYNC_CHANGES: usize = 64;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

struct RemoteHttpResponse {
    status: StatusCode,
    body: Vec<u8>,
}

enum RemoteHttpCommand {
    Request {
        body: Option<Vec<u8>>,
        method: Method,
        reply: mpsc::SyncSender<Result<RemoteHttpResponse, String>>,
        url: Url,
    },
    Shutdown,
}

struct RemoteHttpClient {
    commands: mpsc::SyncSender<RemoteHttpCommand>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl fmt::Debug for RemoteHttpClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteHttpClient")
            .finish_non_exhaustive()
    }
}

impl RemoteHttpClient {
    fn start(timeout: Duration, token: String) -> anyhow::Result<Self> {
        let (commands, receiver) = mpsc::sync_channel(16);
        let (ready, readiness) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("lenso-remote-configuration-http".to_owned())
            .spawn(move || {
                let client = Client::builder()
                    .timeout(timeout)
                    .redirect(reqwest::redirect::Policy::none())
                    .build();
                let client = match client {
                    Ok(client) => client,
                    Err(error) => {
                        let _ = ready.send(Err(format!(
                            "build remote Plugin configuration HTTP client: {error}"
                        )));
                        return;
                    }
                };
                let _ = ready.send(Ok(()));
                while let Ok(command) = receiver.recv() {
                    match command {
                        RemoteHttpCommand::Request {
                            body,
                            method,
                            reply,
                            url,
                        } => {
                            let mut request = client
                                .request(method, url)
                                .bearer_auth(&token)
                                .header("accept", "application/json");
                            if let Some(body) = body {
                                request = request
                                    .header("content-type", "application/json")
                                    .body(body);
                            }
                            let result = request
                                .send()
                                .map_err(|error| error.to_string())
                                .and_then(|response| {
                                    let status = response.status();
                                    let mut body = Vec::new();
                                    response
                                        .take((MAX_RESPONSE_BYTES + 1) as u64)
                                        .read_to_end(&mut body)
                                        .map_err(|error| error.to_string())?;
                                    Ok(RemoteHttpResponse { status, body })
                                });
                            let _ = reply.send(result);
                        }
                        RemoteHttpCommand::Shutdown => break,
                    }
                }
            })
            .context("start remote Plugin configuration HTTP thread")?;
        readiness
            .recv()
            .context("remote Plugin configuration HTTP thread stopped before readiness")?
            .map_err(anyhow::Error::msg)?;
        Ok(Self {
            commands,
            thread: Mutex::new(Some(thread)),
        })
    }

    fn request(
        &self,
        method: Method,
        url: Url,
        body: Option<Vec<u8>>,
    ) -> anyhow::Result<RemoteHttpResponse> {
        let (reply, response) = mpsc::sync_channel(1);
        self.commands
            .send(RemoteHttpCommand::Request {
                body,
                method,
                reply,
                url,
            })
            .map_err(|_| anyhow::anyhow!("remote Plugin configuration HTTP thread stopped"))?;
        response
            .recv()
            .map_err(|_| anyhow::anyhow!("remote Plugin configuration HTTP response was lost"))?
            .map_err(anyhow::Error::msg)
    }
}

impl Drop for RemoteHttpClient {
    fn drop(&mut self) {
        let _ = self.commands.send(RemoteHttpCommand::Shutdown);
        if let Ok(thread) = self.thread.get_mut()
            && let Some(thread) = thread.take()
        {
            let _ = thread.join();
        }
    }
}

/// Identity of one remotely managed App configuration resource.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RemotePluginConfigurationResource {
    service_url: Url,
    app: String,
    environment: String,
}

impl RemotePluginConfigurationResource {
    pub fn new(
        service_url: impl AsRef<str>,
        app: impl Into<String>,
        environment: impl Into<String>,
    ) -> anyhow::Result<Self> {
        let mut service_url = Url::parse(service_url.as_ref())
            .context("parse remote Plugin configuration service URL")?;
        validate_service_url(&service_url)?;
        let app = validate_resource_segment("App", app.into())?;
        let environment = validate_resource_segment("environment", environment.into())?;
        if !service_url.path().ends_with('/') {
            let path = format!("{}/", service_url.path());
            service_url.set_path(&path);
        }
        Ok(Self {
            service_url,
            app,
            environment,
        })
    }

    pub fn app(&self) -> &str {
        &self.app
    }

    pub fn environment(&self) -> &str {
        &self.environment
    }

    pub fn service_url(&self) -> &Url {
        &self.service_url
    }

    fn resource_url(&self) -> anyhow::Result<Url> {
        let mut url = self.service_url.clone();
        url.path_segments_mut()
            .map_err(|()| anyhow::anyhow!("remote configuration service URL cannot be a base"))?
            .pop_if_empty()
            .extend(["v1", "apps", &self.app, "environments", &self.environment]);
        Ok(url)
    }
}

/// Host-owned connection settings for one remote Plugin configuration authority.
#[derive(Clone)]
pub struct RemotePluginConfigurationConfig {
    pub resource: RemotePluginConfigurationResource,
    bearer_token: String,
    timeout: Duration,
}

impl RemotePluginConfigurationConfig {
    pub fn new(
        resource: RemotePluginConfigurationResource,
        bearer_token: impl Into<String>,
    ) -> anyhow::Result<Self> {
        let bearer_token = bearer_token.into();
        if bearer_token.is_empty() || bearer_token.chars().any(char::is_control) {
            bail!("remote Plugin configuration bearer token is invalid");
        }
        Ok(Self {
            resource,
            bearer_token,
            timeout: DEFAULT_TIMEOUT,
        })
    }

    pub fn with_timeout(mut self, timeout: Duration) -> anyhow::Result<Self> {
        if timeout.is_zero() || timeout > Duration::from_secs(60) {
            bail!("remote Plugin configuration timeout must be between 1ms and 60s");
        }
        self.timeout = timeout;
        Ok(self)
    }
}

impl fmt::Debug for RemotePluginConfigurationConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemotePluginConfigurationConfig")
            .field("resource", &self.resource)
            .field("bearer_token", &"[REDACTED]")
            .field("timeout", &self.timeout)
            .finish()
    }
}

/// HTTP adapter for one revision-fenced remote Plugin configuration resource.
///
/// The remote service owns desired state and CAS. This adapter keeps the Host's
/// visible Plugin Root as an exact materialized mirror; it never accepts a
/// remote Plan and never falls back to local publication.
pub struct RemotePluginConfigurationAuthority {
    http: RemoteHttpClient,
    resource_url: Url,
    local: LocalPluginRootAuthority,
    source: PluginConfigurationAuthoritySource,
    proposals: Mutex<BTreeMap<String, RetainedRemoteProposal>>,
    access: Mutex<()>,
    sync_cursor: Mutex<Option<String>>,
}

#[derive(Clone)]
struct RetainedRemoteProposal {
    bytes: Vec<u8>,
    remote_digest: String,
    rollback_of_proposal_digest: Option<String>,
}

impl fmt::Debug for RemotePluginConfigurationAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemotePluginConfigurationAuthority")
            .field("resource_url", &self.resource_url)
            .finish_non_exhaustive()
    }
}

impl RemotePluginConfigurationAuthority {
    pub fn connect(
        root: impl Into<PathBuf>,
        config: RemotePluginConfigurationConfig,
    ) -> anyhow::Result<Self> {
        let resource_url = config.resource.resource_url()?;
        let reference = resource_url.as_str().trim_end_matches('/');
        let source =
            PluginConfigurationAuthoritySource::new("remote_configuration_service", reference)?;
        let http = RemoteHttpClient::start(config.timeout, config.bearer_token)?;
        let authority = Self {
            http,
            resource_url,
            local: LocalPluginRootAuthority::new(root),
            source,
            proposals: Mutex::new(BTreeMap::new()),
            access: Mutex::new(()),
            sync_cursor: Mutex::new(None),
        };
        authority.synchronize(Duration::ZERO)?;
        Ok(authority)
    }

    fn endpoint(&self, segments: &[&str]) -> anyhow::Result<Url> {
        let mut url = self.resource_url.clone();
        url.path_segments_mut()
            .map_err(|()| anyhow::anyhow!("remote configuration resource URL cannot be a base"))?
            .pop_if_empty()
            .extend(segments);
        Ok(url)
    }

    fn plugin_endpoint(
        &self,
        plugin_id: &str,
        instance: &str,
        suffix: &[&str],
    ) -> anyhow::Result<Url> {
        let mut segments = vec!["plugins", plugin_id, instance, "configuration"];
        segments.extend_from_slice(suffix);
        self.endpoint(&segments)
    }

    fn send<T: DeserializeOwned>(
        &self,
        method: Method,
        url: Url,
        body: Option<Vec<u8>>,
        operation: &str,
    ) -> anyhow::Result<T> {
        let response = self
            .http
            .request(method, url, body)
            .with_context(|| format!("remote Plugin configuration {operation} request failed"))?;
        let status = response.status;
        let bytes = response.body;
        if bytes.len() > MAX_RESPONSE_BYTES {
            bail!("remote Plugin configuration {operation} response exceeded 2 MiB");
        }
        if !status.is_success() {
            let detail = serde_json::from_slice::<RemoteProblem>(&bytes).map_or_else(
                |_| {
                    status
                        .canonical_reason()
                        .unwrap_or("request failed")
                        .to_owned()
                },
                |problem| problem.detail,
            );
            bail!("remote Plugin configuration {operation} failed with {status}: {detail}");
        }
        serde_json::from_slice(&bytes)
            .with_context(|| format!("decode remote Plugin configuration {operation} response"))
    }

    fn compare_proposal(
        local: &PluginConfigurationProposal,
        remote: &RemoteProposalResponse,
    ) -> anyhow::Result<()> {
        ensure_schema(&remote.schema, local.schema())?;
        validate_remote_proposal_evidence(remote)?;
        if remote.base_revision != local.base_revision().as_str()
            || remote.candidate_revision != local.candidate_revision().as_str()
            || remote.plugin_id != local.plugin_id()
            || remote.instance_key != local.instance_key()
        {
            bail!(
                "remote Plugin configuration proposal identity or semantic revision does not match the Host-reviewed candidate"
            );
        }
        Ok(())
    }

    fn remember_proposal(
        &self,
        proposal: &PluginConfigurationProposal,
        bytes: &[u8],
        remote_digest: &str,
        rollback_of_proposal_digest: Option<&str>,
    ) -> anyhow::Result<()> {
        let mut proposals = self.proposals.lock().map_err(|_| {
            anyhow::anyhow!("remote Plugin configuration proposal lock is poisoned")
        })?;
        let retained = RetainedRemoteProposal {
            bytes: bytes.to_vec(),
            remote_digest: remote_digest.to_owned(),
            rollback_of_proposal_digest: rollback_of_proposal_digest.map(str::to_owned),
        };
        if let Some(existing) = proposals.get(proposal.digest()) {
            if existing.bytes != retained.bytes
                || existing.remote_digest != retained.remote_digest
                || existing.rollback_of_proposal_digest != retained.rollback_of_proposal_digest
            {
                bail!("remote Plugin configuration proposal evidence conflicts with its digest");
            }
            return Ok(());
        }
        if proposals.len() >= MAX_RETAINED_PROPOSALS {
            bail!("remote Plugin configuration proposal cache is full");
        }
        proposals.insert(proposal.digest().to_owned(), retained);
        Ok(())
    }

    /// Materializes one complete, ordered remote revision transition batch.
    ///
    /// Each transition remains one exact Plugin Instance configuration
    /// publication. A missing or reordered transition fails closed rather than
    /// replacing unrelated Plugin Root authority.
    pub(crate) fn synchronize(&self, wait: Duration) -> anyhow::Result<bool> {
        let _guard = self
            .access
            .lock()
            .map_err(|_| anyhow::anyhow!("remote Plugin configuration access lock is poisoned"))?;
        self.synchronize_locked(wait)
    }

    fn synchronize_locked(&self, wait: Duration) -> anyhow::Result<bool> {
        let mut current = self.local.inspect()?.revision().clone();
        let mut url = self.endpoint(&["changes"])?;
        {
            let cursor = self
                .sync_cursor
                .lock()
                .map_err(|_| {
                    anyhow::anyhow!("remote Plugin configuration cursor lock is poisoned")
                })?
                .clone();
            let mut query = url.query_pairs_mut();
            query
                .append_pair("afterRevision", current.as_str())
                .append_pair("limit", &MAX_SYNC_CHANGES.to_string())
                .append_pair(
                    "waitMs",
                    &u64::try_from(wait.as_millis())
                        .unwrap_or(u64::MAX)
                        .to_string(),
                );
            if let Some(cursor) = cursor.as_deref() {
                query.append_pair("afterCursor", cursor);
            }
        }
        let reply: RemoteChangesResponse =
            self.send(Method::GET, url, None, "desired change synchronization")?;
        ensure_schema(&reply.schema, "lenso.configuration.plugin-changes.v1")?;
        if reply.base_revision != current.as_str() {
            bail!(
                "remote Plugin configuration change batch does not start at the materialized revision"
            );
        }
        if reply.changes.len() > MAX_SYNC_CHANGES {
            bail!("remote Plugin configuration change batch exceeds the requested limit");
        }
        validate_cursor(&reply.cursor)?;
        let changed = !reply.changes.is_empty();
        for change in reply.changes {
            if change.base_revision != current.as_str() {
                bail!("remote Plugin configuration change chain is discontinuous");
            }
            let expected_revision = PluginRootRevision::from_str(&change.base_revision)?;
            let proposal = self.local.propose(
                &expected_revision,
                &change.plugin_id,
                &change.instance_key,
                change.configuration_toml.as_bytes(),
            )?;
            validate_sha256_digest("remote proposal", &change.proposal_digest)?;
            if proposal.candidate_revision().as_str() != change.revision {
                bail!(
                    "remote Plugin configuration change does not match the Host-reviewed candidate revision: remote {}, Host {}",
                    change.revision,
                    proposal.candidate_revision()
                );
            }
            let publication = self.local.publish(&proposal)?;
            if publication.revision().as_str() != change.revision {
                bail!("materialized Plugin Root revision does not match the remote change");
            }
            current = publication.revision().clone();
        }
        if reply.revision != current.as_str() {
            bail!("remote Plugin configuration change batch does not reach its declared revision");
        }
        *self.sync_cursor.lock().map_err(|_| {
            anyhow::anyhow!("remote Plugin configuration cursor lock is poisoned")
        })? = Some(reply.cursor);
        Ok(changed)
    }
}

impl PluginConfigurationAuthority for RemotePluginConfigurationAuthority {
    fn source(&self) -> PluginConfigurationAuthoritySource {
        self.source.clone()
    }

    fn inspect(&self) -> anyhow::Result<PluginRootAuthoringState> {
        let _guard = self
            .access
            .lock()
            .map_err(|_| anyhow::anyhow!("remote Plugin configuration access lock is poisoned"))?;
        self.synchronize_locked(Duration::ZERO)?;
        self.local.inspect()
    }

    fn propose(
        &self,
        expected_revision: &PluginRootRevision,
        plugin_id: &str,
        instance: &str,
        bytes: &[u8],
    ) -> anyhow::Result<PluginConfigurationProposal> {
        let _guard = self
            .access
            .lock()
            .map_err(|_| anyhow::anyhow!("remote Plugin configuration access lock is poisoned"))?;
        let local = self
            .local
            .propose(expected_revision, plugin_id, instance, bytes)?;
        let request = RemoteProposalRequest {
            expected_revision: expected_revision.as_str(),
            toml: std::str::from_utf8(bytes).context("Plugin configuration must be UTF-8")?,
        };
        let remote: RemoteProposalResponse = self.send(
            Method::POST,
            self.plugin_endpoint(plugin_id, instance, &["proposals"])?,
            Some(serde_json::to_vec(&request)?),
            "proposal",
        )?;
        Self::compare_proposal(&local, &remote)?;
        self.remember_proposal(&local, bytes, &remote.proposal_digest, None)?;
        Ok(local)
    }

    fn publish(
        &self,
        proposal: &PluginConfigurationProposal,
    ) -> anyhow::Result<PluginConfigurationPublication> {
        let _guard = self
            .access
            .lock()
            .map_err(|_| anyhow::anyhow!("remote Plugin configuration access lock is poisoned"))?;
        let retained = self
            .proposals
            .lock()
            .map_err(|_| anyhow::anyhow!("remote Plugin configuration proposal lock is poisoned"))?
            .get(proposal.digest())
            .cloned()
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "remote Plugin configuration proposal bytes are unavailable; propose it again"
                )
            })?;
        let request = RemotePublicationRequest {
            expected_revision: proposal.base_revision().as_str(),
            proposal_digest: &retained.remote_digest,
            rollback_of_proposal_digest: retained.rollback_of_proposal_digest.as_deref(),
            toml: std::str::from_utf8(&retained.bytes)
                .context("Plugin configuration must be UTF-8")?,
        };
        let remote: RemotePublicationResponse = self.send(
            Method::PUT,
            self.plugin_endpoint(proposal.plugin_id(), proposal.instance_key(), &[])?,
            Some(serde_json::to_vec(&request)?),
            "publication",
        )?;
        ensure_schema(&remote.schema, "lenso.plugin-configuration-publication.v1")?;
        if remote.status != "published"
            || remote.base_revision != proposal.base_revision().as_str()
            || remote.proposal_digest != retained.remote_digest
            || remote.revision != proposal.candidate_revision().as_str()
        {
            bail!("remote Plugin configuration publication does not match the reviewed proposal");
        }
        let local = self.local.publish(proposal).with_context(|| {
            format!(
                "remote publication {} succeeded but materializing the Host Plugin Root failed; authority is divergent",
                proposal.digest()
            )
        })?;
        if local.revision().as_str() != remote.revision {
            bail!("materialized Plugin Root revision does not match remote desired revision");
        }
        self.proposals
            .lock()
            .map_err(|_| anyhow::anyhow!("remote Plugin configuration proposal lock is poisoned"))?
            .remove(proposal.digest());
        Ok(local)
    }
}

impl PluginConfigurationHistoryAuthority for RemotePluginConfigurationAuthority {
    fn publications(
        &self,
        plugin_id: &str,
        instance: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<PluginConfigurationPublicationRecord>> {
        let mut url = self.plugin_endpoint(plugin_id, instance, &["publications"])?;
        url.query_pairs_mut()
            .append_pair("limit", &limit.to_string());
        let reply: RemoteHistoryResponse =
            self.send(Method::GET, url, None, "publication history")?;
        ensure_schema(&reply.schema, "lenso.configuration.plugin-history.v1")?;
        if reply.plugin_id != plugin_id || reply.instance_key != instance {
            bail!("remote Plugin configuration history identity does not match the request");
        }
        Ok(reply
            .publications
            .into_iter()
            .map(|record| PluginConfigurationPublicationRecord {
                proposal_digest: record.proposal_digest,
                revision: record.revision,
                base_revision: record.base_revision,
                base_source_digest: record.base_source_digest,
                plugin_id: plugin_id.to_owned(),
                instance_key: instance.to_owned(),
                configuration_toml: record.configuration_toml,
                published_at_unix_ms: record.published_at_unix_ms,
                rollback_of_proposal_digest: record.rollback_of_proposal_digest,
            })
            .collect())
    }

    fn propose_rollback(
        &self,
        expected_revision: &PluginRootRevision,
        plugin_id: &str,
        instance: &str,
        publication_proposal_digest: &str,
    ) -> anyhow::Result<Option<(PluginConfigurationProposal, String)>> {
        let _guard = self
            .access
            .lock()
            .map_err(|_| anyhow::anyhow!("remote Plugin configuration access lock is poisoned"))?;
        let request = RemoteRollbackRequest {
            expected_revision: expected_revision.as_str(),
            publication_proposal_digest,
        };
        let response = self.http.request(
            Method::POST,
            self.plugin_endpoint(plugin_id, instance, &["rollback-proposals"])?,
            Some(serde_json::to_vec(&request)?),
        )?;
        if response.status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let status = response.status;
        let bytes = response.body;
        if bytes.len() > MAX_RESPONSE_BYTES {
            bail!("remote Plugin configuration rollback proposal response exceeded 2 MiB");
        }
        if !status.is_success() {
            let detail = serde_json::from_slice::<RemoteProblem>(&bytes).map_or_else(
                |_| {
                    status
                        .canonical_reason()
                        .unwrap_or("request failed")
                        .to_owned()
                },
                |problem| problem.detail,
            );
            bail!("remote Plugin configuration rollback proposal failed with {status}: {detail}");
        }
        let remote: RemoteRollbackResponse = serde_json::from_slice(&bytes)
            .context("decode remote Plugin configuration rollback proposal response")?;
        ensure_schema(
            &remote.schema,
            "lenso.configuration.plugin-rollback-proposal.v1",
        )?;
        if remote.rollback_of_proposal_digest != publication_proposal_digest {
            bail!("remote rollback proposal references a different publication");
        }
        let local = self.local.propose(
            expected_revision,
            plugin_id,
            instance,
            remote.configuration_toml.as_bytes(),
        )?;
        Self::compare_proposal(&local, &remote.proposal)?;
        self.remember_proposal(
            &local,
            remote.configuration_toml.as_bytes(),
            &remote.proposal.proposal_digest,
            Some(publication_proposal_digest),
        )?;
        Ok(Some((local, remote.configuration_toml)))
    }
}

fn validate_service_url(url: &Url) -> anyhow::Result<()> {
    if url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!(
            "remote Plugin configuration service URL cannot contain credentials, query, or fragment"
        );
    }
    match url.scheme() {
        "https" => Ok(()),
        "http" if is_loopback(url.host_str()) => Ok(()),
        "http" => bail!("remote Plugin configuration service requires HTTPS outside loopback"),
        _ => bail!("remote Plugin configuration service URL must use HTTPS or loopback HTTP"),
    }
}

fn is_loopback(host: Option<&str>) -> bool {
    host == Some("localhost")
        || host
            .and_then(|host| host.parse::<IpAddr>().ok())
            .is_some_and(|address| address.is_loopback())
}

fn validate_resource_segment(label: &str, value: String) -> anyhow::Result<String> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        bail!("remote Plugin configuration {label} identity is invalid");
    }
    Ok(value)
}

fn validate_cursor(cursor: &str) -> anyhow::Result<()> {
    if cursor.is_empty() || cursor.len() > 256 || cursor.chars().any(char::is_control) {
        bail!("remote Plugin configuration cursor is invalid");
    }
    Ok(())
}

fn validate_remote_proposal_evidence(remote: &RemoteProposalResponse) -> anyhow::Result<()> {
    validate_sha256_digest("remote proposal", &remote.proposal_digest)?;
    if !matches!(
        remote.status.as_str(),
        "ready" | "needs_decision" | "rejected"
    ) {
        bail!("remote Plugin configuration proposal status is invalid");
    }
    if !matches!(
        remote.application.as_str(),
        "noop" | "app_generation" | "blocked"
    ) {
        bail!("remote Plugin configuration proposal application is invalid");
    }
    if remote.diagnostics.len() > 64
        || remote.diagnostics.iter().any(|diagnostic| {
            diagnostic.code.is_empty()
                || diagnostic.code.len() > 64
                || diagnostic.detail.is_empty()
                || diagnostic.detail.len() > 4_096
        })
    {
        bail!("remote Plugin configuration proposal diagnostics are invalid");
    }
    Ok(())
}

fn validate_sha256_digest(label: &str, digest: &str) -> anyhow::Result<()> {
    let valid = digest.strip_prefix("sha256:").is_some_and(|value| {
        value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
    });
    if !valid {
        bail!("{label} digest is invalid");
    }
    Ok(())
}

fn ensure_schema(actual: &str, expected: &str) -> anyhow::Result<()> {
    if actual != expected {
        bail!(
            "remote Plugin configuration schema mismatch: expected {expected}, received {actual}"
        );
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct RemoteProblem {
    detail: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteProposalRequest<'a> {
    expected_revision: &'a str,
    toml: &'a str,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteProposalResponse {
    application: String,
    base_revision: String,
    candidate_revision: String,
    diagnostics: Vec<RemoteDiagnostic>,
    instance_key: String,
    plugin_id: String,
    proposal_digest: String,
    schema: String,
    status: String,
}

#[derive(Debug, Deserialize)]
struct RemoteDiagnostic {
    code: String,
    detail: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemotePublicationRequest<'a> {
    expected_revision: &'a str,
    proposal_digest: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    rollback_of_proposal_digest: Option<&'a str>,
    toml: &'a str,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemotePublicationResponse {
    base_revision: String,
    proposal_digest: String,
    revision: String,
    schema: String,
    status: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteHistoryResponse {
    instance_key: String,
    plugin_id: String,
    publications: Vec<RemotePublicationRecord>,
    schema: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemotePublicationRecord {
    base_revision: String,
    base_source_digest: Option<String>,
    configuration_toml: String,
    proposal_digest: String,
    published_at_unix_ms: i64,
    revision: String,
    rollback_of_proposal_digest: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteChangesResponse {
    base_revision: String,
    changes: Vec<RemoteConfigurationChange>,
    cursor: String,
    revision: String,
    schema: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteConfigurationChange {
    base_revision: String,
    configuration_toml: String,
    instance_key: String,
    plugin_id: String,
    proposal_digest: String,
    revision: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RemoteRollbackRequest<'a> {
    expected_revision: &'a str,
    publication_proposal_digest: &'a str,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoteRollbackResponse {
    configuration_toml: String,
    proposal: RemoteProposalResponse,
    rollback_of_proposal_digest: String,
    schema: String,
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        net::SocketAddr,
        thread::{self, JoinHandle},
    };

    use axum::extract::{Query, State};
    use lenso_agent_host::{AgentHost, WebSurface};
    use lenso_app_plan::authoring::{
        HostCatalog, HostDefaultPlugin, HostPluginRelease, HostSlot, PluginDescriptor,
    };
    use serde_json::json;
    use tokio::sync::oneshot;

    use super::*;
    use crate::{
        PluginConfigurationService, PluginConfigurationServiceAccess,
        PluginConfigurationServiceResource, PluginConfigurationStoreConfig,
        SqlitePluginConfigurationAuthority,
    };

    const TOKEN: &str = "test-remote-authority-token";
    const READ_TOKEN: &str = "test-read-only-token";

    struct TestServer {
        address: SocketAddr,
        service: PluginConfigurationService,
        shutdown: Option<oneshot::Sender<()>>,
        thread: Option<JoinHandle<()>>,
    }

    impl Drop for TestServer {
        fn drop(&mut self) {
            if let Some(shutdown) = self.shutdown.take() {
                let _ = shutdown.send(());
            }
            if let Some(thread) = self.thread.take() {
                thread.join().unwrap();
            }
        }
    }

    impl TestServer {
        fn start(authority: SqlitePluginConfigurationAuthority) -> Self {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let address = listener.local_addr().unwrap();
            let service = PluginConfigurationService::new(
                authority,
                PluginConfigurationServiceResource::new("agent", "production").unwrap(),
                PluginConfigurationServiceAccess::new(READ_TOKEN, TOKEN).unwrap(),
            );
            let router = service.router();
            let (shutdown, receiver) = oneshot::channel();
            let thread = thread::spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                runtime.block_on(async move {
                    let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                    axum::serve(listener, router)
                        .with_graceful_shutdown(async {
                            let _ = receiver.await;
                        })
                        .await
                        .unwrap();
                });
            });
            Self {
                address,
                service,
                shutdown: Some(shutdown),
                thread: Some(thread),
            }
        }

        fn service_url(&self) -> String {
            format!("http://{}/", self.address)
        }

        fn authority(&self) -> &SqlitePluginConfigurationAuthority {
            self.service.authority()
        }
    }

    fn fixture_root() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".lenso")).unwrap();
        let descriptor = PluginDescriptor::new("example.agent", "1.0.0", "agent")
            .with_configuration_schema(json!({
                "type": "object",
                "properties": {
                    "greeting": { "type": "string" }
                },
                "additionalProperties": false
            }));
        let host = HostCatalog::new(
            [HostSlot::many("agent")],
            [HostPluginRelease::new(descriptor)],
            [
                HostDefaultPlugin::new("example.agent", "default"),
                HostDefaultPlugin::new("example.agent", "secondary"),
            ],
        );
        fs::write(
            root.path().join(".lenso/host-catalog.json"),
            serde_json::to_vec(&host).unwrap(),
        )
        .unwrap();
        root
    }

    fn remote_authority(root: &tempfile::TempDir) -> SqlitePluginConfigurationAuthority {
        SqlitePluginConfigurationAuthority::open(
            root.path(),
            PluginConfigurationStoreConfig::new(
                root.path().join("configuration.sqlite3"),
                "agent/production",
            ),
        )
        .unwrap()
    }

    fn prepare_default_host(root: &tempfile::TempDir) {
        AgentHost::builder()
            .plugins(lenso_agent_default_plugins::link)
            .agent_home(root.path())
            .unwrap()
            .surface(WebSurface::browser())
            .build()
            .unwrap()
            .prepare_authoring()
            .unwrap();
    }

    fn connect(
        root: &tempfile::TempDir,
        server: &TestServer,
        token: &str,
    ) -> anyhow::Result<RemotePluginConfigurationAuthority> {
        let resource =
            RemotePluginConfigurationResource::new(server.service_url(), "agent", "production")?;
        RemotePluginConfigurationAuthority::connect(
            root.path(),
            RemotePluginConfigurationConfig::new(resource, token)?,
        )
    }

    #[test]
    fn validates_remote_resource_identity_and_transport() {
        assert!(
            RemotePluginConfigurationResource::new(
                "https://config.example.com/base",
                "agent",
                "production"
            )
            .is_ok()
        );
        assert!(
            RemotePluginConfigurationResource::new(
                "http://config.example.com",
                "agent",
                "production"
            )
            .unwrap_err()
            .to_string()
            .contains("requires HTTPS")
        );
        assert!(
            RemotePluginConfigurationResource::new(
                "http://127.0.0.1:8080",
                "bad/app",
                "production"
            )
            .is_err()
        );
        assert!(
            ensure_schema(
                "lenso.configuration.plugin-management.v0",
                "lenso.configuration.plugin-management.v1"
            )
            .unwrap_err()
            .to_string()
            .contains("schema mismatch")
        );
    }

    #[test]
    fn publishes_history_and_rollback_through_the_remote_cas_resource() {
        let remote_root = fixture_root();
        let local_root = fixture_root();
        let server = TestServer::start(remote_authority(&remote_root));
        let authority = connect(&local_root, &server, TOKEN).unwrap();
        assert_eq!(authority.source().kind(), "remote_configuration_service");
        assert!(
            authority
                .source()
                .reference()
                .contains("/apps/agent/environments/production")
        );

        let initial = authority.inspect().unwrap().revision().clone();
        let first = authority
            .propose(
                &initial,
                "example.agent",
                "default",
                b"greeting = \"first\"\n",
            )
            .unwrap();
        let first_digest = first.digest().to_owned();
        let first_publication = authority.publish(&first).unwrap();
        assert_eq!(
            fs::read(local_root.path().join("plugins/example.agent/default.toml")).unwrap(),
            b"greeting = \"first\"\n"
        );
        assert_eq!(
            fs::read(
                remote_root
                    .path()
                    .join("plugins/example.agent/default.toml")
            )
            .unwrap(),
            b"greeting = \"first\"\n"
        );

        let second = authority
            .propose(
                first_publication.revision(),
                "example.agent",
                "default",
                b"greeting = \"second\"\n",
            )
            .unwrap();
        let second_publication = authority.publish(&second).unwrap();
        let history = authority
            .publications("example.agent", "default", 20)
            .unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].plugin_id, "example.agent");
        assert_eq!(history[0].instance_key, "default");

        let (rollback, toml) = authority
            .propose_rollback(
                second_publication.revision(),
                "example.agent",
                "default",
                &first_digest,
            )
            .unwrap()
            .unwrap();
        assert_eq!(toml, "greeting = \"first\"\n");
        authority.publish(&rollback).unwrap();
        let history = authority
            .publications("example.agent", "default", 20)
            .unwrap();
        assert_eq!(history.len(), 3);
        assert_eq!(
            history[0].rollback_of_proposal_digest.as_deref(),
            Some(first_digest.as_str())
        );
    }

    #[test]
    fn rejects_bad_authorization_and_recovers_a_remote_publication_on_connect() {
        let remote_root = fixture_root();
        let local_root = fixture_root();
        let remote_authority = remote_authority(&remote_root);
        let base = remote_authority.inspect().unwrap().revision().clone();
        let proposal = remote_authority
            .propose(
                &base,
                "example.agent",
                "default",
                b"greeting = \"remote-only\"\n",
            )
            .unwrap();
        let remote_revision = remote_authority
            .publish(&proposal)
            .unwrap()
            .revision()
            .clone();
        let server = TestServer::start(remote_authority);
        let unauthorized = connect(&local_root, &server, "wrong-token").unwrap_err();
        assert!(unauthorized.to_string().contains("401 Unauthorized"));

        let recovered = connect(&local_root, &server, TOKEN).unwrap();
        assert_eq!(recovered.inspect().unwrap().revision(), &remote_revision);
        assert_eq!(
            fs::read(local_root.path().join("plugins/example.agent/default.toml")).unwrap(),
            b"greeting = \"remote-only\"\n"
        );

        let read_only = connect(&local_root, &server, READ_TOKEN).unwrap();
        let error = read_only
            .propose(
                &remote_revision,
                "example.agent",
                "default",
                b"greeting = \"forbidden\"\n",
            )
            .unwrap_err();
        assert!(error.to_string().contains("403 Forbidden"));
    }

    #[test]
    fn rejects_a_revision_history_gap_without_overwriting_the_plugin_root() {
        let remote_root = fixture_root();
        let local_root = fixture_root();
        let remote = remote_authority(&remote_root);
        let remote_base = remote.inspect().unwrap().revision().clone();
        let remote_proposal = remote
            .propose(
                &remote_base,
                "example.agent",
                "default",
                b"greeting = \"remote\"\n",
            )
            .unwrap();
        remote.publish(&remote_proposal).unwrap();

        let local = LocalPluginRootAuthority::new(local_root.path());
        let local_base = local.inspect().unwrap().revision().clone();
        assert_eq!(local_base, remote_base);
        let local_proposal = local
            .propose(
                &local_base,
                "example.agent",
                "default",
                b"greeting = \"local\"\n",
            )
            .unwrap();
        local.publish(&local_proposal).unwrap();

        let server = TestServer::start(remote);
        let error = connect(&local_root, &server, TOKEN).unwrap_err();
        assert!(error.to_string().contains("revision history gap"));
        assert_eq!(
            fs::read(local_root.path().join("plugins/example.agent/default.toml")).unwrap(),
            b"greeting = \"local\"\n"
        );
    }

    #[test]
    fn recovers_one_global_change_chain_across_plugin_instances() {
        let remote_root = fixture_root();
        let local_root = fixture_root();
        let remote = remote_authority(&remote_root);
        let base = remote.inspect().unwrap().revision().clone();
        let first = remote
            .propose(&base, "example.agent", "default", b"greeting = \"first\"\n")
            .unwrap();
        let first = remote.publish(&first).unwrap();
        let second = remote
            .propose(
                first.revision(),
                "example.agent",
                "secondary",
                b"greeting = \"second\"\n",
            )
            .unwrap();
        let expected = remote.publish(&second).unwrap().revision().clone();

        let server = TestServer::start(remote);
        let recovered = connect(&local_root, &server, TOKEN).unwrap();
        assert_eq!(recovered.inspect().unwrap().revision(), &expected);
        assert_eq!(
            fs::read(local_root.path().join("plugins/example.agent/default.toml")).unwrap(),
            b"greeting = \"first\"\n"
        );
        assert_eq!(
            fs::read(
                local_root
                    .path()
                    .join("plugins/example.agent/secondary.toml")
            )
            .unwrap(),
            b"greeting = \"second\"\n"
        );
    }

    #[test]
    fn rejects_a_stale_remote_cas_without_local_fallback() {
        let remote_root = fixture_root();
        let local_root = fixture_root();
        let server = TestServer::start(remote_authority(&remote_root));
        let authority = connect(&local_root, &server, TOKEN).unwrap();
        let base = authority.inspect().unwrap().revision().clone();
        let winner = authority
            .propose(
                &base,
                "example.agent",
                "default",
                b"greeting = \"winner\"\n",
            )
            .unwrap();
        let stale = authority
            .propose(&base, "example.agent", "default", b"greeting = \"stale\"\n")
            .unwrap();
        authority.publish(&winner).unwrap();

        let error = authority.publish(&stale).unwrap_err();
        assert!(error.to_string().contains("revision conflict"));
        assert_eq!(
            fs::read(local_root.path().join("plugins/example.agent/default.toml")).unwrap(),
            b"greeting = \"winner\"\n"
        );
        assert_eq!(
            fs::read(
                remote_root
                    .path()
                    .join("plugins/example.agent/default.toml")
            )
            .unwrap(),
            b"greeting = \"winner\"\n"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn watches_remote_changes_and_switches_a_ready_generation() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let remote_root = tempfile::tempdir().unwrap();
                let local_root = tempfile::tempdir().unwrap();
                prepare_default_host(&remote_root);
                prepare_default_host(&local_root);
                let remote = SqlitePluginConfigurationAuthority::open(
                    remote_root.path(),
                    PluginConfigurationStoreConfig::new(
                        remote_root.path().join("configuration.sqlite3"),
                        "agent/production",
                    ),
                )
                .unwrap();
                let server = TestServer::start(remote);
                let resource = RemotePluginConfigurationResource::new(
                    server.service_url(),
                    "agent",
                    "production",
                )
                .unwrap();
                let mut config = crate::AgentWebConfig::new(lenso_agent_default_plugins::link);
                config.agent_home = Some(local_root.path().to_path_buf());
                config.control = crate::AgentWebControl::HostAuthorized;
                config.plugin_control = true;
                config.plugin_configuration_remote =
                    Some(RemotePluginConfigurationConfig::new(resource, TOKEN).unwrap());
                let surface = crate::AgentWebSurface::start(config).await.unwrap();
                let before = server.authority().inspect().unwrap().revision().clone();
                let configuration = concat!(
                    "model = \"gpt-5.6-luna\"\n",
                    "max_steps = 7\n",
                    "max_tool_calls = 4\n",
                    "max_parallel_tool_calls = 4\n",
                    "max_output_tokens = 1024\n",
                    "max_history_events = 200\n",
                    "max_compaction_summary_characters = 8192\n",
                    "max_memory_items = 8\n",
                    "max_memory_characters = 16384\n",
                );
                let proposal = server
                    .authority()
                    .propose(
                        &before,
                        "lenso.agent.loop",
                        "agent",
                        configuration.as_bytes(),
                    )
                    .unwrap();
                let revision = server
                    .authority()
                    .publish(&proposal)
                    .unwrap()
                    .revision()
                    .as_str()
                    .to_owned();

                let mut observed_switch = false;
                tokio::time::timeout(Duration::from_secs(8), async {
                    loop {
                        let inventory = crate::plugin_control::plugin_inventory(
                            State(surface.runtime.clone()),
                            Query(crate::plugin_control::PluginInventoryQuery::default()),
                        )
                        .await
                        .unwrap()
                        .0;
                        observed_switch |= inventory
                            .events
                            .iter()
                            .any(|event| event.status() == "switched");
                        if inventory.applied_revision.as_deref() == Some(revision.as_str()) {
                            assert_eq!(
                                inventory.desired_revision.as_deref(),
                                Some(revision.as_str())
                            );
                            assert_eq!(inventory.configuration_status, "applied");
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                })
                .await
                .expect("remote configuration should reach the Ready-gated Generation");
                assert!(observed_switch);
                assert_eq!(
                    fs::read_to_string(
                        local_root
                            .path()
                            .join("plugins/lenso.agent.loop/agent.toml")
                    )
                    .unwrap(),
                    configuration
                );
                surface.shutdown().await.unwrap();
            })
            .await;
    }
}
