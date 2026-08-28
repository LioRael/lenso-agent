//! Durable SQLite + FTS curated Memory Adapter.

use std::{cell::RefCell, collections::BTreeSet, fs, path::PathBuf, rc::Rc};

use lenso::prelude::*;
use lenso_capability_agent_memory::{
    self as memory_contract, ForgetError, ForgetRequest, ForgetResponse, MemoryItem, MemorySource,
    ObserveError, ObserveRequest, ObserveResponse, RecallError, RecallRequest, RecallResponse,
    RememberError, RememberRequest, RememberResponse,
};
use lenso_kernel::RuntimeFailure;
use rusqlite::{Connection, Transaction, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

const SCHEMA: &str = r"
PRAGMA foreign_keys = ON;
CREATE TABLE IF NOT EXISTS memory_entries (
    memory_id TEXT PRIMARY KEY NOT NULL,
    scope TEXT NOT NULL,
    content TEXT NOT NULL,
    content_digest TEXT NOT NULL,
    confidence_milli INTEGER NOT NULL CHECK (confidence_milli BETWEEN 0 AND 1000),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    deleted_at TEXT
) STRICT;
CREATE UNIQUE INDEX IF NOT EXISTS memory_entries_scope_digest
ON memory_entries(scope, content_digest);
CREATE INDEX IF NOT EXISTS memory_entries_active_updated
ON memory_entries(scope, updated_at DESC) WHERE deleted_at IS NULL;
CREATE TABLE IF NOT EXISTS memory_sources (
    memory_id TEXT NOT NULL REFERENCES memory_entries(memory_id) ON DELETE CASCADE,
    session_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    observed_at TEXT NOT NULL,
    PRIMARY KEY (memory_id, session_id, turn_id)
) STRICT;
CREATE VIRTUAL TABLE IF NOT EXISTS memory_fts USING fts5(
    memory_id UNINDEXED,
    scope UNINDEXED,
    content,
    tokenize = 'unicode61'
);
";

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MemoryConfig {
    database: PathBuf,
    scope: String,
    max_records: usize,
    max_item_characters: usize,
    max_recall_items: usize,
    max_recall_characters: usize,
}

#[lenso::plugin(
    lifecycle,
    configuration_schema = "config.schema.json",
    validate = validate_config
)]
#[derive(Clone, Debug)]
struct SqliteMemoryPlugin {
    #[config]
    config: MemoryConfig,
    provider: Rc<RefCell<Option<SqliteMemory>>>,
}

#[derive(Clone, Debug)]
struct SqliteMemory {
    config: MemoryConfig,
    operation_lock: Rc<RefCell<()>>,
}

#[derive(Debug)]
enum OperationFailure<D> {
    Domain(D),
    Runtime(RuntimeFailure),
}

impl<D> From<D> for OperationFailure<D> {
    fn from(error: D) -> Self {
        Self::Domain(error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MemoryDomain {
    InvalidRequest,
    ContentTooLarge,
}

fn validate_config(config: &MemoryConfig) -> Result<(), RuntimeFailure> {
    if config.database.as_os_str().is_empty()
        || config.scope.trim().is_empty()
        || config.scope.len() > 128
        || !(1..=1_000_000).contains(&config.max_records)
        || !(256..=262_144).contains(&config.max_item_characters)
        || !(1..=64).contains(&config.max_recall_items)
        || !(256..=262_144).contains(&config.max_recall_characters)
    {
        return Err(RuntimeFailure::InvalidResolvedPlan {
            detail: "Memory configuration is invalid".to_owned(),
        });
    }
    Ok(())
}

impl SqliteMemory {
    fn prepare_store(&self) -> Result<(), RuntimeFailure> {
        if let Some(parent) = self
            .config
            .database
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|error| io_failure("create", &error))?;
        }
        if self.config.database.exists() {
            let metadata = fs::symlink_metadata(&self.config.database)
                .map_err(|error| io_failure("inspect", &error))?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(store_failure("Memory database is not a regular file"));
            }
        }
        let connection = self.connect()?;
        let version = connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .map_err(sql_failure)?;
        if !matches!(version, 0 | 1) {
            return Err(store_failure(format!(
                "Memory database schema version {version} is unsupported"
            )));
        }
        connection
            .execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL;")
            .map_err(sql_failure)?;
        connection.execute_batch(SCHEMA).map_err(sql_failure)?;
        if version == 0 {
            connection
                .pragma_update(None, "user_version", 1_i64)
                .map_err(sql_failure)?;
        }
        Ok(())
    }

    fn connect(&self) -> Result<Connection, RuntimeFailure> {
        let connection = Connection::open(&self.config.database).map_err(sql_failure)?;
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(sql_failure)?;
        connection
            .execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = FULL;")
            .map_err(sql_failure)?;
        Ok(connection)
    }

    fn observe(
        &self,
        request: &ObserveRequest,
    ) -> Result<ObserveResponse, OperationFailure<MemoryDomain>> {
        if !valid_id(&request.source.session_id)
            || !valid_id(&request.source.turn_id)
            || request.user_input.trim().is_empty()
            || request.assistant_output.trim().is_empty()
        {
            return Err(MemoryDomain::InvalidRequest.into());
        }
        let content = format!(
            "User: {}\nAssistant: {}",
            normalize(&request.user_input),
            normalize(&request.assistant_output)
        );
        let memory_id = self.store(
            &content,
            500,
            &request.source.session_id,
            &request.source.turn_id,
        )?;
        Ok(ObserveResponse {
            memory_ids: vec![memory_id],
        })
    }

    fn remember(
        &self,
        request: &RememberRequest,
    ) -> Result<RememberResponse, OperationFailure<MemoryDomain>> {
        if !valid_id(&request.session_id)
            || request.content.trim().is_empty()
            || !(0..=1000).contains(&request.confidence_milli)
        {
            return Err(MemoryDomain::InvalidRequest.into());
        }
        let content = normalize(&request.content);
        let memory_id = self.store(
            &content,
            request.confidence_milli,
            &request.session_id,
            "explicit",
        )?;
        Ok(RememberResponse { memory_id })
    }

    fn store(
        &self,
        content: &str,
        confidence_milli: i64,
        session_id: &str,
        turn_id: &str,
    ) -> Result<String, OperationFailure<MemoryDomain>> {
        if content.chars().count() > self.config.max_item_characters {
            return Err(MemoryDomain::ContentTooLarge.into());
        }
        let _operation = self.operation_lock.borrow_mut();
        let mut connection = self.connect().map_err(OperationFailure::Runtime)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| OperationFailure::Runtime(sql_failure(error)))?;
        let content_digest = digest(content);
        let memory_identity = digest(&format!("{}\n{content}", self.config.scope));
        let memory_id = format!("mem-{}", memory_identity.trim_start_matches("sha256:"));
        let now = now().map_err(OperationFailure::Runtime)?;
        transaction
            .execute(
                "INSERT INTO memory_entries(
                    memory_id, scope, content, content_digest, confidence_milli,
                    created_at, updated_at, deleted_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, NULL)
                 ON CONFLICT(scope, content_digest) DO UPDATE SET
                    confidence_milli = MAX(memory_entries.confidence_milli, excluded.confidence_milli),
                    updated_at = excluded.updated_at,
                    deleted_at = NULL",
                params![
                    memory_id,
                    self.config.scope,
                    content,
                    content_digest,
                    confidence_milli,
                    now
                ],
            )
            .map_err(|error| OperationFailure::Runtime(sql_failure(error)))?;
        transaction
            .execute(
                "INSERT INTO memory_sources(memory_id, session_id, turn_id, observed_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(memory_id, session_id, turn_id) DO NOTHING",
                params![memory_id, session_id, turn_id, now],
            )
            .map_err(|error| OperationFailure::Runtime(sql_failure(error)))?;
        refresh_fts(&transaction, &memory_id, &self.config.scope, content)
            .map_err(OperationFailure::Runtime)?;
        enforce_capacity(&transaction, &self.config.scope, self.config.max_records)
            .map_err(OperationFailure::Runtime)?;
        transaction
            .commit()
            .map_err(|error| OperationFailure::Runtime(sql_failure(error)))?;
        Ok(memory_id)
    }

    fn recall(
        &self,
        request: &RecallRequest,
    ) -> Result<RecallResponse, OperationFailure<MemoryDomain>> {
        if !valid_id(&request.session_id)
            || request.query.trim().is_empty()
            || !(1..=64).contains(&request.max_items)
            || !(256..=262_144).contains(&request.max_characters)
        {
            return Err(MemoryDomain::InvalidRequest.into());
        }
        let requested_items = usize::try_from(request.max_items)
            .unwrap_or(usize::MAX)
            .min(self.config.max_recall_items);
        let character_limit = usize::try_from(request.max_characters)
            .unwrap_or(usize::MAX)
            .min(self.config.max_recall_characters);
        let Some(query) = fts_query(&request.query) else {
            return Ok(RecallResponse { items: Vec::new() });
        };
        let _operation = self.operation_lock.borrow_mut();
        let connection = self.connect().map_err(OperationFailure::Runtime)?;
        let mut statement = connection
            .prepare(
                "SELECT e.memory_id, e.content, e.confidence_milli,
                        s.session_id, s.turn_id
                 FROM memory_fts f
                 JOIN memory_entries e ON e.memory_id = f.memory_id
                 JOIN memory_sources s ON s.rowid = (
                     SELECT newest.rowid FROM memory_sources newest
                     WHERE newest.memory_id = e.memory_id
                     ORDER BY newest.observed_at DESC, newest.rowid DESC LIMIT 1
                 )
                 WHERE memory_fts MATCH ?1 AND e.scope = ?2 AND e.deleted_at IS NULL
                 ORDER BY bm25(memory_fts), e.updated_at DESC
                 LIMIT ?3",
            )
            .map_err(|error| OperationFailure::Runtime(sql_failure(error)))?;
        let fetch_limit = i64::try_from(requested_items.saturating_mul(4).max(requested_items))
            .unwrap_or(i64::MAX);
        let rows = statement
            .query_map(params![query, self.config.scope, fetch_limit], |row| {
                Ok(MemoryItem {
                    memory_id: row.get(0)?,
                    content: row.get(1)?,
                    confidence_milli: row.get(2)?,
                    source: MemorySource {
                        session_id: row.get(3)?,
                        turn_id: row.get(4)?,
                    },
                })
            })
            .map_err(|error| OperationFailure::Runtime(sql_failure(error)))?;
        let mut items = Vec::new();
        let mut characters = 0_usize;
        for row in rows {
            let item = row.map_err(|error| OperationFailure::Runtime(sql_failure(error)))?;
            if item.source.session_id == request.session_id {
                continue;
            }
            let item_characters = item.content.chars().count();
            if characters.saturating_add(item_characters) > character_limit {
                continue;
            }
            characters = characters.saturating_add(item_characters);
            items.push(item);
            if items.len() == requested_items {
                break;
            }
        }
        Ok(RecallResponse { items })
    }

    fn forget(
        &self,
        request: ForgetRequest,
    ) -> Result<ForgetResponse, OperationFailure<MemoryDomain>> {
        let ids = request.memory_ids.into_iter().collect::<BTreeSet<_>>();
        if ids.is_empty() || ids.len() > 64 || ids.iter().any(|id| !valid_id(id)) {
            return Err(MemoryDomain::InvalidRequest.into());
        }
        let _operation = self.operation_lock.borrow_mut();
        let mut connection = self.connect().map_err(OperationFailure::Runtime)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| OperationFailure::Runtime(sql_failure(error)))?;
        let timestamp = now().map_err(OperationFailure::Runtime)?;
        let mut forgotten = 0_u32;
        for memory_id in ids {
            let changed = transaction
                .execute(
                    "UPDATE memory_entries SET deleted_at = ?1, updated_at = ?1
                     WHERE memory_id = ?2 AND scope = ?3 AND deleted_at IS NULL",
                    params![timestamp, memory_id, self.config.scope],
                )
                .map_err(|error| OperationFailure::Runtime(sql_failure(error)))?;
            if changed > 0 {
                transaction
                    .execute("DELETE FROM memory_fts WHERE memory_id = ?1", [&memory_id])
                    .map_err(|error| OperationFailure::Runtime(sql_failure(error)))?;
                forgotten = forgotten.saturating_add(1);
            }
        }
        transaction
            .commit()
            .map_err(|error| OperationFailure::Runtime(sql_failure(error)))?;
        Ok(ForgetResponse {
            forgotten: i64::from(forgotten),
        })
    }
}

fn refresh_fts(
    transaction: &Transaction<'_>,
    memory_id: &str,
    scope: &str,
    content: &str,
) -> Result<(), RuntimeFailure> {
    transaction
        .execute("DELETE FROM memory_fts WHERE memory_id = ?1", [memory_id])
        .map_err(sql_failure)?;
    transaction
        .execute(
            "INSERT INTO memory_fts(memory_id, scope, content) VALUES (?1, ?2, ?3)",
            params![memory_id, scope, content],
        )
        .map_err(sql_failure)?;
    Ok(())
}

fn enforce_capacity(
    transaction: &Transaction<'_>,
    scope: &str,
    max_records: usize,
) -> Result<(), RuntimeFailure> {
    let count = transaction
        .query_row(
            "SELECT COUNT(*) FROM memory_entries WHERE scope = ?1 AND deleted_at IS NULL",
            [scope],
            |row| row.get::<_, i64>(0),
        )
        .map_err(sql_failure)?;
    let overflow = usize::try_from(count)
        .unwrap_or(usize::MAX)
        .saturating_sub(max_records);
    if overflow == 0 {
        return Ok(());
    }
    let mut statement = transaction
        .prepare(
            "SELECT memory_id FROM memory_entries
             WHERE scope = ?1 AND deleted_at IS NULL
             ORDER BY updated_at ASC, memory_id ASC LIMIT ?2",
        )
        .map_err(sql_failure)?;
    let ids = statement
        .query_map(
            params![scope, i64::try_from(overflow).unwrap_or(i64::MAX)],
            |row| row.get::<_, String>(0),
        )
        .map_err(sql_failure)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_failure)?;
    drop(statement);
    let timestamp = now()?;
    for memory_id in ids {
        transaction
            .execute(
                "UPDATE memory_entries SET deleted_at = ?1, updated_at = ?1
                 WHERE memory_id = ?2",
                params![timestamp, memory_id],
            )
            .map_err(sql_failure)?;
        transaction
            .execute("DELETE FROM memory_fts WHERE memory_id = ?1", [&memory_id])
            .map_err(sql_failure)?;
    }
    Ok(())
}

fn fts_query(value: &str) -> Option<String> {
    let tokens = value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| token.chars().count() >= 2)
        .take(16)
        .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
        .collect::<Vec<_>>();
    (!tokens.is_empty()).then(|| tokens.join(" OR "))
}

fn normalize(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn valid_id(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 128
}

fn digest(value: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(value.as_bytes()))
}

fn now() -> Result<String, RuntimeFailure> {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| store_failure(format!("failed to format Memory timestamp: {error}")))
}

fn io_failure(operation: &str, error: &std::io::Error) -> RuntimeFailure {
    store_failure(format!("Memory storage {operation} failed: {error}"))
}

#[allow(clippy::needless_pass_by_value)]
fn sql_failure(error: rusqlite::Error) -> RuntimeFailure {
    store_failure(format!("Memory SQLite operation failed: {error}"))
}

fn store_failure(detail: impl Into<String>) -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: detail.into(),
    }
}

fn plugin_result<T, D, E>(
    result: Result<T, OperationFailure<D>>,
    map_domain: impl FnOnce(D) -> E,
) -> PluginResult<T, E> {
    result.map_err(|error| match error {
        OperationFailure::Domain(error) => PluginError::domain(map_domain(error)),
        OperationFailure::Runtime(error) => PluginError::runtime(error),
    })
}

fn observe_error(error: MemoryDomain) -> ObserveError {
    match error {
        MemoryDomain::InvalidRequest => ObserveError::InvalidRequest,
        MemoryDomain::ContentTooLarge => ObserveError::ContentTooLarge,
    }
}

fn recall_error(error: MemoryDomain) -> RecallError {
    match error {
        MemoryDomain::InvalidRequest => RecallError::InvalidRequest,
        MemoryDomain::ContentTooLarge => RecallError::ContentTooLarge,
    }
}

fn remember_error(error: MemoryDomain) -> RememberError {
    match error {
        MemoryDomain::InvalidRequest => RememberError::InvalidRequest,
        MemoryDomain::ContentTooLarge => RememberError::ContentTooLarge,
    }
}

fn forget_error(error: MemoryDomain) -> ForgetError {
    match error {
        MemoryDomain::InvalidRequest => ForgetError::InvalidRequest,
        MemoryDomain::ContentTooLarge => ForgetError::ContentTooLarge,
    }
}

#[lenso::provides(memory_contract::Memory)]
impl SqliteMemoryPlugin {
    async fn observe(
        &self,
        _: Ctx,
        request: ObserveRequest,
    ) -> PluginResult<ObserveResponse, ObserveError> {
        plugin_result(
            self.provider()
                .map_err(PluginError::runtime)?
                .observe(&request),
            observe_error,
        )
    }

    async fn recall(
        &self,
        _: Ctx,
        request: RecallRequest,
    ) -> PluginResult<RecallResponse, RecallError> {
        plugin_result(
            self.provider()
                .map_err(PluginError::runtime)?
                .recall(&request),
            recall_error,
        )
    }

    async fn remember(
        &self,
        _: Ctx,
        request: RememberRequest,
    ) -> PluginResult<RememberResponse, RememberError> {
        plugin_result(
            self.provider()
                .map_err(PluginError::runtime)?
                .remember(&request),
            remember_error,
        )
    }

    async fn forget(
        &self,
        _: Ctx,
        request: ForgetRequest,
    ) -> PluginResult<ForgetResponse, ForgetError> {
        plugin_result(
            self.provider()
                .map_err(PluginError::runtime)?
                .forget(request),
            forget_error,
        )
    }
}

impl SqliteMemoryPlugin {
    fn provider(&self) -> Result<SqliteMemory, RuntimeFailure> {
        self.provider
            .borrow()
            .clone()
            .ok_or(RuntimeFailure::Unavailable {
                capability: memory_contract::CAPABILITY_ID,
            })
    }
}

impl Lifecycle for SqliteMemoryPlugin {
    #[allow(clippy::unused_async_trait_impl)]
    async fn prepare(&self, _: PrepareContext) -> Result<(), RuntimeFailure> {
        let provider = SqliteMemory {
            config: self.config.clone(),
            operation_lock: Rc::new(RefCell::new(())),
        };
        provider.prepare_store()?;
        self.provider.replace(Some(provider));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory(directory: &tempfile::TempDir) -> SqliteMemory {
        let memory = SqliteMemory {
            config: MemoryConfig {
                database: directory.path().join("memory.sqlite3"),
                scope: "test".to_owned(),
                max_records: 100,
                max_item_characters: 4096,
                max_recall_items: 8,
                max_recall_characters: 8192,
            },
            operation_lock: Rc::new(RefCell::new(())),
        };
        memory.prepare_store().unwrap();
        memory
    }

    #[test]
    fn observes_recalls_deduplicates_and_forgets_with_provenance() {
        let directory = tempfile::tempdir().unwrap();
        let memory = memory(&directory);
        let observed = memory
            .observe(&ObserveRequest {
                source: MemorySource {
                    session_id: "session-a".to_owned(),
                    turn_id: "turn-a".to_owned(),
                },
                user_input: "Use SQLite for durable storage.".to_owned(),
                assistant_output: "SQLite with WAL is a good baseline.".to_owned(),
            })
            .unwrap();
        let duplicate = memory
            .observe(&ObserveRequest {
                source: MemorySource {
                    session_id: "session-b".to_owned(),
                    turn_id: "turn-b".to_owned(),
                },
                user_input: "Use SQLite for durable storage.".to_owned(),
                assistant_output: "SQLite with WAL is a good baseline.".to_owned(),
            })
            .unwrap();
        assert_eq!(observed.memory_ids, duplicate.memory_ids);

        let recalled = memory
            .recall(&RecallRequest {
                session_id: "session-c".to_owned(),
                query: "durable SQLite".to_owned(),
                max_items: 4,
                max_characters: 4096,
            })
            .unwrap();
        assert_eq!(recalled.items.len(), 1);
        assert_eq!(recalled.items[0].source.session_id, "session-b");

        let forgotten = memory
            .forget(ForgetRequest {
                memory_ids: observed.memory_ids,
            })
            .unwrap();
        assert_eq!(forgotten.forgotten, 1);
        assert!(
            memory
                .recall(&RecallRequest {
                    session_id: "session-c".to_owned(),
                    query: "durable SQLite".to_owned(),
                    max_items: 4,
                    max_characters: 4096,
                })
                .unwrap()
                .items
                .is_empty()
        );
    }

    #[test]
    fn store_rejects_content_over_the_configured_bound() {
        let directory = tempfile::tempdir().unwrap();
        let memory = memory(&directory);
        let result = memory.remember(&RememberRequest {
            session_id: "session-a".to_owned(),
            content: "x".repeat(4097),
            confidence_milli: 900,
        });
        assert!(matches!(
            result,
            Err(OperationFailure::Domain(MemoryDomain::ContentTooLarge))
        ));
    }
}
