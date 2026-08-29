use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
    time::Duration,
};

use fs2::FileExt;
use lenso_plugin_control_plane::{
    AppGenerationSpec, CanonicalDocument, ControlLifecycle, ControlPlaneError, ControlStateStore,
    DurableControlState, FileControlStateStore,
};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};

use crate::{
    AgentSurfaceKind,
    authority::{AuthorityCoordinator, AuthorityFence},
};

const APP_ID: &str = "lenso.agent.harness";
const STATE_DIRECTORY: &str = ".state";
const DATABASE_FILE: &str = "runtime.sqlite3";
const LEGACY_BACKUP_DIRECTORY: &str = "legacy-v0";
const LEGACY_GENERATION_DIRECTORY: &str = "generations";
const LEGACY_AUTHORITY_LOCK: &str = "generation-authority.lock";
const LEGACY_GC_LOCK: &str = "generation-gc.lock";
const SCHEMA_VERSION: i64 = 1;
const SCHEMA: &str = r"
CREATE TABLE IF NOT EXISTS runtime_metadata (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS controller_states (
    lineage TEXT PRIMARY KEY NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 0),
    state_json BLOB NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS generation_specs (
    digest TEXT PRIMARY KEY NOT NULL,
    spec_json BLOB NOT NULL
) STRICT;
CREATE TABLE IF NOT EXISTS maintenance_runs (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    completed_at_unix_seconds INTEGER NOT NULL CHECK (completed_at_unix_seconds >= 0),
    removed_generations INTEGER NOT NULL CHECK (removed_generations >= 0)
) STRICT;
";

/// The Host-owned durable runtime ledger.
///
/// Surface callers provide only a logical lineage. SQLite layout, canonical
/// Generation records, CAS revisions, and maintenance metadata stay private to
/// this implementation.
#[derive(Clone, Debug)]
pub(crate) struct RuntimeState {
    root: PathBuf,
    database: PathBuf,
    read_only: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeStateSummary {
    pub(crate) controller_lineages: u64,
    pub(crate) recoverable_generations: u64,
    pub(crate) last_maintenance: Option<(u64, u64)>,
}

impl RuntimeState {
    pub(crate) fn open(root: &Path) -> Result<Self, String> {
        prepare_root(root)?;
        let state = Self {
            root: root.to_path_buf(),
            database: root.join(STATE_DIRECTORY).join(DATABASE_FILE),
            read_only: false,
        };
        state.prepare_database().map_err(runtime_error)?;
        state.migrate_legacy_layout()?;
        Ok(state)
    }

    /// Opens an already-initialized ledger without creating or migrating state.
    pub(crate) fn open_existing(root: &Path) -> Result<Self, String> {
        validate_existing_root(root)?;
        let state = Self {
            root: root.to_path_buf(),
            database: root.join(STATE_DIRECTORY).join(DATABASE_FILE),
            read_only: true,
        };
        let metadata = fs::symlink_metadata(&state.database)
            .map_err(|error| format!("failed to inspect runtime ledger: {error}"))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err("runtime ledger is not a regular file".to_owned());
        }
        let connection = state.connect_existing().map_err(runtime_error)?;
        let version = connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .map_err(store_error)
            .map_err(runtime_error)?;
        if version != SCHEMA_VERSION {
            return Err(format!(
                "runtime ledger schema version {version} is unsupported"
            ));
        }
        Ok(state)
    }

    pub(crate) fn control_store(
        &self,
        lineage: impl Into<String>,
    ) -> Result<LedgerControlStateStore, String> {
        let lineage = lineage.into();
        validate_lineage(&lineage)?;
        Ok(LedgerControlStateStore {
            database: self.database.clone(),
            lineage,
        })
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn attach(&self, surface: AgentSurfaceKind) -> Result<RuntimeAttachment, String> {
        let authority = AuthorityCoordinator::prepare(&self.root)?;
        let (base, capacity) = surface_policy(surface);
        let (lineage, host_lease) = (0..capacity)
            .find_map(|index| {
                let lineage = lineage_name(base, index);
                match authority.try_host_lease(&lineage) {
                    Ok(Some(lease)) => Some(Ok((lineage, lease))),
                    Ok(None) => None,
                    Err(error) => Some(Err(error)),
                }
            })
            .transpose()?
            .ok_or_else(|| format!("all {capacity} `{base}` Host slots are already in use"))?;
        let generation_gc_lease = authority.generation_gc_snapshot()?;
        let store = self.control_store(&lineage)?;
        Ok(RuntimeAttachment {
            state: self.clone(),
            authority,
            store,
            host_lease: Some(host_lease),
            generation_gc_lease: Some(generation_gc_lease),
        })
    }

    pub(crate) fn record_generation(
        &self,
        spec: &CanonicalDocument<AppGenerationSpec>,
    ) -> Result<(), String> {
        validate_generation(spec)?;
        let mut connection = self.connect().map_err(runtime_error)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)
            .map_err(runtime_error)?;
        record_generation_in(&transaction, spec).map_err(runtime_error)?;
        transaction
            .commit()
            .map_err(store_error)
            .map_err(runtime_error)
    }

    pub(crate) fn load_generation(
        &self,
        digest: &str,
    ) -> Result<CanonicalDocument<AppGenerationSpec>, String> {
        canonical_digest_hash(digest)?;
        let connection = self.connect().map_err(runtime_error)?;
        let bytes = connection
            .query_row(
                "SELECT spec_json FROM generation_specs WHERE digest = ?1",
                [digest],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(store_error)
            .map_err(runtime_error)?
            .ok_or_else(|| format!("Generation Spec `{digest}` is unavailable"))?;
        parse_generation(digest, &bytes)
    }

    pub(crate) fn has_generation(&self, digest: &str) -> Result<bool, String> {
        canonical_digest_hash(digest)?;
        let connection = self.connect().map_err(runtime_error)?;
        connection
            .query_row(
                "SELECT 1 FROM generation_specs WHERE digest = ?1",
                [digest],
                |_| Ok(()),
            )
            .optional()
            .map(|value| value.is_some())
            .map_err(store_error)
            .map_err(runtime_error)
    }

    pub(crate) fn generations(
        &self,
    ) -> Result<BTreeMap<String, CanonicalDocument<AppGenerationSpec>>, String> {
        let connection = self.connect().map_err(runtime_error)?;
        let mut statement = connection
            .prepare("SELECT digest, spec_json FROM generation_specs ORDER BY digest")
            .map_err(store_error)
            .map_err(runtime_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
            })
            .map_err(store_error)
            .map_err(runtime_error)?;
        let mut generations = BTreeMap::new();
        for row in rows {
            let (digest, bytes) = row.map_err(store_error).map_err(runtime_error)?;
            generations.insert(digest.clone(), parse_generation(&digest, &bytes)?);
        }
        Ok(generations)
    }

    pub(crate) fn remove_generation(&self, digest: &str) -> Result<(), String> {
        canonical_digest_hash(digest)?;
        self.load_generation(digest)?;
        let connection = self.connect().map_err(runtime_error)?;
        let removed = connection
            .execute("DELETE FROM generation_specs WHERE digest = ?1", [digest])
            .map_err(store_error)
            .map_err(runtime_error)?;
        if removed != 1 {
            return Err(format!(
                "Generation Spec `{digest}` changed during collection"
            ));
        }
        Ok(())
    }

    pub(crate) fn controller_states(&self) -> Result<Vec<(String, DurableControlState)>, String> {
        let connection = self.connect().map_err(runtime_error)?;
        let mut statement = connection
            .prepare("SELECT lineage, state_json FROM controller_states ORDER BY lineage")
            .map_err(store_error)
            .map_err(runtime_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
            })
            .map_err(store_error)
            .map_err(runtime_error)?;
        let mut states = Vec::new();
        for row in rows {
            let (lineage, bytes) = row.map_err(store_error).map_err(runtime_error)?;
            validate_lineage(&lineage)?;
            states.push((lineage, parse_control_state(&bytes).map_err(runtime_error)?));
        }
        Ok(states)
    }

    pub(crate) fn summary(&self) -> Result<RuntimeStateSummary, String> {
        let connection = self.connect_existing().map_err(runtime_error)?;
        let controller_lineages = count_rows(&connection, "controller_states")?;
        let recoverable_generations = count_rows(&connection, "generation_specs")?;
        let last_maintenance = connection
            .query_row(
                "SELECT completed_at_unix_seconds, removed_generations FROM maintenance_runs ORDER BY sequence DESC LIMIT 1",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()
            .map_err(store_error)
            .map_err(runtime_error)?
            .map(|(completed_at, removed)| -> Result<(u64, u64), String> {
                Ok((
                    u64::try_from(completed_at)
                        .map_err(|_| "runtime maintenance time is invalid".to_owned())?,
                    u64::try_from(removed)
                        .map_err(|_| "runtime maintenance count is invalid".to_owned())?,
                ))
            })
            .transpose()?;
        Ok(RuntimeStateSummary {
            controller_lineages,
            recoverable_generations,
            last_maintenance,
        })
    }

    pub(crate) fn record_maintenance(&self, removed_generations: usize) -> Result<(), String> {
        let removed_generations = i64::try_from(removed_generations)
            .map_err(|_| "Generation maintenance count exceeded SQLite range".to_owned())?;
        let completed_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| format!("system clock precedes Unix epoch: {error}"))?;
        let completed_at = i64::try_from(completed_at.as_secs())
            .map_err(|_| "Generation maintenance time exceeded SQLite range".to_owned())?;
        let connection = self.connect().map_err(runtime_error)?;
        connection
            .execute(
                "INSERT INTO maintenance_runs(completed_at_unix_seconds, removed_generations) VALUES (?1, ?2)",
                params![completed_at, removed_generations],
            )
            .map_err(store_error)
            .map_err(runtime_error)?;
        Ok(())
    }

    /// Removes the hidden legacy recovery copy only after the new ledger has
    /// successfully opened and the Host has completed exact recovery.
    pub(crate) fn confirm_legacy_migration(&self) -> Result<(), String> {
        let backup = self.legacy_backup();
        let metadata = match fs::symlink_metadata(&backup) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return self.remove_legacy_lease_files();
            }
            Err(error) => {
                return Err(format!("failed to inspect legacy runtime backup: {error}"));
            }
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("legacy runtime backup is not a regular directory".to_owned());
        }
        fs::remove_dir_all(&backup)
            .map_err(|error| format!("failed to remove migrated runtime backup: {error}"))?;
        sync_directory(backup.parent().expect("legacy runtime backup has a parent"))?;
        self.remove_legacy_lease_files()
    }

    fn prepare_database(&self) -> Result<(), ControlPlaneError> {
        let state_directory = self
            .database
            .parent()
            .expect("runtime ledger has a state directory");
        fs::create_dir_all(state_directory).map_err(store_error)?;
        let state_metadata = fs::symlink_metadata(state_directory).map_err(store_error)?;
        if !state_metadata.is_dir() || state_metadata.file_type().is_symlink() {
            return Err(store_failure(
                "runtime state path is not a regular directory",
            ));
        }
        if self.database.exists() {
            let metadata = fs::symlink_metadata(&self.database).map_err(store_error)?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(store_failure("runtime ledger is not a regular file"));
            }
        }
        let connection = self.connect()?;
        let version = connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .map_err(store_error)?;
        if !matches!(version, 0 | SCHEMA_VERSION) {
            return Err(store_failure(format!(
                "runtime ledger schema version {version} is unsupported"
            )));
        }
        connection
            .execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL;")
            .map_err(store_error)?;
        connection.execute_batch(SCHEMA).map_err(store_error)?;
        if version == 0 {
            connection
                .pragma_update(None, "user_version", SCHEMA_VERSION)
                .map_err(store_error)?;
        }
        Ok(())
    }

    fn connect(&self) -> Result<Connection, ControlPlaneError> {
        if self.read_only {
            return self.connect_existing();
        }
        let connection = Connection::open(&self.database).map_err(store_error)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(store_error)?;
        connection
            .execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = FULL;")
            .map_err(store_error)?;
        Ok(connection)
    }

    fn connect_existing(&self) -> Result<Connection, ControlPlaneError> {
        let connection = Connection::open_with_flags(
            &self.database,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(store_error)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(store_error)?;
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(store_error)?;
        Ok(connection)
    }

    fn migrate_legacy_layout(&self) -> Result<(), String> {
        let connection = self.connect().map_err(runtime_error)?;
        let imported = connection
            .query_row(
                "SELECT value FROM runtime_metadata WHERE key = 'legacy_layout'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(store_error)
            .map_err(runtime_error)?;
        if imported.as_deref() == Some("imported") {
            self.stage_legacy_backup()?;
            return Ok(());
        }
        if imported.is_some() {
            return Err("runtime ledger contains an invalid legacy migration marker".to_owned());
        }

        let legacy = self.legacy_entries()?;
        if legacy.is_empty() {
            connection
                .execute(
                    "INSERT INTO runtime_metadata(key, value) VALUES ('legacy_layout', 'imported')",
                    [],
                )
                .map_err(store_error)
                .map_err(runtime_error)?;
            return Ok(());
        }

        let _legacy_fence = LegacyMigrationFence::try_acquire(&self.root)?;
        let mut connection = self.connect().map_err(runtime_error)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)
            .map_err(runtime_error)?;
        for entry in &legacy {
            match entry {
                LegacyEntry::Controller(lineage, path) => {
                    let state = FileControlStateStore::open(path)
                        .map_err(runtime_error)?
                        .load(APP_ID)
                        .map_err(runtime_error)?;
                    import_control_state(&transaction, lineage, &state).map_err(runtime_error)?;
                }
                LegacyEntry::Generations(path) => {
                    for generation in legacy_generations(path)? {
                        record_generation_in(&transaction, &generation).map_err(runtime_error)?;
                    }
                }
            }
        }
        transaction
            .execute(
                "INSERT INTO runtime_metadata(key, value) VALUES ('legacy_layout', 'imported')",
                [],
            )
            .map_err(store_error)
            .map_err(runtime_error)?;
        transaction
            .commit()
            .map_err(store_error)
            .map_err(runtime_error)?;
        self.stage_legacy_backup()
    }

    fn legacy_entries(&self) -> Result<Vec<LegacyEntry>, String> {
        let mut entries = Vec::new();
        for (legacy_lineage, lineage) in legacy_controller_mappings() {
            let path = self.root.join(&legacy_lineage);
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                    entries.push(LegacyEntry::Controller(lineage, path));
                }
                Ok(_) => {
                    return Err(format!(
                        "legacy Controller `{lineage}` is not a regular directory"
                    ));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "failed to inspect legacy Controller `{lineage}`: {error}"
                    ));
                }
            }
        }
        let generations = self.root.join(LEGACY_GENERATION_DIRECTORY);
        match fs::symlink_metadata(&generations) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                entries.push(LegacyEntry::Generations(generations));
            }
            Ok(_) => {
                return Err("legacy Generation path is not a regular directory".to_owned());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!("failed to inspect legacy Generations: {error}"));
            }
        }
        Ok(entries)
    }

    fn stage_legacy_backup(&self) -> Result<(), String> {
        let entries = self.legacy_entries()?;
        if entries.is_empty() {
            return Ok(());
        }
        let backup = self.legacy_backup();
        fs::create_dir_all(&backup)
            .map_err(|error| format!("failed to create legacy runtime backup: {error}"))?;
        for entry in entries {
            let source = entry.path();
            let name = source
                .file_name()
                .ok_or_else(|| "legacy runtime entry has no file name".to_owned())?;
            let destination = backup.join(name);
            if destination.exists() {
                return Err(format!(
                    "legacy runtime backup already contains `{}`",
                    name.to_string_lossy()
                ));
            }
            fs::rename(source, destination)
                .map_err(|error| format!("failed to stage legacy runtime backup: {error}"))?;
        }
        sync_directory(&self.root)?;
        sync_directory(&backup)?;
        Ok(())
    }

    fn legacy_backup(&self) -> PathBuf {
        self.database
            .parent()
            .expect("runtime ledger has a state directory")
            .join(LEGACY_BACKUP_DIRECTORY)
    }

    fn remove_legacy_lease_files(&self) -> Result<(), String> {
        let mut names = vec![LEGACY_AUTHORITY_LOCK.to_owned(), LEGACY_GC_LOCK.to_owned()];
        names.extend(
            legacy_controller_mappings()
                .into_iter()
                .map(|(legacy, _)| format!("{legacy}.host.lock")),
        );
        for name in names {
            let path = self.root.join(&name);
            let Some(file) = try_legacy_lock(&path)? else {
                continue;
            };
            fs::remove_file(&path).map_err(|error| {
                format!("failed to remove migrated runtime lease `{name}`: {error}")
            })?;
            drop(file);
        }
        sync_directory(&self.root)
    }
}

/// One process-owned logical Surface attachment. Dropping it releases only
/// process leases; durable lineage and Generation authority remain in the
/// ledger for exact recovery.
#[derive(Debug)]
pub(crate) struct RuntimeAttachment {
    state: RuntimeState,
    authority: AuthorityCoordinator,
    store: LedgerControlStateStore,
    host_lease: Option<AuthorityFence>,
    generation_gc_lease: Option<AuthorityFence>,
}

impl RuntimeAttachment {
    pub(crate) fn control_store(&self) -> LedgerControlStateStore {
        self.store.clone()
    }

    pub(crate) fn authority_snapshot(&self) -> Result<AuthorityFence, String> {
        self.authority.snapshot()
    }

    pub(crate) fn state(&self) -> &RuntimeState {
        &self.state
    }

    pub(crate) fn release(&mut self) {
        self.host_lease.take();
        self.generation_gc_lease.take();
    }
}

#[derive(Debug)]
enum LegacyEntry {
    Controller(String, PathBuf),
    Generations(PathBuf),
}

impl LegacyEntry {
    fn path(&self) -> &Path {
        match self {
            Self::Controller(_, path) | Self::Generations(path) => path,
        }
    }
}

#[derive(Debug)]
struct LegacyMigrationFence {
    _gc: Option<File>,
    _authority: Option<File>,
}

impl LegacyMigrationFence {
    fn try_acquire(root: &Path) -> Result<Self, String> {
        let gc = try_legacy_lock(&root.join(LEGACY_GC_LOCK))?;
        let authority = try_legacy_lock(&root.join(LEGACY_AUTHORITY_LOCK))?;
        Ok(Self {
            _gc: gc,
            _authority: authority,
        })
    }
}

/// One logical Controller lineage projected into the shared ledger.
#[derive(Clone, Debug)]
pub(crate) struct LedgerControlStateStore {
    database: PathBuf,
    lineage: String,
}

impl LedgerControlStateStore {
    fn connect(&self) -> Result<Connection, ControlPlaneError> {
        let connection = Connection::open(&self.database).map_err(store_error)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(store_error)?;
        connection
            .execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = FULL;")
            .map_err(store_error)?;
        Ok(connection)
    }

    fn load_from(
        &self,
        connection: &Connection,
        app_id: &str,
    ) -> Result<DurableControlState, ControlPlaneError> {
        let bytes = connection
            .query_row(
                "SELECT state_json FROM controller_states WHERE lineage = ?1",
                [&self.lineage],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(store_error)?;
        match bytes {
            Some(bytes) => {
                let state = parse_control_state(&bytes)?;
                validate_control_state(&state, app_id)?;
                Ok(state)
            }
            None => Ok(initial_control_state(app_id)),
        }
    }
}

impl ControlStateStore for LedgerControlStateStore {
    fn load(&self, app_id: &str) -> Result<DurableControlState, ControlPlaneError> {
        let connection = self.connect()?;
        self.load_from(&connection, app_id)
    }

    fn compare_and_swap(
        &self,
        app_id: &str,
        expected_revision: u64,
        mut next: DurableControlState,
    ) -> Result<DurableControlState, ControlPlaneError> {
        let mut connection = self.connect()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(store_error)?;
        let current = self.load_from(&transaction, app_id)?;
        if current.revision != expected_revision {
            return Err(ControlPlaneError::TransitionRejected {
                detail: format!(
                    "control-state revision is {}, expected {expected_revision}",
                    current.revision
                ),
            });
        }
        next.revision = expected_revision.checked_add(1).ok_or_else(|| {
            ControlPlaneError::TransitionRejected {
                detail: "control-state revision exhausted".to_owned(),
            }
        })?;
        validate_control_state(&next, app_id)?;
        let revision = sql_revision(next.revision)?;
        let document = CanonicalDocument::from_value("control-state.json", next.clone())?;
        transaction
            .execute(
                "INSERT INTO controller_states(lineage, revision, state_json) VALUES (?1, ?2, ?3)
                 ON CONFLICT(lineage) DO UPDATE SET revision = excluded.revision, state_json = excluded.state_json",
                params![self.lineage, revision, document.bytes()],
            )
            .map_err(store_error)?;
        transaction.commit().map_err(store_error)?;
        Ok(next)
    }
}

fn prepare_root(root: &Path) -> Result<(), String> {
    fs::create_dir_all(root)
        .map_err(|error| format!("failed to create Agent runtime root: {error}"))?;
    let metadata = fs::symlink_metadata(root)
        .map_err(|error| format!("failed to inspect Agent runtime root: {error}"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Agent runtime root is not a regular directory".to_owned());
    }
    Ok(())
}

fn validate_existing_root(root: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(root)
        .map_err(|error| format!("failed to inspect Agent runtime root: {error}"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Agent runtime root is not a regular directory".to_owned());
    }
    let state_directory = root.join(STATE_DIRECTORY);
    let metadata = fs::symlink_metadata(&state_directory)
        .map_err(|error| format!("failed to inspect runtime state path: {error}"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("runtime state path is not a regular directory".to_owned());
    }
    Ok(())
}

fn record_generation_in(
    transaction: &Transaction<'_>,
    spec: &CanonicalDocument<AppGenerationSpec>,
) -> Result<(), ControlPlaneError> {
    let existing = transaction
        .query_row(
            "SELECT spec_json FROM generation_specs WHERE digest = ?1",
            [spec.digest()],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .map_err(store_error)?;
    if let Some(existing) = existing {
        if existing != spec.bytes() {
            return Err(ControlPlaneError::DigestMismatch {
                subject: "Generation Spec".to_owned(),
            });
        }
        return Ok(());
    }
    transaction
        .execute(
            "INSERT INTO generation_specs(digest, spec_json) VALUES (?1, ?2)",
            params![spec.digest(), spec.bytes()],
        )
        .map_err(store_error)?;
    Ok(())
}

fn count_rows(connection: &Connection, table: &str) -> Result<u64, String> {
    let sql = match table {
        "controller_states" => "SELECT COUNT(*) FROM controller_states",
        "generation_specs" => "SELECT COUNT(*) FROM generation_specs",
        _ => return Err("runtime summary requested an unknown table".to_owned()),
    };
    let count = connection
        .query_row(sql, [], |row| row.get::<_, i64>(0))
        .map_err(store_error)
        .map_err(runtime_error)?;
    u64::try_from(count).map_err(|_| "runtime summary count is invalid".to_owned())
}

fn import_control_state(
    transaction: &Transaction<'_>,
    lineage: &str,
    state: &DurableControlState,
) -> Result<(), ControlPlaneError> {
    validate_lineage(lineage).map_err(store_failure)?;
    validate_control_state(state, APP_ID)?;
    let document = CanonicalDocument::from_value("control-state.json", state.clone())?;
    let existing = transaction
        .query_row(
            "SELECT state_json FROM controller_states WHERE lineage = ?1",
            [lineage],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()
        .map_err(store_error)?;
    if let Some(existing) = existing {
        if existing != document.bytes() {
            return Err(ControlPlaneError::AuthorityMismatch {
                detail: format!("legacy Controller `{lineage}` conflicts with the runtime ledger"),
            });
        }
        return Ok(());
    }
    transaction
        .execute(
            "INSERT INTO controller_states(lineage, revision, state_json) VALUES (?1, ?2, ?3)",
            params![lineage, sql_revision(state.revision)?, document.bytes()],
        )
        .map_err(store_error)?;
    Ok(())
}

fn legacy_generations(
    directory: &Path,
) -> Result<Vec<CanonicalDocument<AppGenerationSpec>>, String> {
    let mut entries = fs::read_dir(directory)
        .map_err(|error| format!("failed to enumerate legacy Generation Specs: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("failed to enumerate legacy Generation Specs: {error}"))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    let mut generations = Vec::new();
    for entry in entries {
        let metadata = entry
            .file_type()
            .map_err(|error| format!("failed to inspect legacy Generation Spec: {error}"))?;
        if !metadata.is_file() || metadata.is_symlink() {
            return Err("legacy Generation entry is not a regular file".to_owned());
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "legacy Generation entry has a non-UTF-8 name".to_owned())?;
        if name.starts_with('.')
            && Path::new(&name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("tmp"))
        {
            continue;
        }
        let hash = name
            .strip_suffix(".json")
            .ok_or_else(|| format!("legacy Generation entry `{name}` is not content-addressed"))?;
        let digest = format!("sha256:{hash}");
        canonical_digest_hash(&digest)?;
        let bytes = fs::read(entry.path())
            .map_err(|error| format!("failed to read legacy Generation Spec: {error}"))?;
        generations.push(parse_generation(&digest, &bytes)?);
    }
    Ok(generations)
}

fn legacy_controller_mappings() -> Vec<(String, String)> {
    let mut lineages = vec![
        ("generation-control".to_owned(), "headless".to_owned()),
        (
            "telegram-generation-control".to_owned(),
            "telegram".to_owned(),
        ),
        (
            "discord-generation-control".to_owned(),
            "discord".to_owned(),
        ),
        ("web-generation-control".to_owned(), "web".to_owned()),
        (
            "channel-generation-control".to_owned(),
            "channels".to_owned(),
        ),
    ];
    lineages.extend((0..32).map(|index| {
        let legacy = if index == 0 {
            "tui-generation-control".to_owned()
        } else {
            format!("tui-generation-control-{}", index + 1)
        };
        (legacy, lineage_name("tui", index))
    }));
    lineages
}

fn try_legacy_lock(path: &Path) -> Result<Option<File>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!("failed to inspect legacy runtime lease: {error}"));
        }
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("legacy runtime lease is not a regular file".to_owned());
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("failed to open legacy runtime lease: {error}"))?;
    match FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(Some(file)),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Err(
            "legacy runtime state is in use; stop older Agent processes before migration"
                .to_owned(),
        ),
        Err(error) => Err(format!("failed to fence legacy runtime migration: {error}")),
    }
}

fn sync_directory(path: &Path) -> Result<(), String> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("failed to sync runtime state directory: {error}"))
}

fn parse_control_state(bytes: &[u8]) -> Result<DurableControlState, ControlPlaneError> {
    CanonicalDocument::<DurableControlState>::parse("control-state.json", bytes)
        .map(CanonicalDocument::into_value)
}

fn parse_generation(
    digest: &str,
    bytes: &[u8],
) -> Result<CanonicalDocument<AppGenerationSpec>, String> {
    let generation = CanonicalDocument::<AppGenerationSpec>::parse("lenso-generation.json", bytes)
        .map_err(|error| format!("Generation Spec validation failed: {error}"))?;
    validate_generation(&generation)?;
    if generation.digest() != digest {
        return Err("Generation Spec does not match its requested digest".to_owned());
    }
    Ok(generation)
}

fn validate_generation(spec: &CanonicalDocument<AppGenerationSpec>) -> Result<(), String> {
    canonical_digest_hash(spec.digest())?;
    if spec.value().app_id != APP_ID {
        return Err("Generation Spec belongs to another App".to_owned());
    }
    Ok(())
}

fn validate_control_state(
    state: &DurableControlState,
    app_id: &str,
) -> Result<(), ControlPlaneError> {
    let unique = state
        .generations
        .iter()
        .map(|record| &record.generation_spec_digest)
        .collect::<BTreeSet<_>>();
    let active_count = state
        .generations
        .iter()
        .filter(|record| record.lifecycle == ControlLifecycle::Active)
        .count();
    let active_closes = state
        .active_generation_spec_digest
        .as_ref()
        .is_none_or(|active| {
            state.generations.iter().any(|record| {
                &record.generation_spec_digest == active
                    && record.lifecycle == ControlLifecycle::Active
            })
        });
    if state.schema_version != 1
        || state.app_id != app_id
        || unique.len() != state.generations.len()
        || !active_closes
        || active_count != usize::from(state.active_generation_spec_digest.is_some())
    {
        return Err(ControlPlaneError::TransitionRejected {
            detail: "durable control state violates App, uniqueness, or active-route closure"
                .to_owned(),
        });
    }
    Ok(())
}

fn initial_control_state(app_id: &str) -> DurableControlState {
    DurableControlState {
        schema_version: 1,
        app_id: app_id.to_owned(),
        revision: 0,
        supervisor_epoch: 0,
        routing_epoch: 0,
        host_suspended: false,
        active_generation_spec_digest: None,
        generations: Vec::new(),
    }
}

fn surface_policy(surface: AgentSurfaceKind) -> (&'static str, usize) {
    match surface {
        AgentSurfaceKind::Headless => ("headless", 1),
        AgentSurfaceKind::Tui => ("tui", 32),
        AgentSurfaceKind::Channels => ("channels", 1),
        AgentSurfaceKind::Telegram => ("telegram", 1),
        AgentSurfaceKind::Discord => ("discord", 1),
        AgentSurfaceKind::Web => ("web", 1),
    }
}

fn lineage_name(base: &str, index: usize) -> String {
    if index == 0 {
        base.to_owned()
    } else {
        format!("{base}-{}", index + 1)
    }
}

fn validate_lineage(lineage: &str) -> Result<(), String> {
    if lineage.is_empty()
        || !lineage
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("Controller lineage is invalid".to_owned());
    }
    Ok(())
}

fn canonical_digest_hash(digest: &str) -> Result<&str, String> {
    digest
        .strip_prefix("sha256:")
        .filter(|hash| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or_else(|| "digest is not canonical SHA-256".to_owned())
}

fn sql_revision(revision: u64) -> Result<i64, ControlPlaneError> {
    i64::try_from(revision)
        .map_err(|_| store_failure("control-state revision exceeded the SQLite integer range"))
}

fn store_error(error: impl std::fmt::Display) -> ControlPlaneError {
    store_failure(error.to_string())
}

fn store_failure(detail: impl Into<String>) -> ControlPlaneError {
    ControlPlaneError::StoreFailure {
        detail: detail.into(),
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "used directly as a Result::map_err conversion"
)]
fn runtime_error(error: ControlPlaneError) -> String {
    format!("Agent runtime state failed: {error}")
}

#[cfg(test)]
mod tests {
    use lenso_plugin_control_plane::{ActivationDirection, ControlHealth, GenerationControlRecord};

    use super::*;

    fn active_state(digest: &str) -> DurableControlState {
        DurableControlState {
            schema_version: 1,
            app_id: APP_ID.to_owned(),
            revision: 0,
            supervisor_epoch: 1,
            routing_epoch: 1,
            host_suspended: false,
            active_generation_spec_digest: Some(digest.to_owned()),
            generations: vec![GenerationControlRecord {
                generation_spec_digest: digest.to_owned(),
                transition_spec_digest: format!("sha256:{}", "2".repeat(64)),
                lifecycle: ControlLifecycle::Active,
                health: ControlHealth::Healthy,
                activation_direction: ActivationDirection::Forward,
                ready_timeout_nanos: "1".to_owned(),
                drain_timeout_nanos: "1".to_owned(),
                drain_deadline_unix_nanos: None,
                rollback_deadline_unix_nanos: None,
                automatic_rollback_on_generation_failure: false,
                state_compatibility_receipt_digests: Vec::new(),
                retirement_reason: None,
            }],
        }
    }

    fn generation_spec() -> CanonicalDocument<AppGenerationSpec> {
        let digest = format!("sha256:{}", "3".repeat(64));
        CanonicalDocument::from_value(
            "lenso-generation.json",
            AppGenerationSpec {
                schema_version: 1,
                app_id: APP_ID.to_owned(),
                host_build_manifest_digest: digest.clone(),
                host_execution_policy_digest: digest.clone(),
                resolved_plan_digest: digest.clone(),
                resolution_authority_digest: digest.clone(),
                resolved_artifact_set_digest: digest.clone(),
                effective_host_grant_set_digest: digest,
            },
        )
        .unwrap()
    }

    #[test]
    fn controller_lineages_share_one_ledger_without_sharing_state() {
        let directory = tempfile::tempdir().unwrap();
        let state = RuntimeState::open(directory.path()).unwrap();
        let tui = state.control_store("tui").unwrap();
        let web = state.control_store("web").unwrap();
        let digest = format!("sha256:{}", "1".repeat(64));

        let committed = tui
            .compare_and_swap(APP_ID, 0, active_state(&digest))
            .unwrap();

        assert_eq!(committed.revision, 1);
        assert_eq!(tui.load(APP_ID).unwrap(), committed);
        assert_eq!(web.load(APP_ID).unwrap().revision, 0);
        assert_eq!(state.controller_states().unwrap().len(), 1);
    }

    #[test]
    fn controller_compare_and_swap_fences_a_stale_revision() {
        let directory = tempfile::tempdir().unwrap();
        let state = RuntimeState::open(directory.path()).unwrap();
        let store = state.control_store("tui").unwrap();
        let digest = format!("sha256:{}", "1".repeat(64));
        store
            .compare_and_swap(APP_ID, 0, active_state(&digest))
            .unwrap();

        let error = store
            .compare_and_swap(APP_ID, 0, active_state(&digest))
            .unwrap_err();

        assert!(matches!(
            error,
            ControlPlaneError::TransitionRejected { .. }
        ));
    }

    #[test]
    fn invalid_lineage_never_becomes_sql() {
        let directory = tempfile::tempdir().unwrap();
        let state = RuntimeState::open(directory.path()).unwrap();
        assert!(state.control_store("../tui").is_err());
    }

    #[test]
    fn read_only_open_does_not_create_runtime_state() {
        let directory = tempfile::tempdir().unwrap();

        assert!(RuntimeState::open_existing(directory.path()).is_err());
        assert!(!directory.path().join(STATE_DIRECTORY).exists());
    }

    #[test]
    fn fresh_runtime_keeps_physical_state_private() {
        let directory = tempfile::tempdir().unwrap();
        let state = RuntimeState::open(directory.path()).unwrap();
        let mut attachment = state.attach(AgentSurfaceKind::Tui).unwrap();

        let mut names = fs::read_dir(directory.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(names, vec![".leases", ".state"]);
        assert_eq!(state.summary().unwrap().controller_lineages, 0);

        attachment.release();
    }

    #[cfg(unix)]
    #[test]
    fn legacy_generation_symlink_is_rejected() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let generations = directory.path().join(LEGACY_GENERATION_DIRECTORY);
        fs::create_dir(&generations).unwrap();
        let target = directory.path().join("outside.json");
        fs::write(&target, b"{}").unwrap();
        symlink(
            &target,
            generations.join(format!("{}.json", "1".repeat(64))),
        )
        .unwrap();

        let error = RuntimeState::open(directory.path()).unwrap_err();
        assert!(error.contains("not a regular file"));
    }

    #[test]
    fn legacy_controller_is_imported_before_the_old_layout_is_removed() {
        let directory = tempfile::tempdir().unwrap();
        let legacy_path = directory.path().join("tui-generation-control");
        let legacy = FileControlStateStore::open(&legacy_path).unwrap();
        let digest = format!("sha256:{}", "1".repeat(64));
        let legacy_state = legacy
            .compare_and_swap(APP_ID, 0, active_state(&digest))
            .unwrap();

        let state = RuntimeState::open(directory.path()).unwrap();

        assert_eq!(
            state.control_store("tui").unwrap().load(APP_ID).unwrap(),
            legacy_state
        );
        assert!(!legacy_path.exists());
        assert!(
            directory
                .path()
                .join(".state/legacy-v0/tui-generation-control")
                .is_dir()
        );

        state.confirm_legacy_migration().unwrap();
        assert!(!directory.path().join(".state/legacy-v0").exists());
    }

    #[test]
    fn legacy_generation_records_are_imported_by_digest() {
        let directory = tempfile::tempdir().unwrap();
        let generations = directory.path().join(LEGACY_GENERATION_DIRECTORY);
        fs::create_dir(&generations).unwrap();
        let spec = generation_spec();
        let hash = spec.digest().strip_prefix("sha256:").unwrap();
        fs::write(generations.join(format!("{hash}.json")), spec.bytes()).unwrap();

        let state = RuntimeState::open(directory.path()).unwrap();

        let loaded = state.load_generation(spec.digest()).unwrap();
        assert_eq!(loaded.digest(), spec.digest());
        assert_eq!(loaded.bytes(), spec.bytes());
        assert!(!generations.exists());
    }
}
