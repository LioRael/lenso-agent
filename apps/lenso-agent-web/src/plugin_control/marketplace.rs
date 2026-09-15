#[cfg(test)]
use super::HeaderMap;
// Target-owned Marketplace installation authority. No catalog-specific Kernel wiring.
use super::{
    ApiProblem, BTreeMap, CommittedPluginMutation, Deserialize, Json, PathBuf, PluginControl,
    PluginInventoryQuery, PluginMutationLinearization, Query, Serialize, StagedHome, State, Value,
    WebRuntime, fs, plugin_inventory, read_plugin_operation,
};
use anyhow::{Context, Result, ensure};
use lenso_app_authoring::{
    BundleMutation,
    bundle_archive::{
        PluginArchiveDownloadPolicy, PluginArchiveIdentity, PluginReleaseIdentity,
        VerifiedPluginArchive,
    },
    signed_plugin_catalog as catalog,
};
use rusqlite::{Connection, OptionalExtension};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    name: String,
    catalogs: Vec<CatalogTrust>,
    artifact_origins: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogTrust {
    catalog_id: String,
    key_id: String,
    public_key_hex: String,
    #[serde(default)]
    snapshot_url: Option<String>,
}

// Public trust metadata is supplied by the release builder, never by a model
// argument or an ambient environment variable at Agent runtime.
const OFFICIAL_POLICY: Option<&str> = option_env!("LENSO_OFFICIAL_MARKETPLACE_POLICY");

impl Policy {
    fn load(home: &std::path::Path, bundled: Option<&str>) -> Result<Option<Self>> {
        let config = home.join(".lenso/marketplace-policy.json");
        let bytes = match fs::metadata(&config) {
            Ok(metadata) => {
                ensure!(metadata.len() <= 32768, "Marketplace policy exceeds bounds");
                // An explicit operator policy remains authoritative, including
                // errors. Do not silently substitute a different source.
                fs::read(config)?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(bundled) = bundled.filter(|value| !value.trim().is_empty()) else {
                    return Ok(None);
                };
                bundled.as_bytes().to_vec()
            }
            Err(error) => return Err(error.into()),
        };
        ensure!(bytes.len() <= 32768, "Marketplace policy exceeds bounds");
        let policy: Self = serde_json::from_slice(&bytes)?;
        ensure!(
            !policy.name.trim().is_empty()
                && policy.name.len() <= 160
                && !policy.catalogs.is_empty()
                && policy.catalogs.len() <= 16,
            "invalid Marketplace policy"
        );
        PluginArchiveDownloadPolicy::new(&policy.artifact_origins)?;
        Ok(Some(policy))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Proposal {
    id: String,
    target_id: String,
    base_revision: String,
    candidate_revision: String,
    catalog_id: String,
    release: catalog::Release,
    previous_version: Option<String>,
    configuration: Value,
    expires_at: u64,
    digest: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrepareRequest {
    target_id: String,
    base_revision: String,
    catalog_id: String,
    envelope: String,
    plugin_id: String,
    version: String,
    #[serde(default = "empty_object")]
    configuration: Value,
}
fn empty_object() -> Value {
    serde_json::json!({})
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyRequest {
    target_id: String,
    proposal_id: String,
    proposal_digest: String,
    idempotency_key: String,
    expected_stream_id: String,
    envelope: String,
}
#[derive(Clone, Serialize, Deserialize)]
struct Receipt {
    id: String,
    plugin_id: String,
    version: String,
    target_id: String,
    proposal_id: String,
    phase: String,
    candidate_revision: String,
    runtime_operation_id: Option<String>,
    detail: Option<String>,
    #[serde(default)]
    accepted_cursor: u64,
}

struct Store {
    connection: Connection,
    root: PathBuf,
    policy: Policy,
    target_id: String,
}
impl Store {
    fn open(control: &PluginControl) -> Result<Self> {
        ensure!(
            control.configuration_authority_is_builtin_local
                || control.configuration_store.is_some(),
            "selected authority does not admit Marketplace installation"
        );
        let policy = Policy::load(&control.authority_home, OFFICIAL_POLICY)?
            .context("Marketplace is not configured in this Agent distribution")?;
        let root = control.authority_home.join(".lenso/marketplace");
        fs::create_dir_all(root.join("artifacts"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        }
        let connection = Connection::open(root.join("operations.sqlite3"))?;
        connection.busy_timeout(Duration::from_secs(3))?;
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS identity(id TEXT NOT NULL); CREATE TABLE IF NOT EXISTS catalogs(id TEXT PRIMARY KEY,envelope TEXT NOT NULL,checkpoint TEXT NOT NULL); CREATE TABLE IF NOT EXISTS proposals(id TEXT PRIMARY KEY,body TEXT NOT NULL); CREATE TABLE IF NOT EXISTS operations(id TEXT PRIMARY KEY,proposal TEXT NOT NULL UNIQUE,request_digest TEXT NOT NULL,body TEXT NOT NULL);")?;
        let target_id = connection
            .query_row("SELECT id FROM identity", [], |r| r.get::<_, String>(0))
            .optional()?;
        let target_id = match target_id {
            Some(id) => id,
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                connection.execute(
                    "INSERT INTO identity SELECT ?1 WHERE NOT EXISTS(SELECT 1 FROM identity)",
                    [&id],
                )?;
                connection.query_row("SELECT id FROM identity", [], |r| r.get(0))?
            }
        };
        Ok(Self {
            connection,
            root,
            policy,
            target_id,
        })
    }
    fn accept(&mut self, id: &str, envelope: &str) -> Result<catalog::VerifiedSnapshot> {
        ensure!(
            envelope.len() <= catalog::MAX_ENVELOPE_BYTES,
            "catalog exceeds bounds"
        );
        let mut keys = BTreeMap::new();
        for key in self
            .policy
            .catalogs
            .iter()
            .filter(|key| key.catalog_id == id)
        {
            let bytes: [u8; 32] = hex::decode(&key.public_key_hex)?
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid trust key"))?;
            ensure!(
                keys.insert(
                    key.key_id.clone(),
                    ed25519_dalek::VerifyingKey::from_bytes(&bytes)?
                )
                .is_none(),
                "duplicate trust key"
            );
        }
        ensure!(!keys.is_empty(), "target does not trust this catalog");
        let trust = catalog::Trust {
            catalog_id: id.into(),
            keys,
        };
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let previous: Option<String> = transaction
            .query_row("SELECT checkpoint FROM catalogs WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        let previous = previous
            .map(|text| serde_json::from_str::<catalog::Checkpoint>(&text))
            .transpose()?;
        let verified = catalog::verify(envelope.as_bytes(), &trust, previous.as_ref(), now())?;
        transaction.execute("INSERT INTO catalogs VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET envelope=excluded.envelope,checkpoint=excluded.checkpoint",[id,envelope,&serde_json::to_string(verified.checkpoint())?])?;
        transaction.commit()?;
        Ok(verified)
    }
    fn artifact(&self, release: &catalog::Release) -> PathBuf {
        self.root.join("artifacts").join(format!(
            "{}.lenso-plugin",
            release.artifact.digest.trim_start_matches("sha256:")
        ))
    }
    fn archive(&self, release: &catalog::Release) -> Result<VerifiedPluginArchive> {
        VerifiedPluginArchive::read_release(
            fs::File::open(self.artifact(release))?,
            &transport(release),
            &release_identity(release),
        )
    }
    fn acquire(&self, release: &catalog::Release) -> Result<()> {
        let destination = self.artifact(release);
        if destination.exists() {
            self.archive(release)?;
            return Ok(());
        }
        let used = fs::read_dir(self.root.join("artifacts"))?.try_fold(0u64, |sum, item| {
            Ok::<_, std::io::Error>(sum.saturating_add(item?.metadata()?.len()))
        })?;
        ensure!(
            used.saturating_add(release.artifact.size) <= 512 * 1024 * 1024,
            "target archive cache quota exceeded"
        );
        let archive = PluginArchiveDownloadPolicy::new(&self.policy.artifact_origins)?
            .download_release(
                &release.artifact.url,
                &transport(release),
                &release_identity(release),
            )?;
        let mut file = tempfile::NamedTempFile::new_in(self.root.join("artifacts"))?;
        std::io::copy(&mut archive.open_archive()?, &mut file)?;
        file.as_file().sync_all()?;
        if file.persist_noclobber(&destination).is_err() {
            self.archive(release)?;
        }
        Ok(())
    }
    fn proposal(&self, id: &str) -> Result<Proposal> {
        let body: String =
            self.connection
                .query_row("SELECT body FROM proposals WHERE id=?1", [id], |r| r.get(0))?;
        Ok(serde_json::from_str(&body)?)
    }
    fn receipt(&self, id: &str) -> Result<Option<Receipt>> {
        let body: Option<String> = self
            .connection
            .query_row("SELECT body FROM operations WHERE id=?1", [id], |r| {
                r.get(0)
            })
            .optional()?;
        body.map(|body| serde_json::from_str(&body).map_err(Into::into))
            .transpose()
    }
    fn save_receipt(&self, receipt: &Receipt) -> Result<()> {
        self.connection.execute(
            "UPDATE operations SET body=?2 WHERE id=?1",
            [&receipt.id, &serde_json::to_string(receipt)?],
        )?;
        Ok(())
    }
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn transport(r: &catalog::Release) -> PluginArchiveIdentity {
    PluginArchiveIdentity {
        size: r.artifact.size,
        sha256: r.artifact.digest.clone(),
    }
}
fn release_identity(r: &catalog::Release) -> PluginReleaseIdentity {
    PluginReleaseIdentity {
        plugin_id: r.plugin_id.clone(),
        release_version: r.version.clone(),
        manifest_digest: r.artifact.manifest_digest.clone(),
    }
}
#[cfg(test)]
fn revision(control: &PluginControl) -> Result<String> {
    Ok(control
        .configuration_authority
        .inspect()
        .map_err(anyhow::Error::msg)?
        .revision()
        .as_str()
        .to_string())
}
fn previous_version(control: &PluginControl, id: &str) -> Result<Option<String>> {
    let path = control
        .app_root
        .join("plugins")
        .join(id)
        .join("plugin.lenso-plugin");
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(
        lenso_plugin_bundle::verify_bundle_directory(&path)?.release_version,
    ))
}
fn locked_revision(control: &PluginControl) -> Result<String> {
    let snapshot = StagedHome::new(&control.app_root).map_err(anyhow::Error::msg)?;
    Ok(lenso_app_authoring::inspect_plugin_root(&snapshot.home)?
        .revision()
        .as_str()
        .to_string())
}
fn stage(
    control: &PluginControl,
    archive: &VerifiedPluginArchive,
    proposal: &Proposal,
) -> Result<(StagedHome, lenso_agent_host::DesiredPluginRootSnapshot)> {
    let staged = StagedHome::new(&control.app_root).map_err(anyhow::Error::msg)?;
    archive
        .prepare_mutation(
            &staged.home,
            if proposal.previous_version.is_some() {
                BundleMutation::Replace
            } else {
                BundleMutation::Add
            },
        )?
        .commit()?;
    if proposal.previous_version.is_none() {
        ensure!(
            proposal.configuration.is_object(),
            "configuration must be an object"
        );
        let configuration = toml::to_string(&proposal.configuration)?;
        lenso_app_authoring::configure_instance(
            &staged.home,
            &proposal.release.plugin_id,
            "default",
            configuration.as_bytes(),
        )?;
    }
    control
        .validate_staged(&staged)
        .map_err(anyhow::Error::msg)?;
    let desired = lenso_agent_host::snapshot_desired_plugin_root_for_home(
        &staged.home,
        &control.authority_home,
        control.snapshot_profile().as_deref(),
    )
    .map_err(anyhow::Error::msg)?;
    Ok((staged, desired))
}
fn prepare(control: &PluginControl, request: PrepareRequest) -> Result<Proposal> {
    ensure!(
        request.configuration.to_string().len() <= 65536,
        "configuration exceeds bounds"
    );
    let mut store = Store::open(control)?;
    ensure!(
        request.target_id == store.target_id,
        "target changed; reconnect before preparing"
    );
    let verified = store.accept(&request.catalog_id, &request.envelope)?;
    let release = verified
        .select(&request.plugin_id, &request.version, now())?
        .clone();
    store.acquire(&release)?;
    let _guard = control
        .mutation
        .lock()
        .map_err(|_| anyhow::anyhow!("target lock poisoned"))?;
    let _fence = PluginMutationLinearization::acquire(&control.app_root, &control.authority_home)
        .map_err(anyhow::Error::msg)?;
    ensure!(
        locked_revision(control)? == request.base_revision,
        "target revision changed; prepare again"
    );
    verified.select(&request.plugin_id, &request.version, now())?;
    let previous = previous_version(control, &request.plugin_id)?;
    ensure!(
        previous.as_deref() != Some(&request.version),
        "this exact version is already installed"
    );
    let mut proposal = Proposal {
        id: uuid::Uuid::new_v4().to_string(),
        target_id: store.target_id.clone(),
        base_revision: request.base_revision,
        candidate_revision: String::new(),
        catalog_id: request.catalog_id,
        release,
        previous_version: previous,
        configuration: request.configuration,
        expires_at: verified.snapshot().expires_at.min(now() + 600),
        digest: String::new(),
    };
    let archive = store.archive(&proposal.release)?;
    let (_, desired) = stage(control, &archive, &proposal)?;
    proposal.candidate_revision = desired.plugin_root_revision().into();
    proposal.digest = catalog::digest(&serde_json::to_vec(&proposal)?);
    store.connection.execute("DELETE FROM proposals WHERE json_extract(body,'$.expires_at') < ?1 AND id NOT IN(SELECT proposal FROM operations)",[i64::try_from(now())?])?;
    let count: i64 = store.connection.query_row(
        "SELECT COUNT(*) FROM proposals WHERE id NOT IN(SELECT proposal FROM operations)",
        [],
        |r| r.get(0),
    )?;
    ensure!(count < 128, "target proposal limit reached");
    store.connection.execute(
        "INSERT INTO proposals VALUES(?1,?2)",
        [&proposal.id, &serde_json::to_string(&proposal)?],
    )?;
    Ok(proposal)
}

fn commit(control: &PluginControl, proposal: &Proposal) -> Result<CommittedPluginMutation, String> {
    (|| -> Result<_> {
        let _guard = control
            .mutation
            .lock()
            .map_err(|_| anyhow::anyhow!("target lock poisoned"))?;
        if let Some(authority) = &control.configuration_store {
            authority.publish_package_change(
                &proposal.id,
                &proposal.base_revision,
                &proposal.candidate_revision,
                || commit_package(control, proposal),
            )
        } else {
            commit_package(control, proposal)
        }
    })()
    .map_err(|error| format!("{error:#}"))
}

// The returned fence remains held until the managed authority has recorded the
// candidate revision and the caller registers its Generation operation.
fn commit_package(control: &PluginControl, proposal: &Proposal) -> Result<CommittedPluginMutation> {
    let linearization =
        PluginMutationLinearization::acquire(&control.app_root, &control.authority_home)
            .map_err(anyhow::Error::msg)?;
    ensure!(
        locked_revision(control)? == proposal.base_revision,
        "target changed after review; prepare again"
    );
    ensure!(now() < proposal.expires_at, "installation proposal expired");
    let mut store = Store::open(control)?;
    ensure!(
        store.target_id == proposal.target_id,
        "installation target changed"
    );
    let envelope: String = store.connection.query_row(
        "SELECT envelope FROM catalogs WHERE id=?1",
        [&proposal.catalog_id],
        |r| r.get(0),
    )?;
    let verified = store.accept(&proposal.catalog_id, &envelope)?;
    let release = verified.select(
        &proposal.release.plugin_id,
        &proposal.release.version,
        now(),
    )?;
    ensure!(
        release.artifact == proposal.release.artifact,
        "reviewed release changed"
    );
    let archive = store.archive(release)?;
    let (staged, desired) = stage(control, &archive, proposal)?;
    ensure!(
        desired.plugin_root_revision() == proposal.candidate_revision,
        "candidate differs from reviewed proposal"
    );
    let source = staged
        .home
        .join("plugins")
        .join(&proposal.release.plugin_id);
    let destination = control
        .app_root
        .join("plugins")
        .join(&proposal.release.plugin_id);
    fs::create_dir_all(destination.parent().context("missing Plugin Root")?)?;
    #[cfg(any(target_os = "linux", target_vendor = "apple"))]
    {
        use rustix::fs::{CWD, RenameFlags, renameat_with};
        let flags = if proposal.previous_version.is_some() {
            RenameFlags::EXCHANGE
        } else {
            RenameFlags::NOREPLACE
        };
        renameat_with(CWD, &source, CWD, &destination, flags)?;
        match control.snapshot_committed() {
            Ok(committed) if committed.plugin_root_revision() == proposal.candidate_revision => {
                Ok(CommittedPluginMutation {
                    desired: committed,
                    linearization,
                })
            }
            result => {
                if proposal.previous_version.is_some() {
                    renameat_with(CWD, &source, CWD, &destination, RenameFlags::EXCHANGE)?;
                } else {
                    fs::rename(&destination, &source)?;
                }
                Err(anyhow::anyhow!(
                    "candidate publication did not produce reviewed revision: {}",
                    result.err().unwrap_or_else(|| "revision mismatch".into())
                ))
            }
        }
    }
    #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
    anyhow::bail!("atomic Marketplace installation is unavailable on this platform");
}

fn problem(error: impl std::fmt::Display) -> ApiProblem {
    ApiProblem::conflict(error.to_string())
}
#[cfg(test)]
async fn target(
    State(runtime): State<WebRuntime>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiProblem> {
    runtime.authorize_control(&headers)?;
    let control = runtime.plugin_control()?;
    let Json(inventory) =
        plugin_inventory(State(runtime), Query(PluginInventoryQuery { after: None })).await?;
    // Inspection takes the authoring lock. Never block the async runtime that a
    // concurrent publisher needs to register its Generation operation.
    tokio::task::spawn_blocking(move || -> Result<Value> {
        let store = Store::open(&control)?;
        let management = serde_json::to_value(control.inspect().map_err(anyhow::Error::msg)?)?;
        let mut installed = Vec::new();
        for plugin in management["plugins"].as_array().context("invalid management inventory")? {
            if let Some(id) = plugin["packageId"].as_str()
                && let Some(version) = previous_version(&control,id)? {
                installed.push(serde_json::json!({"plugin_id":id,"version":version}));
            }
        }
        let active = serde_json::to_value(inventory.active)?;
        Ok(serde_json::json!({"target_id":store.target_id,"name":store.policy.name,"revision":revision(&control)?,"stream_id":inventory.stream_id,"active_revision":active["pluginRootRevision"],"installed":installed}))
    }).await.map_err(problem)?.map(Json).map_err(problem)
}
#[expect(
    clippy::too_many_lines,
    reason = "keep the durable reservation, publication fence and runtime registration in one auditable transaction"
)]
async fn apply(runtime: WebRuntime, request: ApplyRequest) -> Result<Json<Receipt>, ApiProblem> {
    uuid::Uuid::parse_str(&request.idempotency_key).map_err(problem)?;
    // The coordinator serializes one target's publication and receipt registration.
    runtime
        .plugin_mutations
        .run(async {
            let control = runtime.plugin_control()?;
            let mut store = Store::open(&control).map_err(problem)?;
            if request.target_id != store.target_id {
                return Err(problem("target changed"));
            }
            let request_digest = catalog::digest(
                format!(
                    "{}:{}:{}",
                    request.target_id, request.proposal_id, request.proposal_digest
                )
                .as_bytes(),
            );
            if let Some(receipt) = store.receipt(&request.idempotency_key).map_err(problem)? {
                let stored: String = store
                    .connection
                    .query_row(
                        "SELECT request_digest FROM operations WHERE id=?1",
                        [&request.idempotency_key],
                        |r| r.get(0),
                    )
                    .map_err(problem)?;
                if stored != request_digest {
                    return Err(problem(
                        "idempotency key belongs to a different reviewed operation",
                    ));
                }
                return Ok(Json(receipt));
            }
            runtime
                .validate_plugin_stream(&request.expected_stream_id)
                .await?;
            let proposal = store.proposal(&request.proposal_id).map_err(problem)?;
            if proposal.digest != request.proposal_digest || proposal.target_id != request.target_id
            {
                return Err(problem("proposal does not match review"));
            }
            store
                .accept(&proposal.catalog_id, &request.envelope)
                .map_err(problem)?
                .select(
                    &proposal.release.plugin_id,
                    &proposal.release.version,
                    now(),
                )
                .map_err(problem)?;
            let mut receipt = Receipt {
                plugin_id: proposal.release.plugin_id.clone(),
                version: proposal.release.version.clone(),
                id: request.idempotency_key,
                target_id: store.target_id.clone(),
                proposal_id: proposal.id.clone(),
                phase: "applying".into(),
                candidate_revision: proposal.candidate_revision.clone(),
                runtime_operation_id: None,
                detail: None,
                accepted_cursor: 0,
            };
            store
                .connection
                .execute(
                    "INSERT INTO operations VALUES(?1,?2,?3,?4)",
                    rusqlite::params![
                        receipt.id,
                        proposal.id,
                        request_digest,
                        serde_json::to_string(&receipt).map_err(problem)?
                    ],
                )
                .map_err(problem)?;
            let committed = tokio::task::spawn_blocking(move || commit(&control, &proposal))
                .await
                .map_err(problem)?;
            let (desired, linearization) = match committed {
                Ok(value) => (Ok(value.desired), Some(value.linearization)),
                Err(error) => (Err(error), None),
            };
            let fence = runtime.plugin_observation_fence().await?;
            receipt.accepted_cursor = fence.cursor;
            let operation = runtime
                .register_plugin_mutation(fence.cursor, fence.cursor, fence.desired_epoch, desired)
                .await?;
            drop(linearization);
            receipt.runtime_operation_id = Some(operation.operation.id.clone());
            receipt.detail = serde_json::to_value(&operation.operation).map_err(problem)?["detail"]
                .as_str()
                .map(str::to_owned);
            receipt.phase = if operation.operation.is_rejected() {
                "failed"
            } else {
                "waiting_for_ready"
            }
            .into();
            store.save_receipt(&receipt).map_err(problem)?;
            Ok(Json(receipt))
        })
        .await
}
async fn read_receipt(runtime: WebRuntime, id: String) -> Result<Json<Receipt>, ApiProblem> {
    let control = runtime.plugin_control()?;
    let store = Store::open(&control).map_err(problem)?;
    let mut receipt = store
        .receipt(&id)
        .map_err(problem)?
        .ok_or_else(|| ApiProblem::not_found("installation operation not found"))?;
    if matches!(
        receipt.phase.as_str(),
        "applying" | "waiting_for_ready" | "recovery_required" | "succeeded"
    ) {
        if let Some(operation_id) = &receipt.runtime_operation_id {
            if let Ok(Json(operation)) =
                read_plugin_operation(runtime.clone(), operation_id.clone()).await
            {
                let value = serde_json::to_value(operation.operation).map_err(problem)?;

                match value["status"].as_str() {
                    Some("switched") => receipt.phase = "succeeded".into(),
                    Some("rejected" | "rolled_back") => {
                        receipt.phase = "failed".into();
                        receipt.detail = value["detail"].as_str().map(str::to_owned);
                    }
                    _ => {}
                }
            } else {
                receipt.phase = "recovery_required".into();
                receipt.detail = Some(
                    "Runtime receipt is unavailable; checking the active App revision.".into(),
                );
            }
        }
        // After a disconnect/restart, the durable active Generation can prove
        // success. Unknown outcomes never trigger another installation.
        let Json(inventory) =
            plugin_inventory(State(runtime), Query(PluginInventoryQuery { after: None })).await?;
        let value = serde_json::to_value(inventory).map_err(problem)?;

        if value["desired"]["pluginRootRevision"].as_str() == Some(&receipt.candidate_revision)
            && let Some(event) = value["events"].as_array().and_then(|events| {
                events.iter().rev().find(|event| {
                    event["status"] == "watch_degraded"
                        && event["cursor"]
                            .as_str()
                            .and_then(|cursor| cursor.parse::<u64>().ok())
                            .is_some_and(|cursor| cursor > receipt.accepted_cursor)
                        && event["detail"].as_str().is_some_and(|detail| {
                            detail.starts_with(
                                "Generation Controller could not activate the candidate:",
                            )
                        })
                })
            })
            && event["status"] == "watch_degraded"
            && event["detail"].as_str().is_some_and(|detail| {
                detail.starts_with("Generation Controller could not activate the candidate:")
            })
        {
            receipt.phase = "recovery_required".into();
            receipt.detail = event["detail"].as_str().map(str::to_owned);
        }
        if value["active"]["pluginRootRevision"].as_str() == Some(&receipt.candidate_revision) {
            receipt.phase = "succeeded".into();
            receipt.detail = None;
        } else if receipt.phase == "applying" {
            receipt.phase = "recovery_required".into();
            receipt.detail =
                Some("Publication was interrupted. Inspect target state before retrying.".into());
        }
        store.save_receipt(&receipt).map_err(problem)?;
    }
    Ok(Json(receipt))
}

fn entry_id(catalog_id: &str, release: &catalog::Release) -> String {
    format!(
        "market:{}",
        catalog::digest(
            format!("{catalog_id}\n{}\n{}", release.plugin_id, release.version).as_bytes()
        )
    )
}

fn refresh(store: &mut Store) -> Result<Vec<(String, String, catalog::VerifiedSnapshot)>> {
    use std::io::Read;
    let sources = store
        .policy
        .catalogs
        .iter()
        .map(|c| (c.catalog_id.clone(), c.snapshot_url.clone()))
        .collect::<Vec<_>>();
    let mut snapshots = Vec::new();
    for (id, url) in sources {
        let envelope = if let Some(url) = url {
            let url = reqwest::Url::parse(&url)?;
            let local = url.host_str().is_some_and(|host| {
                host == "localhost"
                    || host
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
            });
            ensure!(
                url.username().is_empty()
                    && url.password().is_none()
                    && url.fragment().is_none()
                    && (url.scheme() == "https" || (url.scheme() == "http" && local)),
                "catalog requires HTTPS or an explicitly configured loopback source"
            );
            let client = reqwest::blocking::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .timeout(Duration::from_secs(30))
                .build()?;
            let response = client
                .get(url)
                .header("Accept-Encoding", "identity")
                .send()?;
            ensure!(
                response.status() == reqwest::StatusCode::OK,
                "catalog did not return HTTP 200"
            );
            let mut bytes = Vec::new();
            response
                .take(catalog::MAX_ENVELOPE_BYTES as u64 + 1)
                .read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() <= catalog::MAX_ENVELOPE_BYTES,
                "catalog exceeds bounds"
            );
            String::from_utf8(bytes)?
        } else {
            // Only in-process signed fixtures can omit transport configuration.
            ensure!(cfg!(test), "configured catalog requires snapshot_url");
            store
                .connection
                .query_row("SELECT envelope FROM catalogs WHERE id=?1", [&id], |r| {
                    r.get(0)
                })?
        };
        let verified = store.accept(&id, &envelope)?;
        snapshots.push((id, envelope, verified));
    }
    Ok(snapshots)
}

pub(super) fn entries(
    control: &PluginControl,
    query: &str,
) -> Result<Vec<super::TrustedPluginCatalogEntry>, String> {
    if Policy::load(&control.authority_home, OFFICIAL_POLICY)
        .map_err(|error| error.to_string())?
        .is_none()
    {
        return Ok(Vec::new());
    }
    (|| -> Result<_> {
        let mut store = Store::open(control)?;
        let query = query.to_lowercase();
        let mut entries = Vec::new();
        for (id, _, snapshot) in refresh(&mut store)? {
            for release in &snapshot.snapshot().releases {
                if snapshot
                    .select(&release.plugin_id, &release.version, now())
                    .is_err()
                {
                    continue;
                }
                if !query.is_empty()
                    && !format!(
                        "{} {} {}",
                        release.plugin_id, release.title, release.summary
                    )
                    .to_lowercase()
                    .contains(&query)
                {
                    continue;
                }
                entries.push(super::TrustedPluginCatalogEntry {
                    catalog_entry_id: entry_id(&id, release),
                    package_id: release.plugin_id.clone(),
                    package_revision: release.version.clone(),
                    source_digest: release.artifact.digest.clone(),
                });
            }
        }
        ensure!(entries.len() <= 256, "too many releases; narrow the search");
        Ok(entries)
    })()
    .map_err(|e| e.to_string())
}

pub(super) fn propose_tool(
    control: &PluginControl,
    entry: &str,
    base: &str,
) -> Result<super::PluginInstallProposalResponse, String> {
    (|| -> Result<_> {
        let mut store = Store::open(control)?;
        for (id, envelope, snapshot) in refresh(&mut store)? {
            for release in &snapshot.snapshot().releases {
                if entry_id(&id, release) != entry {
                    continue;
                }
                let proposal = prepare(
                    control,
                    PrepareRequest {
                        target_id: store.target_id.clone(),
                        base_revision: base.into(),
                        catalog_id: id.clone(),
                        envelope: envelope.clone(),
                        plugin_id: release.plugin_id.clone(),
                        version: release.version.clone(),
                        configuration: empty_object(),
                    },
                )?;
                return Ok(super::PluginInstallProposalResponse {
                    authority: super::PluginControl::lifecycle_authority(),
                    base_revision: proposal.base_revision,
                    candidate_revision: proposal.candidate_revision,
                    catalog_entry_id: entry.into(),
                    package_id: release.plugin_id.clone(),
                    package_revision: release.version.clone(),
                    proposal_digest: proposal.digest,
                    source_digest: release.artifact.digest.clone(),
                    schema: "lenso.agent.plugin-install-proposal.v1",
                });
            }
        }
        anyhow::bail!("signed catalog entry not found")
    })()
    .map_err(|e| e.to_string())
}

fn reviewed(store: &Store, digest: &str) -> Result<Proposal> {
    let body: String = store.connection.query_row(
        "SELECT body FROM proposals WHERE json_extract(body,'$.digest')=?1",
        [digest],
        |r| r.get(0),
    )?;
    Ok(serde_json::from_str(&body)?)
}

pub(super) async fn publish_tool(
    runtime: WebRuntime,
    request: super::target_contract::PublishInstallRequest,
) -> Result<super::target_contract::PublishInstallResponse, String> {
    let control = runtime.plugin_control().map_err(|e| e.detail)?;
    let entry = request.catalog_entry_id.clone();
    let base = request.expected_revision.clone();
    let digest = request.proposal_digest.clone();
    let (proposal, envelope) = tokio::task::spawn_blocking(move || -> Result<_> {
        let mut store = Store::open(&control)?;
        let proposal = reviewed(&store, &digest)?;
        ensure!(
            proposal.base_revision == base
                && entry_id(&proposal.catalog_id, &proposal.release) == entry,
            "installation does not match reviewed target and release"
        );
        // A retry reads its retained receipt; it cannot publish again after expiry.
        if store.receipt(&proposal.id)?.is_some() {
            return Ok((proposal, String::new()));
        }
        let envelope = refresh(&mut store)?
            .into_iter()
            .find(|(id, _, _)| id == &proposal.catalog_id)
            .context("catalog no longer configured")?
            .1;
        Ok((proposal, envelope))
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;
    let Json(inventory) = plugin_inventory(
        State(runtime.clone()),
        Query(PluginInventoryQuery { after: None }),
    )
    .await
    .map_err(|e| e.detail)?;
    let Json(receipt) = apply(
        runtime,
        ApplyRequest {
            target_id: proposal.target_id.clone(),
            proposal_id: proposal.id.clone(),
            proposal_digest: proposal.digest.clone(),
            idempotency_key: proposal.id.clone(),
            expected_stream_id: inventory.stream_id,
            envelope,
        },
    )
    .await
    .map_err(|e| e.detail)?;
    if receipt.phase == "failed" {
        return Err(receipt
            .detail
            .unwrap_or_else(|| "installation publication failed".into()));
    }
    Ok(super::target_contract::PublishInstallResponse {
        agent_id: request.agent_id,
        authority: super::lifecycle_authority_contract(PluginControl::lifecycle_authority()),
        base_revision: proposal.base_revision,
        catalog_entry_id: request.catalog_entry_id,
        package_id: proposal.release.plugin_id,
        package_revision: proposal.release.version,
        proposal_digest: proposal.digest,
        revision: proposal.candidate_revision,
    })
}

pub(super) async fn status_tool(
    runtime: WebRuntime,
    request: super::target_contract::InstallationRequest,
) -> Result<super::target_contract::InstallationResponse, String> {
    let control = runtime.plugin_control().map_err(|e| e.detail)?;
    let store = Store::open(&control).map_err(|e| e.to_string())?;
    let proposal = reviewed(&store, &request.proposal_digest).map_err(|e| e.to_string())?;
    let Json(receipt) = read_receipt(runtime, proposal.id)
        .await
        .map_err(|e| e.detail)?;
    Ok(super::target_contract::InstallationResponse {
        agent_id: request.agent_id,
        operation_id: receipt.id,
        status: receipt.phase,
        detail: receipt.detail.unwrap_or_default(),
        candidate_revision: receipt.candidate_revision,
    })
}

#[cfg(test)]
mod tests {
    mod remote_acceptance;

    use super::*;
    use std::path::Path;

    fn bundled_policy() -> String {
        serde_json::json!({
            "name": "Official Marketplace",
            "catalogs": [{"catalog_id":"official", "key_id":"release", "public_key_hex":"00".repeat(32), "snapshot_url":"https://marketplace.example/api/marketplace/v1/snapshot"}],
            "artifact_origins":["https://marketplace.example"]
        }).to_string()
    }

    #[test]
    fn bundled_marketplace_requires_no_user_policy_file() {
        let root = tempfile::tempdir().unwrap();
        let policy = Policy::load(root.path(), Some(&bundled_policy()))
            .unwrap()
            .unwrap();
        assert_eq!(policy.catalogs[0].catalog_id, "official");
        assert!(!root.path().join(".lenso").exists());
        assert!(Policy::load(root.path(), None).unwrap().is_none());
        assert!(Policy::load(root.path(), Some("")).unwrap().is_none());
    }

    #[test]
    fn explicit_operator_policy_is_not_replaced_by_bundled_default() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join(".lenso")).unwrap();
        let path = root.path().join(".lenso/marketplace-policy.json");
        let custom = bundled_policy().replace("official", "private");
        fs::write(&path, custom).unwrap();
        assert_eq!(
            Policy::load(root.path(), Some(&bundled_policy()))
                .unwrap()
                .unwrap()
                .catalogs[0]
                .catalog_id,
            "private"
        );
        fs::write(&path, "invalid json").unwrap();
        assert!(Policy::load(root.path(), Some(&bundled_policy())).is_err());
    }

    #[test]
    fn invalid_bundled_marketplace_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        assert!(Policy::load(root.path(), Some("not json")).is_err());
        assert!(Policy::load(root.path(), Some(&"x".repeat(32769))).is_err());
        assert!(
            Policy::load(
                root.path(),
                Some(&bundled_policy().replace("https://marketplace.example", "http://localhost"))
            )
            .is_err()
        );
    }

    // The former HTTP-only proof missed the Host bridge rejecting local Console
    // catalog calls. Exercise the public tool provider and its real target wiring.
    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires real Process archives in LENSO_MARKETPLACE_*_ARCHIVE"]
    async fn console_tools_install_signed_release_and_observe_runtime() {
        console_tools_install_for_authority(false).await;
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires real Process archives in LENSO_MARKETPLACE_*_ARCHIVE"]
    async fn console_tools_install_signed_release_and_observe_runtime_sqlite() {
        console_tools_install_for_authority(true).await;
    }

    async fn console_tools_install_for_authority(sqlite: bool) {
        let root = tempfile::tempdir().unwrap();
        crate::configure_test_fixture_model(root.path());
        let approval = root
            .path()
            .join("plugins/lenso.agent.interactive-approval-hook");
        fs::create_dir_all(&approval).unwrap();
        fs::write(
            approval.join("default.toml"),
            "default_decision = \"allow\"\nask_tools = []\nmax_preview_bytes = 4096\n",
        )
        .unwrap();
        let configuration = || {
            let mut config = crate::AgentWebConfig::new(lenso_agent_console_plugins::link);
            config.agent_home = Some(root.path().to_path_buf());
            config.control = crate::AgentWebControl::HostAuthorized;
            config.plugin_control = true;
            config.tool_policy = Some(root.path().join("tool-policy.json"));
            if sqlite {
                config.plugin_configuration_store =
                    Some(crate::PluginConfigurationStoreConfig::new(
                        root.path().join("configuration.sqlite3"),
                        "test/console",
                    ));
            }
            config
        };
        tokio::task::LocalSet::new().run_until(async {
            let surface = crate::AgentWebSurface::start(configuration()).await.unwrap();
            let runtime = surface.runtime.clone();
            let control = runtime.plugin_control().unwrap();
            let archive = PathBuf::from(std::env::var("LENSO_MARKETPLACE_ARCHIVE").unwrap());
            let (_, envelope) = seed(&control, &archive, 1);
            let source = std::sync::Arc::new(std::sync::RwLock::new(envelope));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let source_url = format!("http://{}/snapshot",listener.local_addr().unwrap());
            let server = tokio::spawn({
                let source=source.clone();
                async move { axum::serve(listener, axum::Router::new().route("/snapshot",axum::routing::get(move || { let source=source.clone(); async move { source.read().unwrap().clone() } }))).await.unwrap(); }
            });
            set_source(&control,&source_url);
            let catalog = tool(&runtime, "list_available_plugins", serde_json::json!({"agent_id":"console"})).await;
            let entry = catalog["entries"][0]["catalog_entry_id"].as_str().unwrap();
            let proposal = tool(&runtime, "check_plugin_install", serde_json::json!({"agent_id":"console","catalog_entry_id":entry,"expected_revision":catalog["revision"]})).await;
            {
                let store=Store::open(&control).unwrap();
                store.connection.execute("UPDATE proposals SET body=json_set(body,'$.expires_at',0) WHERE json_extract(body,'$.digest')=?1",[proposal["proposal_digest"].as_str().unwrap()]).unwrap();
            }
            let expired=raw_tool(&runtime,"apply_plugin_install",serde_json::json!({"agent_id":"console","catalog_entry_id":entry,"expected_revision":catalog["revision"],"proposal_digest":proposal["proposal_digest"]})).await;
            assert_ne!(expired["status"],"succeeded","{expired}");
            assert!(previous_version(&control,"lenso.marketplace.proof").unwrap().is_none());
            let proposal = tool(&runtime, "check_plugin_install", serde_json::json!({"agent_id":"console","catalog_entry_id":entry,"expected_revision":catalog["revision"]})).await;
            assert!(previous_version(&control,"lenso.marketplace.proof").unwrap().is_none());
            let apply_args = serde_json::json!({"agent_id":"console","catalog_entry_id":entry,"expected_revision":catalog["revision"],"proposal_digest":proposal["proposal_digest"]});
            let mut wrong = apply_args.clone(); wrong["agent_id"] = "missing-target".into();
            assert_ne!(raw_tool(&runtime,"apply_plugin_install",wrong).await["status"],"succeeded");
            let mut wrong = apply_args.clone(); wrong["proposal_digest"] = format!("sha256:{}","0".repeat(64)).into();
            assert_ne!(raw_tool(&runtime,"apply_plugin_install",wrong).await["status"],"succeeded");
            tool(&runtime,"apply_plugin_install",apply_args.clone()).await;
            let digest = proposal["proposal_digest"].as_str().unwrap();
            let receipt = wait_tool(&runtime,digest).await;
            assert_eq!(receipt["status"],"succeeded","{receipt}");
            tool(&runtime,"apply_plugin_install",apply_args).await;
            let retry = tool(&runtime,"get_plugin_installation",serde_json::json!({"agent_id":"console","proposal_digest":digest})).await;
            assert_eq!(receipt["operation_id"],retry["operation_id"]);
            assert_eq!(tool(&runtime,"marketplace_proof",serde_json::json!({"text":"installation proof"})).await, "installation proof");
            surface.shutdown().await.unwrap();
            let surface = crate::AgentWebSurface::start(configuration()).await.unwrap();
            let runtime = surface.runtime.clone();
            let receipt = wait_tool(&runtime,digest).await;
            assert_eq!(receipt["status"],"succeeded","{receipt}");
            let control = runtime.plugin_control().unwrap();
            for (revision, variable, expected) in [(2,"LENSO_MARKETPLACE_UPDATE_ARCHIVE","succeeded"),(3,"LENSO_MARKETPLACE_FAILURE_ARCHIVE","recovery_required")] {
                let (_, envelope) = seed(&control, &PathBuf::from(std::env::var(variable).unwrap()), revision);
                *source.write().unwrap()=envelope;
                set_source(&control,&source_url);
                let catalog = tool(&runtime,"list_available_plugins",serde_json::json!({"agent_id":"console"})).await;
                let entry = &catalog["entries"][0]["catalog_entry_id"];
                let proposal = tool(&runtime,"check_plugin_install",serde_json::json!({"agent_id":"console","catalog_entry_id":entry,"expected_revision":catalog["revision"]})).await;
                let args=serde_json::json!({"agent_id":"console","catalog_entry_id":entry,"expected_revision":catalog["revision"],"proposal_digest":proposal["proposal_digest"]});
                tool(&runtime,"apply_plugin_install",args).await;
                let receipt=wait_tool(&runtime,proposal["proposal_digest"].as_str().unwrap()).await;
                assert_eq!(receipt["status"],expected,"{receipt}");
                assert_eq!(tool(&runtime,"marketplace_proof",serde_json::json!({"text":"installation proof"})).await,"v2: installation proof");
            }
            let inspected=tool(&runtime,"inspect_app",serde_json::json!({"agent_id":"console"})).await;
            let removal=tool(&runtime,"check_plugin_removal",serde_json::json!({"agent_id":"console","plugin_id":"lenso.marketplace.proof","expected_revision":inspected["revision"]})).await;
            tool(&runtime,"apply_plugin_removal",serde_json::json!({"agent_id":"console","plugin_id":"lenso.marketplace.proof","expected_revision":inspected["revision"],"proposal_digest":removal["proposal_digest"]})).await;
            assert!(previous_version(&control,"lenso.marketplace.proof").unwrap().is_none());
            server.abort();
            surface.shutdown().await.unwrap();
            let surface = crate::AgentWebSurface::start(configuration()).await.unwrap();
            let control = surface.runtime.plugin_control().unwrap();
            assert!(previous_version(&control,"lenso.marketplace.proof").unwrap().is_none());
            assert!(control.configuration_authority.inspect().is_ok());
            surface.shutdown().await.unwrap();
        }).await;
    }

    fn set_source(control: &PluginControl, url: &str) {
        let path = control
            .authority_home
            .join(".lenso/marketplace-policy.json");
        let mut policy: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        policy["catalogs"][0]["snapshot_url"] = url.into();
        fs::write(path, serde_json::to_vec(&policy).unwrap()).unwrap();
    }
    async fn raw_tool(runtime: &WebRuntime, name: &str, arguments: Value) -> Value {
        tokio::time::timeout(
            Duration::from_secs(20),
            raw_tool_inner(runtime, name, arguments),
        )
        .await
        .expect(name)
    }
    async fn raw_tool_inner(runtime: &WebRuntime, name: &str, arguments: Value) -> Value {
        eprintln!("tool proof: {name} start");
        let Json(policy) = crate::read_tool_policy(State(runtime.clone()), HeaderMap::new())
            .await
            .unwrap();
        if !policy.allowed.iter().any(|tool| tool == name) {
            let mut allowed = policy.allowed;
            allowed.push(name.into());
            let _ = crate::update_tool_policy(
                State(runtime.clone()),
                HeaderMap::new(),
                Json(crate::UpdateToolPolicyRequest {
                    allowed,
                    expected_revision: policy.revision,
                }),
            )
            .await
            .unwrap();
        }
        eprintln!("tool proof: {name} policy ready");
        let Json(tools) = crate::agent_tool_catalog(State(runtime.clone()))
            .await
            .unwrap();
        eprintln!("tool proof: {name} executing {arguments}");
        let Json(result) = crate::execute_agent_tool(
            State(runtime.clone()),
            Json(crate::AgentToolExecuteRequest {
                arguments_json: arguments.to_string().try_into().unwrap(),
                expected_generation: tools.generation,
                name: name.into(),
            }),
        )
        .await
        .unwrap();
        eprintln!("tool proof: {name} finished");
        serde_json::to_value(result).unwrap()
    }
    async fn tool(runtime: &WebRuntime, name: &str, arguments: Value) -> Value {
        let result = raw_tool(runtime, name, arguments).await;
        assert_eq!(result["status"], "succeeded", "{name}: {result}");
        let content = result["response"]["content"].as_str().unwrap();
        serde_json::from_str(content).unwrap_or_else(|_| content.into())
    }
    async fn wait_tool(runtime: &WebRuntime, digest: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(45), async {
            loop {
                let result = tool(
                    runtime,
                    "get_plugin_installation",
                    serde_json::json!({"agent_id":"console","proposal_digest":digest}),
                )
                .await;
                if matches!(
                    result["status"].as_str(),
                    Some("succeeded" | "failed" | "recovery_required")
                ) {
                    return result;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap()
    }
    fn seed(
        control: &PluginControl,
        archive_path: &Path,
        revision: u64,
    ) -> (catalog::Release, String) {
        let bytes = fs::read(archive_path).unwrap();
        let archive = VerifiedPluginArchive::read(
            bytes.as_slice(),
            &PluginArchiveIdentity {
                size: bytes.len() as u64,
                sha256: catalog::digest(&bytes),
            },
        )
        .unwrap();
        let bundle = archive.bundle();
        let release = catalog::Release {
            plugin_id: bundle.plugin_id.clone(),
            version: bundle.release_version.clone(),
            publisher_id: "fixture".into(),
            title: "Installation proof".into(),
            summary: "Real Process plugin fixture".into(),
            description: String::new(),
            presentation: None,
            source_url: "https://example.test/source".into(),
            source_revision: "a".repeat(40),
            license: "MIT".into(),
            artifact: catalog::Artifact {
                url: "https://example.test/proof.lenso-plugin".into(),
                digest: catalog::digest(&bytes),
                size: bytes.len() as u64,
                manifest_digest: bundle.manifest_digest.clone(),
            },
            availability: catalog::Availability::Listed,
        };
        let key = ed25519_dalek::SigningKey::from_bytes(&[17; 32]);
        fs::write(control.authority_home.join(".lenso/marketplace-policy.json"), serde_json::to_vec(&serde_json::json!({"name":"Installation proof Agent","catalogs":[{"catalog_id":"local-fixture","key_id":"test-key","public_key_hex":hex::encode(key.verifying_key().as_bytes())}],"artifact_origins":["https://example.test"]})).unwrap()).unwrap();
        let mut store = Store::open(control).unwrap();
        fs::write(store.artifact(&release), bytes).unwrap();
        let envelope = String::from_utf8(
            catalog::sign(
                &catalog::Snapshot {
                    schema: "lenso.marketplace.snapshot.v1".into(),
                    catalog_id: "local-fixture".into(),
                    revision,
                    issued_at: now(),
                    expires_at: now() + 3600,
                    releases: vec![release.clone()],
                },
                "test-key",
                &key,
            )
            .unwrap(),
        )
        .unwrap();
        store.accept("local-fixture", &envelope).unwrap();
        (release, envelope)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn marketplace_authorization_precedes_storage_and_checkpoints_survive_reopen() {
        let root = tempfile::tempdir().unwrap();
        crate::configure_test_fixture_model(root.path());
        let mut config = crate::AgentWebConfig::new(lenso_agent_console_plugins::link);
        config.agent_home = Some(root.path().to_path_buf());
        config.control = crate::AgentWebControl::Bearer("test-control".into());
        config.plugin_control = true;
        tokio::task::LocalSet::new().run_until(async {
            let surface = crate::AgentWebSurface::start(config).await.unwrap();
            assert!(target(State(surface.runtime.clone()),HeaderMap::new()).await.is_err());
            assert!(!root.path().join(".lenso/marketplace").exists());
            let control = surface.runtime.plugin_control().unwrap();
            let key = ed25519_dalek::SigningKey::from_bytes(&[17;32]);
            fs::write(root.path().join(".lenso/marketplace-policy.json"), serde_json::to_vec(&serde_json::json!({"name":"Fixture","catalogs":[{"catalog_id":"fixture","key_id":"key","public_key_hex":hex::encode(key.verifying_key().as_bytes())}],"artifact_origins":["https://example.test"]})).unwrap()).unwrap();
            let envelope = |revision, expired| {
                let snapshot = catalog::Snapshot { schema:"lenso.marketplace.snapshot.v1".into(),catalog_id:"fixture".into(),revision,issued_at:now()-100,expires_at:if expired {now()-1}else{now()+3600},releases:vec![] };
                String::from_utf8(catalog::sign(&snapshot,"key",&key).unwrap()).unwrap()
            };
            let mut store = Store::open(&control).unwrap();
            let identity = store.target_id.clone();
            store.accept("fixture",&envelope(2,false)).unwrap();
            drop(store);
            let mut reopened = Store::open(&control).unwrap();
            assert_eq!(reopened.target_id,identity);
            assert!(reopened.accept("fixture",&envelope(1,false)).is_err());
            assert!(reopened.accept("fixture",&envelope(3,true)).is_err());
            assert!(reopened.accept("untrusted",&envelope(3,false)).is_err());
            let checkpoint:String = reopened.connection.query_row("SELECT checkpoint FROM catalogs WHERE id='fixture'",[],|row|row.get(0)).unwrap();
            assert_eq!(serde_json::from_str::<catalog::Checkpoint>(&checkpoint).unwrap().revision,2);
            surface.shutdown().await.unwrap();
        }).await;
    }
}
