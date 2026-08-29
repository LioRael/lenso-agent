use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use fs2::FileExt;
use lenso_app_authoring::{
    LocalPluginRootAuthority, PluginConfigurationAuthority, PluginConfigurationAuthoritySource,
    PluginConfigurationProposal, PluginConfigurationProposalStatus, PluginConfigurationPublication,
    PluginRootAuthoringState, PluginRootRevision,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

const STORE_SCHEMA: &str = "lenso.plugin-configuration-store.v1";

/// Host-owned settings for one durable managed Plugin configuration authority.
#[derive(Clone, Debug)]
pub struct PluginConfigurationStoreConfig {
    pub database: PathBuf,
    pub reference: String,
}

impl PluginConfigurationStoreConfig {
    pub fn new(database: impl Into<PathBuf>, reference: impl Into<String>) -> Self {
        Self {
            database: database.into(),
            reference: reference.into(),
        }
    }
}

/// SQLite-backed compare-and-swap authority for one materialized Plugin Root.
///
/// Proposals are durable review records. Publication first commits one exact
/// materialization intent, then delegates the atomic Plugin Root replacement to
/// the local authoring implementation, and finally advances durable desired
/// state. A process crash leaves enough evidence for deterministic recovery.
#[derive(Debug)]
pub struct SqlitePluginConfigurationAuthority {
    database: PathBuf,
    local: LocalPluginRootAuthority,
    operation_lock: PathBuf,
    source: PluginConfigurationAuthoritySource,
    access: Mutex<()>,
}

impl SqlitePluginConfigurationAuthority {
    pub fn open(
        root: impl Into<PathBuf>,
        config: PluginConfigurationStoreConfig,
    ) -> anyhow::Result<Self> {
        validate_database_path(&config.database)?;
        let source = PluginConfigurationAuthoritySource::new(
            "sqlite_configuration_store",
            config.reference,
        )?;
        let operation_lock = config.database.with_extension("sqlite3.lock");
        let authority = Self {
            database: config.database,
            local: LocalPluginRootAuthority::new(root),
            operation_lock,
            source,
            access: Mutex::new(()),
        };
        authority.with_operation(|connection| {
            authority.reconcile(connection)?;
            Ok(())
        })?;
        Ok(authority)
    }

    pub fn database(&self) -> &Path {
        &self.database
    }

    fn with_operation<T>(
        &self,
        operation: impl FnOnce(&mut Connection) -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let _guard = self
            .access
            .lock()
            .map_err(|_| anyhow::anyhow!("Plugin configuration store lock is poisoned"))?;
        let _lease = OperationLease::acquire(&self.operation_lock)?;
        let mut connection = open_database(&self.database)?;
        initialize_schema(&connection, self.source.reference())?;
        operation(&mut connection)
    }

    fn reconcile(&self, connection: &mut Connection) -> anyhow::Result<PluginRootAuthoringState> {
        let materialized = self.local.inspect()?;
        let materialized_revision = materialized.revision().as_str();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("begin Plugin configuration store reconciliation")?;
        let desired_revision = desired_revision(&transaction)?;
        let Some(desired_revision) = desired_revision else {
            transaction.execute(
                "INSERT INTO authority_state(singleton, schema, authority_reference, desired_revision) VALUES (1, ?1, ?2, ?3)",
                params![STORE_SCHEMA, self.source.reference(), materialized_revision],
            )?;
            transaction.commit()?;
            return Ok(materialized);
        };

        let materializing = materializing_proposals(&transaction)?;
        if desired_revision == materialized_revision {
            match materializing.as_slice() {
                [] => {}
                [proposal] if proposal.candidate_revision == materialized_revision => {
                    finalize_publication(&transaction, proposal, materialized_revision)?;
                }
                [proposal] if proposal.base_revision == materialized_revision => {
                    transaction.execute(
                        "UPDATE configuration_proposals SET phase = 'proposed' WHERE proposal_digest = ?1 AND phase = 'materializing'",
                        [&proposal.proposal_digest],
                    )?;
                }
                _ => bail!("Plugin configuration store has ambiguous materialization intent"),
            }
        } else {
            match materializing.as_slice() {
                [proposal]
                    if proposal.base_revision == desired_revision
                        && proposal.candidate_revision == materialized_revision =>
                {
                    finalize_publication(&transaction, proposal, materialized_revision)?;
                }
                _ => bail!(
                    "materialized Plugin Root revision {materialized_revision} diverges from managed desired revision {desired_revision}"
                ),
            }
        }
        transaction.commit()?;
        Ok(materialized)
    }

    fn persist_proposal(
        connection: &mut Connection,
        proposal: &PluginConfigurationProposal,
        bytes: &[u8],
    ) -> anyhow::Result<()> {
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("begin durable Plugin configuration proposal")?;
        let current = desired_revision(&transaction)?
            .context("Plugin configuration store is not initialized")?;
        if current != proposal.base_revision().as_str() {
            bail!(
                "Plugin configuration store revision conflict: expected {}, current {current}",
                proposal.base_revision()
            );
        }
        if materializing_proposals(&transaction)?.is_empty() {
            let review_status = match proposal.status() {
                PluginConfigurationProposalStatus::Ready => "ready",
                PluginConfigurationProposalStatus::NeedsDecision => "needs_decision",
                PluginConfigurationProposalStatus::Rejected => "rejected",
            };
            transaction.execute(
                "INSERT OR IGNORE INTO configuration_proposals(
                    proposal_digest, base_revision, candidate_revision, plugin_id,
                    instance_key, configuration_toml, review_status, phase
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'proposed')",
                params![
                    proposal.digest(),
                    proposal.base_revision().as_str(),
                    proposal.candidate_revision().as_str(),
                    proposal.plugin_id(),
                    proposal.instance_key(),
                    bytes,
                    review_status,
                ],
            )?;
            verify_stored_proposal(&transaction, proposal, bytes)?;
            transaction.commit()?;
            return Ok(());
        }
        bail!("Plugin configuration publication is already materializing")
    }

    fn mark_materializing(
        connection: &mut Connection,
        proposal: &PluginConfigurationProposal,
    ) -> anyhow::Result<()> {
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("begin Plugin configuration publication CAS")?;
        let current = desired_revision(&transaction)?
            .context("Plugin configuration store is not initialized")?;
        if current != proposal.base_revision().as_str() {
            bail!(
                "Plugin configuration store revision conflict: expected {}, current {current}",
                proposal.base_revision()
            );
        }
        let bytes = stored_proposal_bytes(&transaction, proposal.digest())?
            .context("reviewed Plugin configuration proposal is not durable")?;
        verify_stored_proposal(&transaction, proposal, &bytes)?;
        let changed = transaction.execute(
            "UPDATE configuration_proposals
             SET phase = 'materializing'
             WHERE proposal_digest = ?1 AND phase = 'proposed' AND review_status = 'ready'",
            [proposal.digest()],
        )?;
        if changed != 1 {
            bail!("Plugin configuration proposal is not ready for publication");
        }
        transaction.commit()?;
        Ok(())
    }

    fn finish_publication(
        &self,
        connection: &mut Connection,
        publication: &PluginConfigurationPublication,
    ) -> anyhow::Result<()> {
        let materialized = self.local.inspect()?;
        if materialized.revision() != publication.revision() {
            bail!(
                "materialized Plugin Root revision {} does not match published revision {}",
                materialized.revision(),
                publication.revision()
            );
        }
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("finish Plugin configuration publication")?;
        let proposal = materializing_proposals(&transaction)?;
        let [proposal] = proposal.as_slice() else {
            bail!("Plugin configuration store lost its materialization intent");
        };
        if proposal.proposal_digest != publication.proposal_digest()
            || proposal.base_revision != publication.base_revision().as_str()
            || proposal.candidate_revision != publication.revision().as_str()
        {
            bail!("Plugin configuration publication does not match durable intent");
        }
        finalize_publication(&transaction, proposal, publication.revision().as_str())?;
        transaction.commit()?;
        Ok(())
    }
}

impl PluginConfigurationAuthority for SqlitePluginConfigurationAuthority {
    fn source(&self) -> PluginConfigurationAuthoritySource {
        self.source.clone()
    }

    fn inspect(&self) -> anyhow::Result<PluginRootAuthoringState> {
        self.with_operation(|connection| self.reconcile(connection))
    }

    fn propose(
        &self,
        expected_revision: &PluginRootRevision,
        plugin_id: &str,
        instance: &str,
        bytes: &[u8],
    ) -> anyhow::Result<PluginConfigurationProposal> {
        self.with_operation(|connection| {
            let materialized = self.reconcile(connection)?;
            if materialized.revision() != expected_revision {
                bail!(
                    "Plugin configuration store revision conflict: expected {expected_revision}, current {}",
                    materialized.revision()
                );
            }
            let proposal = self
                .local
                .propose(expected_revision, plugin_id, instance, bytes)?;
            Self::persist_proposal(connection, &proposal, bytes)?;
            Ok(proposal)
        })
    }

    fn publish(
        &self,
        proposal: &PluginConfigurationProposal,
    ) -> anyhow::Result<PluginConfigurationPublication> {
        self.with_operation(|connection| {
            self.reconcile(connection)?;
            Self::mark_materializing(connection, proposal)?;
            let publication = match self.local.publish(proposal) {
                Ok(publication) => publication,
                Err(error) => {
                    let materialized = self.local.inspect()?;
                    if materialized.revision() == proposal.base_revision() {
                        connection.execute(
                            "UPDATE configuration_proposals SET phase = 'proposed' WHERE proposal_digest = ?1 AND phase = 'materializing'",
                            [proposal.digest()],
                        )?;
                    }
                    return Err(error);
                }
            };
            self.finish_publication(connection, &publication)?;
            Ok(publication)
        })
    }
}

#[derive(Debug)]
struct OperationLease(File);

impl OperationLease {
    fn acquire(path: &Path) -> anyhow::Result<Self> {
        let parent = path
            .parent()
            .context("Plugin configuration operation lock has no parent")?;
        fs::create_dir_all(parent)?;
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .with_context(|| format!("open Plugin configuration lock {}", path.display()))?;
        file.try_lock_exclusive().with_context(|| {
            format!(
                "Plugin configuration authority is busy for {}",
                path.display()
            )
        })?;
        Ok(Self(file))
    }
}

impl Drop for OperationLease {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[derive(Clone, Debug)]
struct StoredProposal {
    proposal_digest: String,
    base_revision: String,
    candidate_revision: String,
    plugin_id: String,
    instance_key: String,
    configuration_toml: Vec<u8>,
}

fn validate_database_path(path: &Path) -> anyhow::Result<()> {
    if !path.is_absolute() {
        bail!(
            "Plugin configuration store path must be absolute: {}",
            path.display()
        );
    }
    if path.to_str().is_none() {
        bail!(
            "Plugin configuration store path must be valid UTF-8: {}",
            path.display()
        );
    }
    Ok(())
}

fn open_database(path: &Path) -> anyhow::Result<Connection> {
    let parent = path
        .parent()
        .context("Plugin configuration store has no parent")?;
    fs::create_dir_all(parent)?;
    let connection = Connection::open(path)
        .with_context(|| format!("open Plugin configuration store {}", path.display()))?;
    connection.busy_timeout(std::time::Duration::from_secs(2))?;
    connection.pragma_update(None, "foreign_keys", true)?;
    Ok(connection)
}

fn initialize_schema(connection: &Connection, reference: &str) -> anyhow::Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS authority_state (
            singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
            schema TEXT NOT NULL,
            authority_reference TEXT NOT NULL,
            desired_revision TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS configuration_proposals (
            proposal_digest TEXT PRIMARY KEY,
            base_revision TEXT NOT NULL,
            candidate_revision TEXT NOT NULL,
            plugin_id TEXT NOT NULL,
            instance_key TEXT NOT NULL,
            configuration_toml BLOB NOT NULL,
            review_status TEXT NOT NULL CHECK (review_status IN ('ready', 'needs_decision', 'rejected')),
            phase TEXT NOT NULL CHECK (phase IN ('proposed', 'materializing', 'published'))
         );
         CREATE TABLE IF NOT EXISTS configuration_publications (
            proposal_digest TEXT PRIMARY KEY,
            revision TEXT NOT NULL,
            base_revision TEXT NOT NULL,
            plugin_id TEXT NOT NULL,
            instance_key TEXT NOT NULL,
            configuration_toml BLOB NOT NULL,
            published_at_unix_ms INTEGER NOT NULL,
            FOREIGN KEY (proposal_digest) REFERENCES configuration_proposals(proposal_digest)
         );",
    )?;
    if let Some((schema, stored)) = connection
        .query_row(
            "SELECT schema, authority_reference FROM authority_state WHERE singleton = 1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
    {
        if schema != STORE_SCHEMA {
            bail!("unsupported Plugin configuration store schema {schema}");
        }
        if stored != reference {
            bail!("Plugin configuration store belongs to authority {stored}, not {reference}");
        }
    }
    Ok(())
}

fn desired_revision(transaction: &Transaction<'_>) -> anyhow::Result<Option<String>> {
    transaction
        .query_row(
            "SELECT desired_revision FROM authority_state WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

fn materializing_proposals(transaction: &Transaction<'_>) -> anyhow::Result<Vec<StoredProposal>> {
    let mut statement = transaction.prepare(
        "SELECT proposal_digest, base_revision, candidate_revision, plugin_id,
                instance_key, configuration_toml
         FROM configuration_proposals WHERE phase = 'materializing'
         ORDER BY proposal_digest",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(StoredProposal {
            proposal_digest: row.get(0)?,
            base_revision: row.get(1)?,
            candidate_revision: row.get(2)?,
            plugin_id: row.get(3)?,
            instance_key: row.get(4)?,
            configuration_toml: row.get(5)?,
        })
    })?;
    rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

fn stored_proposal_bytes(
    transaction: &Transaction<'_>,
    digest: &str,
) -> anyhow::Result<Option<Vec<u8>>> {
    transaction
        .query_row(
            "SELECT configuration_toml FROM configuration_proposals WHERE proposal_digest = ?1",
            [digest],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

fn verify_stored_proposal(
    transaction: &Transaction<'_>,
    proposal: &PluginConfigurationProposal,
    bytes: &[u8],
) -> anyhow::Result<()> {
    let stored = transaction
        .query_row(
            "SELECT proposal_digest, base_revision, candidate_revision, plugin_id,
                    instance_key, configuration_toml
             FROM configuration_proposals WHERE proposal_digest = ?1",
            [proposal.digest()],
            |row| {
                Ok(StoredProposal {
                    proposal_digest: row.get(0)?,
                    base_revision: row.get(1)?,
                    candidate_revision: row.get(2)?,
                    plugin_id: row.get(3)?,
                    instance_key: row.get(4)?,
                    configuration_toml: row.get(5)?,
                })
            },
        )
        .optional()?
        .context("reviewed Plugin configuration proposal is not durable")?;
    if stored.proposal_digest != proposal.digest()
        || stored.base_revision != proposal.base_revision().as_str()
        || stored.candidate_revision != proposal.candidate_revision().as_str()
        || stored.plugin_id != proposal.plugin_id()
        || stored.instance_key != proposal.instance_key()
        || stored.configuration_toml != bytes
    {
        bail!("durable Plugin configuration proposal does not match reviewed evidence");
    }
    Ok(())
}

fn finalize_publication(
    transaction: &Transaction<'_>,
    proposal: &StoredProposal,
    revision: &str,
) -> anyhow::Result<()> {
    let current =
        desired_revision(transaction)?.context("Plugin configuration store is not initialized")?;
    if current != proposal.base_revision {
        bail!(
            "Plugin configuration store revision conflict: expected {}, current {current}",
            proposal.base_revision
        );
    }
    transaction.execute(
        "INSERT OR IGNORE INTO configuration_publications(
            proposal_digest, revision, base_revision, plugin_id, instance_key,
            configuration_toml, published_at_unix_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            proposal.proposal_digest,
            revision,
            proposal.base_revision,
            proposal.plugin_id,
            proposal.instance_key,
            proposal.configuration_toml,
            unix_time_millis()?,
        ],
    )?;
    let changed = transaction.execute(
        "UPDATE authority_state SET desired_revision = ?1 WHERE singleton = 1 AND desired_revision = ?2",
        params![revision, proposal.base_revision],
    )?;
    if changed != 1 {
        bail!("Plugin configuration store lost its compare-and-swap fence");
    }
    transaction.execute(
        "UPDATE configuration_proposals SET phase = 'published' WHERE proposal_digest = ?1 AND phase = 'materializing'",
        [&proposal.proposal_digest],
    )?;
    Ok(())
}

fn unix_time_millis() -> anyhow::Result<i64> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?;
    i64::try_from(duration.as_millis()).context("Unix timestamp exceeds SQLite integer range")
}

#[cfg(test)]
mod tests {
    use super::*;
    use lenso_app_plan::authoring::{
        HostCatalog, HostDefaultPlugin, HostPluginRelease, HostSlot, PluginDescriptor,
    };

    fn fixture_root() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".lenso")).unwrap();
        let descriptor = PluginDescriptor::new("example.agent", "1.0.0", "agent")
            .with_configuration_schema(serde_json::json!({
                "type": "object",
                "properties": {
                    "greeting": { "type": "string" }
                },
                "additionalProperties": false
            }));
        let host = HostCatalog::new(
            [HostSlot::one("agent")],
            [HostPluginRelease::new(descriptor)],
            [HostDefaultPlugin::new("example.agent", "default")],
        );
        fs::write(
            root.path().join(".lenso/host-catalog.json"),
            serde_json::to_vec(&host).unwrap(),
        )
        .unwrap();
        root
    }

    fn open_authority(
        root: &tempfile::TempDir,
        database: &Path,
    ) -> SqlitePluginConfigurationAuthority {
        SqlitePluginConfigurationAuthority::open(
            root.path(),
            PluginConfigurationStoreConfig::new(database, "tenant/app"),
        )
        .unwrap()
    }

    fn proposal(
        authority: &SqlitePluginConfigurationAuthority,
        toml: &[u8],
    ) -> PluginConfigurationProposal {
        let base = authority.inspect().unwrap().revision().clone();
        authority
            .propose(&base, "example.agent", "default", toml)
            .unwrap()
    }

    #[test]
    fn proposal_is_durable_but_does_not_change_desired_state() {
        let root = fixture_root();
        let database = root.path().join("configuration.sqlite3");
        let authority = open_authority(&root, &database);
        let base = authority.inspect().unwrap().revision().clone();
        let proposal = authority
            .propose(&base, "example.agent", "default", b"greeting = \"hello\"\n")
            .unwrap();

        assert_eq!(authority.inspect().unwrap().revision(), &base);
        assert!(
            !root
                .path()
                .join("plugins/example.agent/default.toml")
                .exists()
        );
        let connection = Connection::open(&database).unwrap();
        let stored = connection
            .query_row(
                "SELECT phase, configuration_toml FROM configuration_proposals WHERE proposal_digest = ?1",
                [proposal.digest()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .unwrap();
        assert_eq!(stored.0, "proposed");
        assert_eq!(stored.1, b"greeting = \"hello\"\n");
    }

    #[test]
    fn publication_advances_durable_cas_and_records_history() {
        let root = fixture_root();
        let database = root.path().join("configuration.sqlite3");
        let authority = open_authority(&root, &database);
        let proposal = proposal(&authority, b"greeting = \"hello\"\n");
        let publication = authority.publish(&proposal).unwrap();

        assert_eq!(
            authority.inspect().unwrap().revision(),
            publication.revision()
        );
        assert_eq!(
            fs::read(root.path().join("plugins/example.agent/default.toml")).unwrap(),
            b"greeting = \"hello\"\n"
        );
        let connection = Connection::open(&database).unwrap();
        let stored = connection
            .query_row(
                "SELECT s.desired_revision, p.phase, h.proposal_digest
                 FROM authority_state s
                 JOIN configuration_proposals p ON p.proposal_digest = ?1
                 JOIN configuration_publications h ON h.proposal_digest = p.proposal_digest",
                [proposal.digest()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(stored.0, publication.revision().as_str());
        assert_eq!(stored.1, "published");
        assert_eq!(stored.2, proposal.digest());
    }

    #[test]
    fn restart_completes_one_exact_interrupted_materialization() {
        let root = fixture_root();
        let database = root.path().join("configuration.sqlite3");
        let authority = open_authority(&root, &database);
        let proposal = proposal(&authority, b"greeting = \"recovered\"\n");
        let candidate = proposal.candidate_revision().clone();

        authority
            .with_operation(|connection| {
                authority.reconcile(connection)?;
                SqlitePluginConfigurationAuthority::mark_materializing(connection, &proposal)?;
                authority.local.publish(&proposal)?;
                Ok(())
            })
            .unwrap();
        drop(authority);

        let recovered = open_authority(&root, &database);
        assert_eq!(recovered.inspect().unwrap().revision(), &candidate);
        let connection = Connection::open(&database).unwrap();
        let phase = connection
            .query_row(
                "SELECT phase FROM configuration_proposals WHERE proposal_digest = ?1",
                [proposal.digest()],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        assert_eq!(phase, "published");
    }

    #[test]
    fn unrecorded_plugin_root_change_fails_closed() {
        let root = fixture_root();
        let database = root.path().join("configuration.sqlite3");
        let authority = open_authority(&root, &database);
        let base = authority.inspect().unwrap().revision().clone();
        drop(authority);

        let local = LocalPluginRootAuthority::new(root.path());
        let proposal = local
            .propose(
                &base,
                "example.agent",
                "default",
                b"greeting = \"bypass\"\n",
            )
            .unwrap();
        local.publish(&proposal).unwrap();

        let error = SqlitePluginConfigurationAuthority::open(
            root.path(),
            PluginConfigurationStoreConfig::new(&database, "tenant/app"),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("diverges from managed desired revision")
        );
    }

    #[test]
    fn stale_publication_cannot_overwrite_the_cas_winner() {
        let root = fixture_root();
        let database = root.path().join("configuration.sqlite3");
        let authority = open_authority(&root, &database);
        let base = authority.inspect().unwrap().revision().clone();
        let first = authority
            .propose(&base, "example.agent", "default", b"greeting = \"first\"\n")
            .unwrap();
        let stale = authority
            .propose(&base, "example.agent", "default", b"greeting = \"stale\"\n")
            .unwrap();
        authority.publish(&first).unwrap();

        let error = authority.publish(&stale).unwrap_err();
        assert!(error.to_string().contains("revision conflict"));
        assert_eq!(
            fs::read(root.path().join("plugins/example.agent/default.toml")).unwrap(),
            b"greeting = \"first\"\n"
        );
    }
}
