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
    PluginRootAuthoringState, PluginRootRevision, PluginSelectionAuthority,
    PluginSelectionPublication,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

const STORE_SCHEMA: &str = "lenso.plugin-configuration-store.v3";
const PREVIOUS_STORE_SCHEMA: &str = "lenso.plugin-configuration-store.v2";
const LEGACY_STORE_SCHEMA: &str = "lenso.plugin-configuration-store.v1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginConfigurationPublicationRecord {
    pub proposal_digest: String,
    pub revision: String,
    pub base_revision: String,
    pub plugin_id: String,
    pub instance_key: String,
    pub configuration_toml: String,
    pub published_at_unix_ms: i64,
    pub rollback_of_proposal_digest: Option<String>,
    pub base_source_digest: Option<String>,
}

pub(crate) struct PluginConfigurationChangeBatch {
    pub desired_revision: String,
    pub head_cursor: String,
    pub publications: Vec<PluginConfigurationPublicationRecord>,
}

pub trait PluginConfigurationHistoryAuthority: std::fmt::Debug + Send + Sync {
    fn publications(
        &self,
        plugin_id: &str,
        instance: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<PluginConfigurationPublicationRecord>>;

    fn propose_rollback(
        &self,
        expected_revision: &PluginRootRevision,
        plugin_id: &str,
        instance: &str,
        publication_proposal_digest: &str,
    ) -> anyhow::Result<Option<(PluginConfigurationProposal, String)>>;
}

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
        rollback_of_proposal_digest: Option<&str>,
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
                    instance_key, configuration_toml, review_status, phase,
                    rollback_of_proposal_digest, base_source_digest
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'proposed', ?8, ?9)",
                params![
                    proposal.digest(),
                    proposal.base_revision().as_str(),
                    proposal.candidate_revision().as_str(),
                    proposal.plugin_id(),
                    proposal.instance_key(),
                    bytes,
                    review_status,
                    rollback_of_proposal_digest,
                    proposal.base_source_digest().as_str(),
                ],
            )?;
            let stored = verify_stored_proposal(&transaction, proposal, bytes)?;
            if stored.rollback_of_proposal_digest.as_deref() != rollback_of_proposal_digest {
                bail!("durable Plugin configuration proposal does not match reviewed evidence");
            }
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
            || proposal.base_source_digest.as_deref()
                != Some(publication.base_source_digest().as_str())
        {
            bail!("Plugin configuration publication does not match durable intent");
        }
        finalize_publication(&transaction, proposal, publication.revision().as_str())?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn propose_with_rollback_source(
        &self,
        expected_revision: &PluginRootRevision,
        plugin_id: &str,
        instance: &str,
        bytes: &[u8],
        rollback_of_proposal_digest: Option<&str>,
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
            Self::persist_proposal(
                connection,
                &proposal,
                bytes,
                rollback_of_proposal_digest,
            )?;
            Ok(proposal)
        })
    }

    pub fn publications(
        &self,
        plugin_id: &str,
        instance: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<PluginConfigurationPublicationRecord>> {
        let limit = i64::try_from(limit.clamp(1, 50)).context("publication limit exceeds i64")?;
        self.with_operation(|connection| {
            self.reconcile(connection)?;
            let mut statement = connection.prepare(
                "SELECT proposal_digest, revision, base_revision, plugin_id, instance_key,
                        configuration_toml, published_at_unix_ms, rollback_of_proposal_digest,
                        base_source_digest
                 FROM configuration_publications
                 WHERE plugin_id = ?1 AND instance_key = ?2
                 ORDER BY published_at_unix_ms DESC, rowid DESC
                 LIMIT ?3",
            )?;
            let rows = statement.query_map(params![plugin_id, instance, limit], |row| {
                let configuration_toml = row.get::<_, Vec<u8>>(5)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    configuration_toml,
                    row.get::<_, i64>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                ))
            })?;
            rows.map(|row| {
                let (
                    proposal_digest,
                    revision,
                    base_revision,
                    plugin_id,
                    instance_key,
                    configuration_toml,
                    published_at_unix_ms,
                    rollback_of_proposal_digest,
                    base_source_digest,
                ) = row?;
                Ok(PluginConfigurationPublicationRecord {
                    proposal_digest,
                    revision,
                    base_revision,
                    plugin_id,
                    instance_key,
                    configuration_toml: String::from_utf8(configuration_toml)
                        .context("published Plugin configuration TOML is not UTF-8")?,
                    published_at_unix_ms,
                    rollback_of_proposal_digest,
                    base_source_digest,
                })
            })
            .collect()
        })
    }

    pub(crate) fn publication_changes(
        &self,
        after_revision: &str,
        after_cursor: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<PluginConfigurationChangeBatch> {
        let limit =
            i64::try_from(limit.clamp(1, 64)).context("publication change limit exceeds i64")?;
        self.with_operation(|connection| {
            let desired_revision = self.reconcile(connection)?.revision().as_str().to_owned();
            let head = connection
                .query_row(
                    "SELECT proposal_digest, revision FROM configuration_publications ORDER BY rowid DESC LIMIT 1",
                    [],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?;
            if let Some((_, head_revision)) = head.as_ref()
                && head_revision != &desired_revision
            {
                bail!("Plugin configuration publication ledger does not reach desired revision");
            }
            let head_cursor = head.map_or_else(|| "initial".to_owned(), |(digest, _)| digest);
            let publications = if after_revision == desired_revision {
                Vec::new()
            } else {
                publication_changes_from(connection, after_revision, after_cursor, limit)?
            };
            if after_revision != desired_revision
                && publications
                    .first()
                    .is_none_or(|record| record.base_revision != after_revision)
            {
                bail!("revision history gap");
            }
            if !publication_chain_is_continuous(&publications) {
                bail!("Plugin configuration publication ledger is discontinuous");
            }
            Ok(PluginConfigurationChangeBatch {
                desired_revision,
                head_cursor,
                publications,
            })
        })
    }

    pub fn propose_rollback(
        &self,
        expected_revision: &PluginRootRevision,
        plugin_id: &str,
        instance: &str,
        publication_proposal_digest: &str,
    ) -> anyhow::Result<Option<(PluginConfigurationProposal, String)>> {
        let publication = self.with_operation(|connection| {
            self.reconcile(connection)?;
            publication_by_digest(connection, plugin_id, instance, publication_proposal_digest)
        })?;
        let Some(publication) = publication else {
            return Ok(None);
        };
        let proposal = self.propose_with_rollback_source(
            expected_revision,
            plugin_id,
            instance,
            publication.configuration_toml.as_bytes(),
            Some(publication_proposal_digest),
        )?;
        Ok(Some((proposal, publication.configuration_toml)))
    }
}

type StoredPublicationRow = (
    String,
    String,
    String,
    String,
    String,
    Vec<u8>,
    i64,
    Option<String>,
    Option<String>,
);

fn publication_record_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredPublicationRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
    ))
}

fn publication_record(
    row: StoredPublicationRow,
) -> anyhow::Result<PluginConfigurationPublicationRecord> {
    let (
        proposal_digest,
        revision,
        base_revision,
        plugin_id,
        instance_key,
        configuration_toml,
        published_at_unix_ms,
        rollback_of_proposal_digest,
        base_source_digest,
    ) = row;
    Ok(PluginConfigurationPublicationRecord {
        proposal_digest,
        revision,
        base_revision,
        plugin_id,
        instance_key,
        configuration_toml: String::from_utf8(configuration_toml)
            .context("published Plugin configuration TOML is not UTF-8")?,
        published_at_unix_ms,
        rollback_of_proposal_digest,
        base_source_digest,
    })
}

fn publication_changes_from(
    connection: &Connection,
    after_revision: &str,
    after_cursor: Option<&str>,
    limit: i64,
) -> anyhow::Result<Vec<PluginConfigurationPublicationRecord>> {
    let (comparison, anchor) = match after_cursor {
        Some("initial") => (">", 0_i64),
        Some(cursor) => {
            let rowid = connection
                .query_row(
                    "SELECT rowid FROM configuration_publications WHERE proposal_digest = ?1",
                    [cursor],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .context("change cursor is unavailable")?;
            (">", rowid)
        }
        None => {
            let rowid = connection
                .query_row(
                    "SELECT rowid FROM configuration_publications WHERE base_revision = ?1 ORDER BY rowid DESC LIMIT 1",
                    [after_revision],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .context("revision history gap")?;
            (">=", rowid)
        }
    };
    let query = format!(
        "SELECT proposal_digest, revision, base_revision, plugin_id, instance_key,
                configuration_toml, published_at_unix_ms, rollback_of_proposal_digest,
                base_source_digest
         FROM configuration_publications
         WHERE rowid {comparison} ?1
         ORDER BY rowid ASC
         LIMIT ?2"
    );
    let mut statement = connection.prepare(&query)?;
    statement
        .query_map(params![anchor, limit], publication_record_row)?
        .map(|row| publication_record(row?))
        .collect()
}

fn publication_chain_is_continuous(publications: &[PluginConfigurationPublicationRecord]) -> bool {
    publications
        .windows(2)
        .all(|pair| pair[0].revision == pair[1].base_revision)
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
        self.propose_with_rollback_source(expected_revision, plugin_id, instance, bytes, None)
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

impl PluginSelectionAuthority for SqlitePluginConfigurationAuthority {
    fn source(&self) -> PluginConfigurationAuthoritySource {
        self.source.clone()
    }

    fn set_enabled(
        &self,
        expected_revision: &PluginRootRevision,
        plugin_id: &str,
        instance: &str,
        enabled: bool,
    ) -> anyhow::Result<PluginSelectionPublication> {
        self.with_operation(|connection| {
            let materialized = self.reconcile(connection)?;
            if materialized.revision() != expected_revision {
                bail!(
                    "Plugin configuration store revision conflict: expected {expected_revision}, current {}",
                    materialized.revision()
                );
            }
            let publication = self.local.set_enabled(
                expected_revision,
                plugin_id,
                instance,
                enabled,
            )?;
            let changed = connection.execute(
                "UPDATE authority_state SET desired_revision = ?1 WHERE singleton = 1 AND desired_revision = ?2",
                params![publication.revision().as_str(), publication.base_revision().as_str()],
            )?;
            if changed != 1 {
                bail!("Plugin configuration store lost selection mutation authority");
            }
            Ok(publication)
        })
    }
}

impl PluginConfigurationHistoryAuthority for SqlitePluginConfigurationAuthority {
    fn publications(
        &self,
        plugin_id: &str,
        instance: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<PluginConfigurationPublicationRecord>> {
        Self::publications(self, plugin_id, instance, limit)
    }

    fn propose_rollback(
        &self,
        expected_revision: &PluginRootRevision,
        plugin_id: &str,
        instance: &str,
        publication_proposal_digest: &str,
    ) -> anyhow::Result<Option<(PluginConfigurationProposal, String)>> {
        Self::propose_rollback(
            self,
            expected_revision,
            plugin_id,
            instance,
            publication_proposal_digest,
        )
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
    rollback_of_proposal_digest: Option<String>,
    base_source_digest: Option<String>,
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
            phase TEXT NOT NULL CHECK (phase IN ('proposed', 'materializing', 'published')),
            rollback_of_proposal_digest TEXT,
            base_source_digest TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS configuration_publications (
            proposal_digest TEXT PRIMARY KEY,
            revision TEXT NOT NULL,
            base_revision TEXT NOT NULL,
            plugin_id TEXT NOT NULL,
            instance_key TEXT NOT NULL,
            configuration_toml BLOB NOT NULL,
            published_at_unix_ms INTEGER NOT NULL,
            rollback_of_proposal_digest TEXT,
            base_source_digest TEXT NOT NULL,
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
        if schema == LEGACY_STORE_SCHEMA {
            add_column_if_missing(
                connection,
                "configuration_proposals",
                "rollback_of_proposal_digest",
                "TEXT",
            )?;
            add_column_if_missing(
                connection,
                "configuration_publications",
                "rollback_of_proposal_digest",
                "TEXT",
            )?;
        }
        if schema == LEGACY_STORE_SCHEMA || schema == PREVIOUS_STORE_SCHEMA {
            add_column_if_missing(
                connection,
                "configuration_proposals",
                "base_source_digest",
                "TEXT",
            )?;
            add_column_if_missing(
                connection,
                "configuration_publications",
                "base_source_digest",
                "TEXT",
            )?;
            connection.execute(
                "UPDATE authority_state SET schema = ?1 WHERE singleton = 1 AND schema = ?2",
                params![STORE_SCHEMA, schema],
            )?;
        } else if schema != STORE_SCHEMA {
            bail!("unsupported Plugin configuration store schema {schema}");
        }
        if stored != reference {
            bail!("Plugin configuration store belongs to authority {stored}, not {reference}");
        }
    }
    Ok(())
}

fn add_column_if_missing(
    connection: &Connection,
    table: &str,
    column: &str,
    declaration: &str,
) -> anyhow::Result<()> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    if !columns.iter().any(|candidate| candidate == column) {
        connection.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {declaration}"
        ))?;
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
                instance_key, configuration_toml, rollback_of_proposal_digest,
                base_source_digest
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
            rollback_of_proposal_digest: row.get(6)?,
            base_source_digest: row.get(7)?,
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
) -> anyhow::Result<StoredProposal> {
    let stored = transaction
        .query_row(
            "SELECT proposal_digest, base_revision, candidate_revision, plugin_id,
                    instance_key, configuration_toml, rollback_of_proposal_digest,
                    base_source_digest
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
                    rollback_of_proposal_digest: row.get(6)?,
                    base_source_digest: row.get(7)?,
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
        || stored.base_source_digest.as_deref() != Some(proposal.base_source_digest().as_str())
    {
        bail!("durable Plugin configuration proposal does not match reviewed evidence");
    }
    Ok(stored)
}

fn finalize_publication(
    transaction: &Transaction<'_>,
    proposal: &StoredProposal,
    revision: &str,
) -> anyhow::Result<()> {
    let base_source_digest = proposal
        .base_source_digest
        .as_deref()
        .context("durable Plugin configuration proposal predates exact-source CAS evidence")?;
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
            configuration_toml, published_at_unix_ms, rollback_of_proposal_digest,
            base_source_digest
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            proposal.proposal_digest,
            revision,
            proposal.base_revision,
            proposal.plugin_id,
            proposal.instance_key,
            proposal.configuration_toml,
            unix_time_millis()?,
            proposal.rollback_of_proposal_digest,
            base_source_digest,
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

fn publication_by_digest(
    connection: &Connection,
    plugin_id: &str,
    instance: &str,
    proposal_digest: &str,
) -> anyhow::Result<Option<PluginConfigurationPublicationRecord>> {
    let row = connection
        .query_row(
            "SELECT proposal_digest, revision, base_revision, plugin_id, instance_key,
                    configuration_toml, published_at_unix_ms, rollback_of_proposal_digest,
                    base_source_digest
             FROM configuration_publications
             WHERE proposal_digest = ?1 AND plugin_id = ?2 AND instance_key = ?3",
            params![proposal_digest, plugin_id, instance],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Vec<u8>>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                ))
            },
        )
        .optional()?;
    row.map(
        |(
            proposal_digest,
            revision,
            base_revision,
            plugin_id,
            instance_key,
            configuration_toml,
            published_at_unix_ms,
            rollback_of_proposal_digest,
            base_source_digest,
        )| {
            Ok(PluginConfigurationPublicationRecord {
                proposal_digest,
                revision,
                base_revision,
                plugin_id,
                instance_key,
                configuration_toml: String::from_utf8(configuration_toml)
                    .context("published Plugin configuration TOML is not UTF-8")?,
                published_at_unix_ms,
                rollback_of_proposal_digest,
                base_source_digest,
            })
        },
    )
    .transpose()
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
            [HostSlot::optional("agent")],
            [HostPluginRelease::new(descriptor)],
            [HostDefaultPlugin::new("example.agent", "default").disableable()],
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
                "SELECT phase, configuration_toml, base_source_digest
                 FROM configuration_proposals WHERE proposal_digest = ?1",
                [proposal.digest()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(stored.0, "proposed");
        assert_eq!(stored.1, b"greeting = \"hello\"\n");
        assert_eq!(stored.2, proposal.base_source_digest().as_str());
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
                "SELECT s.desired_revision, p.phase, h.proposal_digest, h.base_source_digest
                 FROM authority_state s
                 JOIN configuration_proposals p ON p.proposal_digest = ?1
                 JOIN configuration_publications h ON h.proposal_digest = p.proposal_digest",
                [proposal.digest()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(stored.0, publication.revision().as_str());
        assert_eq!(stored.1, "published");
        assert_eq!(stored.2, proposal.digest());
        assert_eq!(stored.3, publication.base_source_digest().as_str());
    }

    #[test]
    fn selection_mutation_advances_the_managed_revision() {
        let root = fixture_root();
        let database = root.path().join("configuration.sqlite3");
        let authority = open_authority(&root, &database);
        let base = authority.inspect().unwrap().revision().clone();

        let disabled = authority
            .set_enabled(&base, "example.agent", "default", false)
            .unwrap();

        assert!(!disabled.enabled());
        assert_eq!(authority.inspect().unwrap().revision(), disabled.revision());
        assert!(
            root.path()
                .join("plugins/example.agent/default.disabled")
                .is_file()
        );
        let stored = Connection::open(database)
            .unwrap()
            .query_row(
                "SELECT desired_revision FROM authority_state WHERE singleton = 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        assert_eq!(stored, disabled.revision().as_str());
    }

    #[test]
    fn rollback_is_a_new_reviewed_proposal_with_a_traceable_publication() {
        let root = fixture_root();
        let database = root.path().join("configuration.sqlite3");
        let authority = open_authority(&root, &database);
        let first = proposal(&authority, b"greeting = \"first\"\n");
        authority.publish(&first).unwrap();
        let second = proposal(&authority, b"greeting = \"second\"\n");
        let second_publication = authority.publish(&second).unwrap();

        let (rollback, toml) = authority
            .propose_rollback(
                second_publication.revision(),
                "example.agent",
                "default",
                first.digest(),
            )
            .unwrap()
            .unwrap();

        assert_eq!(toml, "greeting = \"first\"\n");
        assert_eq!(
            fs::read(root.path().join("plugins/example.agent/default.toml")).unwrap(),
            b"greeting = \"second\"\n"
        );
        authority.publish(&rollback).unwrap();

        let history = authority
            .publications("example.agent", "default", 20)
            .unwrap();
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].configuration_toml, "greeting = \"first\"\n");
        assert_eq!(
            history[0].rollback_of_proposal_digest.as_deref(),
            Some(first.digest())
        );
        assert_eq!(history[1].proposal_digest, second.digest());
        assert_eq!(history[2].proposal_digest, first.digest());
    }

    #[test]
    fn normal_proposal_cannot_reuse_rollback_provenance_for_the_same_digest() {
        let root = fixture_root();
        let database = root.path().join("configuration.sqlite3");
        let authority = open_authority(&root, &database);
        let first = proposal(&authority, b"greeting = \"first\"\n");
        authority.publish(&first).unwrap();
        let second = proposal(&authority, b"greeting = \"second\"\n");
        let second_publication = authority.publish(&second).unwrap();

        let (rollback, _) = authority
            .propose_rollback(
                second_publication.revision(),
                "example.agent",
                "default",
                first.digest(),
            )
            .unwrap()
            .unwrap();
        let error = authority
            .propose(
                second_publication.revision(),
                "example.agent",
                "default",
                b"greeting = \"first\"\n",
            )
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("does not match reviewed evidence"),
            "{error}"
        );
        let connection = Connection::open(&database).unwrap();
        let stored: Option<String> = connection
            .query_row(
                "SELECT rollback_of_proposal_digest FROM configuration_proposals WHERE proposal_digest = ?1",
                [rollback.digest()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored.as_deref(), Some(first.digest()));
    }

    #[test]
    fn opens_and_migrates_a_v1_store_without_losing_history() {
        let root = fixture_root();
        let database = root.path().join("configuration.sqlite3");
        let authority = open_authority(&root, &database);
        let published = proposal(&authority, b"greeting = \"legacy\"\n");
        authority.publish(&published).unwrap();
        drop(authority);

        let connection = Connection::open(&database).unwrap();
        connection
            .execute(
                "UPDATE authority_state SET schema = ?1 WHERE singleton = 1",
                [LEGACY_STORE_SCHEMA],
            )
            .unwrap();
        connection
            .execute_batch(
                "ALTER TABLE configuration_proposals DROP COLUMN rollback_of_proposal_digest;
                 ALTER TABLE configuration_publications DROP COLUMN rollback_of_proposal_digest;
                 ALTER TABLE configuration_proposals DROP COLUMN base_source_digest;
                 ALTER TABLE configuration_publications DROP COLUMN base_source_digest;",
            )
            .unwrap();
        drop(connection);

        let migrated = open_authority(&root, &database);
        let history = migrated
            .publications("example.agent", "default", 20)
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].proposal_digest, published.digest());
        assert_eq!(history[0].rollback_of_proposal_digest, None);
        assert_eq!(history[0].base_source_digest, None);
        let connection = Connection::open(&database).unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT schema FROM authority_state WHERE singleton = 1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            STORE_SCHEMA
        );
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
