//! Durable SQLite-backed Session Plugin.

use std::{
    cell::RefCell,
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
};

use futures::future::ready;
use lenso::prelude::*;
use lenso_agent_session_inspection::{
    InspectedSession, InspectedSessionEvent, SessionInspector, valid_session_id,
};
use lenso_capability_agent_session::{
    self as session_contract, AppendError, AppendErrorRevisionConflictPayload,
    AppendSessionRequest, AppendSessionRequestEventsItem, AppendSessionResponse, ListError,
    ListSessionsRequest, ListSessionsResponse, ListSessionsResponseSessionsItem, OpenError,
    OpenSessionRequest, OpenSessionResponse, ReadError, ReadSessionRequest, ReadSessionResponse,
    ReadSessionResponseEventsItem, ReadSessionResponseEventsItemKind, SessionAppend, SessionList,
    SessionOpen, SessionProvider, SessionRead,
};
use lenso_kernel::{InvocationContext, RuntimeFailure};
use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior, params};

const SCHEMA: &str = r"
PRAGMA foreign_keys = ON;
CREATE TABLE IF NOT EXISTS sessions (
    session_id TEXT PRIMARY KEY NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 0)
) STRICT;
CREATE TABLE IF NOT EXISTS events (
    session_id TEXT NOT NULL REFERENCES sessions(session_id) ON DELETE CASCADE,
    revision INTEGER NOT NULL CHECK (revision > 0),
    event_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    turn_id TEXT,
    occurred_at TEXT NOT NULL,
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    PRIMARY KEY (session_id, revision),
    UNIQUE (session_id, event_id)
) STRICT;
CREATE INDEX IF NOT EXISTS events_turn_started
ON events(session_id, revision) WHERE kind = 'turn_started';
";

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SqliteSessionConfig {
    database: PathBuf,
}

fn validate_config(config: &SqliteSessionConfig) -> Result<(), RuntimeFailure> {
    if config.database.as_os_str().is_empty() {
        return Err(invalid_plan("Session database path must not be empty"));
    }
    Ok(())
}

#[lenso::plugin(
    lifecycle,
    configuration_schema = "config.schema.json",
    validate = validate_config
)]
#[derive(Clone, Debug)]
struct SqliteSessionPlugin {
    #[config]
    config: SqliteSessionConfig,
    provider: Rc<RefCell<Option<SqliteSessionProvider>>>,
}

#[derive(Clone, Debug)]
struct SqliteSessionProvider {
    database: PathBuf,
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

impl SqliteSessionProvider {
    fn prepare_store(&self) -> Result<(), RuntimeFailure> {
        if let Some(parent) = self
            .database
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|error| io_failure("create", &error))?;
        }
        if self.database.exists() {
            let metadata = fs::symlink_metadata(&self.database)
                .map_err(|error| io_failure("inspect", &error))?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err(store_failure("Session database is not a regular file"));
            }
        }
        let connection = self.connect()?;
        let schema_version = connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
            .map_err(sql_failure)?;
        if !matches!(schema_version, 0 | 1) {
            return Err(store_failure(format!(
                "Session database schema version {schema_version} is unsupported"
            )));
        }
        connection
            .execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = FULL;")
            .map_err(sql_failure)?;
        connection.execute_batch(SCHEMA).map_err(sql_failure)?;
        if schema_version == 0 {
            connection
                .pragma_update(None, "user_version", 1_i64)
                .map_err(sql_failure)?;
        }
        Ok(())
    }

    fn connect(&self) -> Result<Connection, RuntimeFailure> {
        let connection = Connection::open(&self.database).map_err(sql_failure)?;
        connection
            .busy_timeout(std::time::Duration::from_secs(5))
            .map_err(sql_failure)?;
        connection
            .execute_batch("PRAGMA foreign_keys = ON; PRAGMA synchronous = FULL;")
            .map_err(sql_failure)?;
        Ok(connection)
    }

    fn open_now(
        &self,
        request: OpenSessionRequest,
    ) -> Result<OpenSessionResponse, OperationFailure<OpenError>> {
        let _operation = self.operation_lock.borrow_mut();
        let connection = self.connect().map_err(OperationFailure::Runtime)?;
        if let Some(session_id) = request.session_id {
            if !valid_session_id(&session_id) {
                return Err(OpenError::InvalidSessionId.into());
            }
            let revision = connection
                .query_row(
                    "SELECT revision FROM sessions WHERE session_id = ?1",
                    [&session_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(|error| OperationFailure::Runtime(sql_failure(error)))?
                .ok_or(OpenError::NotFound)?;
            return Ok(OpenSessionResponse {
                created: false,
                revision: database_revision(revision)
                    .map_err(OperationFailure::Runtime)?
                    .to_string(),
                session_id,
            });
        }
        let session_id = uuid::Uuid::new_v4().to_string();
        connection
            .execute(
                "INSERT INTO sessions(session_id, revision) VALUES (?1, 0)",
                [&session_id],
            )
            .map_err(|error| OperationFailure::Runtime(sql_failure(error)))?;
        Ok(OpenSessionResponse {
            created: true,
            revision: "0".to_owned(),
            session_id,
        })
    }

    fn append_now(
        &self,
        request: AppendSessionRequest,
    ) -> Result<AppendSessionResponse, OperationFailure<AppendError>> {
        let _operation = self.operation_lock.borrow_mut();
        if !valid_session_id(&request.session_id) || request.events.is_empty() {
            return Err(AppendError::InvalidEvent.into());
        }
        let expected = request
            .expected_revision
            .parse::<u64>()
            .map_err(|_| AppendError::InvalidEvent)?;
        let _ = sql_revision(expected)?;
        let proposed = request
            .events
            .into_iter()
            .enumerate()
            .map(|(offset, event)| validate_event(event, expected, offset))
            .collect::<Result<Vec<_>, _>>()?;
        validate_unique_event_ids(&proposed)?;
        let mut connection = self.connect().map_err(OperationFailure::Runtime)?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| OperationFailure::Runtime(sql_failure(error)))?;
        let current = transaction
            .query_row(
                "SELECT revision FROM sessions WHERE session_id = ?1",
                [&request.session_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|error| OperationFailure::Runtime(sql_failure(error)))?
            .ok_or(AppendError::NotFound)?;
        let current = database_revision(current).map_err(OperationFailure::Runtime)?;
        let mut duplicate_count = 0_usize;
        for event in &proposed {
            let existing = transaction
                .query_row(
                    "SELECT kind, turn_id, occurred_at, payload_json FROM events WHERE session_id = ?1 AND event_id = ?2",
                    params![request.session_id, event.event_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?)),
                )
                .optional()
                .map_err(|error| OperationFailure::Runtime(sql_failure(error)))?;
            if let Some(existing) = existing {
                if existing
                    == (
                        event.kind.clone(),
                        event.turn_id.clone(),
                        event.occurred_at.clone(),
                        event.payload_json.clone(),
                    )
                {
                    duplicate_count += 1;
                } else {
                    return Err(AppendError::InvalidEvent.into());
                }
            }
        }
        if duplicate_count == proposed.len() {
            return Ok(AppendSessionResponse {
                revision: current.to_string(),
            });
        }
        if duplicate_count != 0 {
            return Err(AppendError::InvalidEvent.into());
        }
        if current != expected {
            return Err(AppendError::RevisionConflict {
                payload: AppendErrorRevisionConflictPayload {
                    current_revision: current.to_string(),
                },
            }
            .into());
        }
        for event in &proposed {
            let event_revision = sql_revision(event.revision)?;
            transaction.execute(
                "INSERT INTO events(session_id, revision, event_id, kind, turn_id, occurred_at, payload_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![request.session_id, event_revision, event.event_id, event.kind, event.turn_id, event.occurred_at, event.payload_json],
            ).map_err(|error| OperationFailure::Runtime(sql_failure(error)))?;
        }
        let revision = expected
            .checked_add(u64::try_from(proposed.len()).map_err(|_| AppendError::InvalidEvent)?)
            .ok_or(AppendError::InvalidEvent)?;
        let database_revision = sql_revision(revision)?;
        transaction
            .execute(
                "UPDATE sessions SET revision = ?2 WHERE session_id = ?1",
                params![request.session_id, database_revision],
            )
            .map_err(|error| OperationFailure::Runtime(sql_failure(error)))?;
        transaction
            .commit()
            .map_err(|error| OperationFailure::Runtime(sql_failure(error)))?;
        Ok(AppendSessionResponse {
            revision: revision.to_string(),
        })
    }

    fn read_now(
        &self,
        request: ReadSessionRequest,
    ) -> Result<ReadSessionResponse, OperationFailure<ReadError>> {
        let _operation = self.operation_lock.borrow();
        if !valid_session_id(&request.session_id) || !(1..=1000).contains(&request.limit) {
            return Err(ReadError::InvalidCursor.into());
        }
        let after = request
            .after_revision
            .parse::<u64>()
            .map_err(|_| ReadError::InvalidCursor)?;
        let database_after = i64::try_from(after).map_err(|_| ReadError::InvalidCursor)?;
        let mut connection = self.connect().map_err(OperationFailure::Runtime)?;
        let transaction = connection
            .transaction()
            .map_err(|error| OperationFailure::Runtime(sql_failure(error)))?;
        let revision = transaction
            .query_row(
                "SELECT revision FROM sessions WHERE session_id = ?1",
                [&request.session_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(|error| OperationFailure::Runtime(sql_failure(error)))?
            .ok_or(ReadError::NotFound)?;
        let revision = database_revision(revision).map_err(OperationFailure::Runtime)?;
        if after > revision {
            return Err(ReadError::InvalidCursor.into());
        }
        let events = {
            let mut statement = transaction.prepare(
                "SELECT revision, event_id, kind, turn_id, occurred_at, payload_json FROM events WHERE session_id = ?1 AND revision > ?2 ORDER BY revision LIMIT ?3",
            ).map_err(|error| OperationFailure::Runtime(sql_failure(error)))?;
            let rows = statement
                .query_map(
                    params![request.session_id, database_after, request.limit],
                    read_row,
                )
                .map_err(|error| OperationFailure::Runtime(sql_failure(error)))?;
            rows.map(|row| {
                row.map_err(|error| OperationFailure::Runtime(sql_failure(error)))
                    .and_then(|event| read_event(event).map_err(OperationFailure::Domain))
            })
            .collect::<Result<Vec<_>, _>>()?
        };
        transaction
            .commit()
            .map_err(|error| OperationFailure::Runtime(sql_failure(error)))?;
        Ok(ReadSessionResponse {
            session_id: request.session_id,
            revision: revision.to_string(),
            events,
        })
    }

    fn list_now(
        &self,
        request: &ListSessionsRequest,
    ) -> Result<ListSessionsResponse, OperationFailure<ListError>> {
        let _operation = self.operation_lock.borrow();
        if !(1..=100).contains(&request.limit) {
            return Err(ListError::InvalidLimit.into());
        }
        let connection = self.connect().map_err(OperationFailure::Runtime)?;
        let mut statement = connection
            .prepare(
                "SELECT s.session_id, s.revision, e.occurred_at \
                 FROM sessions s \
                 JOIN events e ON e.session_id = s.session_id AND e.revision = s.revision \
                 WHERE s.revision > 0 \
                 ORDER BY e.occurred_at DESC, s.session_id DESC \
                 LIMIT ?1",
            )
            .map_err(|error| OperationFailure::Runtime(sql_failure(error)))?;
        let rows = statement
            .query_map([request.limit], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|error| OperationFailure::Runtime(sql_failure(error)))?;
        let sessions = rows
            .map(|row| {
                let (session_id, revision, updated_at) =
                    row.map_err(|error| OperationFailure::Runtime(sql_failure(error)))?;
                let revision = database_revision(revision).map_err(OperationFailure::Runtime)?;
                Ok(ListSessionsResponseSessionsItem {
                    revision: revision.to_string(),
                    session_id,
                    updated_at,
                })
            })
            .collect::<Result<Vec<_>, OperationFailure<ListError>>>()?;
        Ok(ListSessionsResponse { sessions })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StoredEvent {
    revision: u64,
    event_id: String,
    kind: String,
    turn_id: Option<String>,
    occurred_at: String,
    payload_json: String,
}

fn validate_event(
    event: AppendSessionRequestEventsItem,
    expected: u64,
    offset: usize,
) -> Result<StoredEvent, AppendError> {
    if event.event_id.is_empty()
        || event.event_id.len() > 128
        || serde_json::from_str::<serde_json::Value>(event.payload_json.as_str()).is_err()
    {
        return Err(AppendError::InvalidEvent);
    }
    let revision = expected
        .checked_add(
            u64::try_from(offset)
                .map_err(|_| AppendError::InvalidEvent)?
                .checked_add(1)
                .ok_or(AppendError::InvalidEvent)?,
        )
        .ok_or(AppendError::InvalidEvent)?;
    Ok(StoredEvent {
        revision,
        event_id: event.event_id,
        kind: event_kind(&event.kind).to_owned(),
        turn_id: event.turn_id,
        occurred_at: event.occurred_at,
        payload_json: event.payload_json.into_string(),
    })
}

fn validate_unique_event_ids(events: &[StoredEvent]) -> Result<(), AppendError> {
    let unique = events
        .iter()
        .map(|event| event.event_id.as_str())
        .collect::<BTreeSet<_>>();
    if unique.len() == events.len() {
        Ok(())
    } else {
        Err(AppendError::InvalidEvent)
    }
}

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredEvent> {
    let revision = row.get::<_, i64>(0)?;
    Ok(StoredEvent {
        revision: u64::try_from(revision).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Integer,
                Box::new(error),
            )
        })?,
        event_id: row.get(1)?,
        kind: row.get(2)?,
        turn_id: row.get(3)?,
        occurred_at: row.get(4)?,
        payload_json: row.get(5)?,
    })
}

fn read_event(event: StoredEvent) -> Result<ReadSessionResponseEventsItem, ReadError> {
    Ok(ReadSessionResponseEventsItem {
        revision: event.revision.to_string(),
        event_id: event.event_id,
        kind: read_event_kind(&event.kind).ok_or(ReadError::InvalidCursor)?,
        turn_id: event.turn_id,
        occurred_at: event.occurred_at,
        payload_json: event
            .payload_json
            .try_into()
            .map_err(|_| ReadError::InvalidCursor)?,
    })
}

fn event_kind(kind: &session_contract::AppendSessionRequestEventsItemKind) -> &'static str {
    use session_contract::AppendSessionRequestEventsItemKind as K;
    match kind {
        K::SessionCreated => "session_created",
        K::SystemInstructionInstalled => "system_instruction_installed",
        K::ContextCompactionStarted => "context_compaction_started",
        K::ContextCompactionCommitted => "context_compaction_committed",
        K::ContextCompactionFailed => "context_compaction_failed",
        K::MemoryRecalled => "memory_recalled",
        K::MemoryRecallFailed => "memory_recall_failed",
        K::MemoryCommitted => "memory_committed",
        K::MemoryCommitFailed => "memory_commit_failed",
        K::TurnStarted => "turn_started",
        K::ModelRequested => "model_requested",
        K::ModelOutput => "model_output",
        K::ToolRequested => "tool_requested",
        K::ToolResult => "tool_result",
        K::TurnCompleted => "turn_completed",
        K::TurnFailed => "turn_failed",
        K::TurnCancelled => "turn_cancelled",
    }
}

fn read_event_kind(kind: &str) -> Option<ReadSessionResponseEventsItemKind> {
    use ReadSessionResponseEventsItemKind as K;
    Some(match kind {
        "session_created" => K::SessionCreated,
        "system_instruction_installed" => K::SystemInstructionInstalled,
        "context_compaction_started" => K::ContextCompactionStarted,
        "context_compaction_committed" => K::ContextCompactionCommitted,
        "context_compaction_failed" => K::ContextCompactionFailed,
        "memory_recalled" => K::MemoryRecalled,
        "memory_recall_failed" => K::MemoryRecallFailed,
        "memory_committed" => K::MemoryCommitted,
        "memory_commit_failed" => K::MemoryCommitFailed,
        "turn_started" => K::TurnStarted,
        "model_requested" => K::ModelRequested,
        "model_output" => K::ModelOutput,
        "tool_requested" => K::ToolRequested,
        "tool_result" => K::ToolResult,
        "turn_completed" => K::TurnCompleted,
        "turn_failed" => K::TurnFailed,
        "turn_cancelled" => K::TurnCancelled,
        _ => return None,
    })
}

impl SessionProvider for SqliteSessionProvider {
    fn append(
        &self,
        _: InvocationContext,
        request: AppendSessionRequest,
    ) -> lenso_kernel::NativeRequestFuture<SessionAppend> {
        Box::pin(ready(native_result(self.append_now(request))))
    }
    fn open(
        &self,
        _: InvocationContext,
        request: OpenSessionRequest,
    ) -> lenso_kernel::NativeRequestFuture<SessionOpen> {
        Box::pin(ready(native_result(self.open_now(request))))
    }
    fn list(
        &self,
        _: InvocationContext,
        request: ListSessionsRequest,
    ) -> lenso_kernel::NativeRequestFuture<SessionList> {
        Box::pin(ready(native_result(self.list_now(&request))))
    }
    fn read(
        &self,
        _: InvocationContext,
        request: ReadSessionRequest,
    ) -> lenso_kernel::NativeRequestFuture<SessionRead> {
        Box::pin(ready(native_result(self.read_now(request))))
    }
}

#[lenso::provides(session_contract::Session)]
impl SessionProvider for SqliteSessionPlugin {
    fn append(
        &self,
        _: InvocationContext,
        request: AppendSessionRequest,
    ) -> lenso_kernel::NativeRequestFuture<SessionAppend> {
        let result = self
            .provider
            .borrow()
            .as_ref()
            .ok_or(RuntimeFailure::Unavailable {
                capability: session_contract::CAPABILITY_ID,
            })
            .and_then(|provider| native_result(provider.append_now(request)));
        Box::pin(ready(result))
    }
    fn open(
        &self,
        _: InvocationContext,
        request: OpenSessionRequest,
    ) -> lenso_kernel::NativeRequestFuture<SessionOpen> {
        let result = self
            .provider
            .borrow()
            .as_ref()
            .ok_or(RuntimeFailure::Unavailable {
                capability: session_contract::CAPABILITY_ID,
            })
            .and_then(|provider| native_result(provider.open_now(request)));
        Box::pin(ready(result))
    }
    fn list(
        &self,
        _: InvocationContext,
        request: ListSessionsRequest,
    ) -> lenso_kernel::NativeRequestFuture<SessionList> {
        let result = self
            .provider
            .borrow()
            .as_ref()
            .ok_or(RuntimeFailure::Unavailable {
                capability: session_contract::CAPABILITY_ID,
            })
            .and_then(|provider| native_result(provider.list_now(&request)));
        Box::pin(ready(result))
    }
    fn read(
        &self,
        _: InvocationContext,
        request: ReadSessionRequest,
    ) -> lenso_kernel::NativeRequestFuture<SessionRead> {
        let result = self
            .provider
            .borrow()
            .as_ref()
            .ok_or(RuntimeFailure::Unavailable {
                capability: session_contract::CAPABILITY_ID,
            })
            .and_then(|provider| native_result(provider.read_now(request)));
        Box::pin(ready(result))
    }
}

impl Lifecycle for SqliteSessionPlugin {
    #[allow(clippy::unused_async_trait_impl)]
    async fn prepare(&self, _: PrepareContext) -> Result<(), RuntimeFailure> {
        let provider = SqliteSessionProvider {
            database: self.config.database.clone(),
            operation_lock: Rc::new(RefCell::new(())),
        };
        provider.prepare_store()?;
        self.provider.replace(Some(provider));
        Ok(())
    }
}

/// Offline inspector for a SQLite Session database.
#[derive(Clone, Debug)]
pub struct SqliteSessionInspector {
    database: PathBuf,
}
impl SqliteSessionInspector {
    pub fn new(database: impl Into<PathBuf>) -> Self {
        Self {
            database: database.into(),
        }
    }
}

impl SessionInspector for SqliteSessionInspector {
    fn inspect_one(&self, session_id: &str) -> Result<InspectedSession, String> {
        inspect_database(&self.database, Some(session_id)).and_then(|mut sessions| {
            sessions
                .pop()
                .ok_or_else(|| format!("Session `{session_id}` was not found"))
        })
    }
    fn inspect_all(&self) -> Result<Vec<InspectedSession>, String> {
        inspect_database(&self.database, None)
    }
}

fn inspect_database(
    path: &Path,
    session_id: Option<&str>,
) -> Result<Vec<InspectedSession>, String> {
    if let Some(session_id) = session_id
        && !valid_session_id(session_id)
    {
        return Err("Session ID is invalid".to_owned());
    }
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("failed to inspect Session database: {error}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Session database is not a regular file".to_owned());
    }
    let mut connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|error| format!("failed to open Session database: {error}"))?;
    let schema_version = connection
        .pragma_query_value(None, "user_version", |row| row.get::<_, i64>(0))
        .map_err(|error| format!("failed to inspect Session database schema: {error}"))?;
    if schema_version != 1 {
        return Err(format!(
            "Session database schema version {schema_version} is unsupported"
        ));
    }
    let transaction = connection
        .transaction()
        .map_err(|error| format!("failed to start Session inspection: {error}"))?;
    let mut sessions = {
        let mut statement = transaction.prepare(
            "SELECT session_id, revision FROM sessions WHERE (?1 IS NULL OR session_id = ?1) ORDER BY session_id",
        )
        .map_err(|error| format!("failed to inspect Sessions: {error}"))?;
        statement
            .query_map(params![session_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|error| format!("failed to inspect Sessions: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("failed to inspect Sessions: {error}"))?
    };
    let mut output = Vec::new();
    for (id, revision) in sessions.drain(..) {
        let mut events_statement = transaction.prepare("SELECT revision, event_id, kind, turn_id, occurred_at, payload_json FROM events WHERE session_id = ?1 ORDER BY revision").map_err(|error| format!("failed to inspect Session events: {error}"))?;
        let events = events_statement
            .query_map([&id], read_row)
            .map_err(|error| format!("failed to inspect Session events: {error}"))?
            .map(|row| {
                row.map(|event| InspectedSessionEvent {
                    revision: event.revision,
                    event_id: event.event_id,
                    kind: event.kind,
                    turn_id: event.turn_id,
                    occurred_at: event.occurred_at,
                    payload_json: event.payload_json,
                })
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("failed to inspect Session events: {error}"))?;
        output.push(InspectedSession {
            session_id: id,
            revision: database_revision(revision).map_err(|error| format!("{error:?}"))?,
            events,
        });
    }
    transaction
        .commit()
        .map_err(|error| format!("failed to finish Session inspection: {error}"))?;
    Ok(output)
}

fn database_revision(value: i64) -> Result<u64, RuntimeFailure> {
    u64::try_from(value).map_err(|_| store_failure("Session database contains an invalid revision"))
}
fn sql_revision(value: u64) -> Result<i64, AppendError> {
    i64::try_from(value).map_err(|_| AppendError::InvalidEvent)
}
fn sql_failure(error: impl std::fmt::Display) -> RuntimeFailure {
    store_failure(format!("Session database operation failed: {error}"))
}
fn io_failure(action: &str, error: &std::io::Error) -> RuntimeFailure {
    store_failure(format!("Session database {action} failed: {error}"))
}
fn store_failure(detail: impl Into<String>) -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: detail.into(),
    }
}
fn invalid_plan(detail: impl Into<String>) -> RuntimeFailure {
    RuntimeFailure::InvalidResolvedPlan {
        detail: detail.into(),
    }
}
fn native_result<T, D>(
    result: Result<T, OperationFailure<D>>,
) -> Result<Result<T, D>, RuntimeFailure> {
    match result {
        Ok(value) => Ok(Ok(value)),
        Err(OperationFailure::Domain(error)) => Ok(Err(error)),
        Err(OperationFailure::Runtime(error)) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lenso_agent_session_inspection::inspect_turn_started;
    use session_contract::AppendSessionRequestEventsItemKind;

    fn provider(database: PathBuf) -> SqliteSessionProvider {
        let provider = SqliteSessionProvider {
            database,
            operation_lock: Rc::new(RefCell::new(())),
        };
        provider.prepare_store().unwrap();
        provider
    }

    fn event(id: &str) -> AppendSessionRequestEventsItem {
        AppendSessionRequestEventsItem {
            event_id: id.to_owned(),
            kind: AppendSessionRequestEventsItemKind::TurnStarted,
            turn_id: Some("turn-1".to_owned()),
            occurred_at: "2026-08-28T00:00:00Z".to_owned(),
            payload_json: format!(
                r#"{{"generation_spec_digest":"sha256:{}","input":"hello"}}"#,
                "a".repeat(64)
            )
            .try_into()
            .unwrap(),
        }
    }

    fn compaction_event(id: &str) -> AppendSessionRequestEventsItem {
        AppendSessionRequestEventsItem {
            event_id: id.to_owned(),
            kind: AppendSessionRequestEventsItemKind::ContextCompactionCommitted,
            turn_id: None,
            occurred_at: "2026-08-28T00:00:01Z".to_owned(),
            payload_json: r#"{"compaction_id":"compact-1"}"#.to_owned().try_into().unwrap(),
        }
    }

    #[test]
    fn session_persists_reopens_and_reads_in_revision_order() {
        let temporary = tempfile::tempdir().unwrap();
        let database = temporary.path().join("sessions.sqlite3");
        let first = provider(database.clone());
        let opened = first
            .open_now(OpenSessionRequest { session_id: None })
            .unwrap();
        let appended = first
            .append_now(AppendSessionRequest {
                session_id: opened.session_id.clone(),
                expected_revision: "0".to_owned(),
                events: vec![event("event-1"), compaction_event("event-2")],
            })
            .unwrap();
        assert_eq!(appended.revision, "2");

        let reopened = provider(database);
        let resumed = reopened
            .open_now(OpenSessionRequest {
                session_id: Some(opened.session_id.clone()),
            })
            .unwrap();
        assert!(!resumed.created);
        assert_eq!(resumed.revision, "2");
        let read = reopened
            .read_now(ReadSessionRequest {
                session_id: opened.session_id,
                after_revision: "0".to_owned(),
                limit: 1000,
            })
            .unwrap();
        assert_eq!(read.events.len(), 2);
        assert_eq!(read.events[0].revision, "1");
        assert_eq!(
            read.events[1].kind,
            ReadSessionResponseEventsItemKind::ContextCompactionCommitted
        );
        assert_eq!(read.events[1].revision, "2");
        let listed = reopened
            .list_now(&ListSessionsRequest { limit: 10 })
            .unwrap();
        assert_eq!(listed.sessions.len(), 1);
        assert_eq!(listed.sessions[0].session_id, read.session_id);
        assert_eq!(listed.sessions[0].updated_at, "2026-08-28T00:00:01Z");
    }

    #[test]
    fn append_is_atomic_idempotent_and_revision_checked() {
        let temporary = tempfile::tempdir().unwrap();
        let provider = provider(temporary.path().join("sessions.sqlite3"));
        let opened = provider
            .open_now(OpenSessionRequest { session_id: None })
            .unwrap();
        let request = AppendSessionRequest {
            session_id: opened.session_id.clone(),
            expected_revision: "0".to_owned(),
            events: vec![event("event-1")],
        };
        assert_eq!(provider.append_now(request.clone()).unwrap().revision, "1");
        assert_eq!(provider.append_now(request).unwrap().revision, "1");

        let conflict = provider
            .append_now(AppendSessionRequest {
                session_id: opened.session_id.clone(),
                expected_revision: "0".to_owned(),
                events: vec![event("event-2")],
            })
            .unwrap_err();
        assert!(matches!(
            conflict,
            OperationFailure::Domain(AppendError::RevisionConflict { .. })
        ));
        let read = provider
            .read_now(ReadSessionRequest {
                session_id: opened.session_id,
                after_revision: "0".to_owned(),
                limit: 1000,
            })
            .unwrap();
        assert_eq!(read.events.len(), 1);

        let duplicate_batch = provider
            .open_now(OpenSessionRequest { session_id: None })
            .unwrap();
        let error = provider
            .append_now(AppendSessionRequest {
                session_id: duplicate_batch.session_id.clone(),
                expected_revision: "0".to_owned(),
                events: vec![event("same"), event("same")],
            })
            .unwrap_err();
        assert!(matches!(
            error,
            OperationFailure::Domain(AppendError::InvalidEvent)
        ));
        assert_eq!(
            provider
                .open_now(OpenSessionRequest {
                    session_id: Some(duplicate_batch.session_id)
                })
                .unwrap()
                .revision,
            "0"
        );
    }

    #[test]
    fn inspection_is_backend_neutral_and_fails_closed() {
        let temporary = tempfile::tempdir().unwrap();
        let database = temporary.path().join("sessions.sqlite3");
        let provider = provider(database.clone());
        let opened = provider
            .open_now(OpenSessionRequest { session_id: None })
            .unwrap();
        provider
            .append_now(AppendSessionRequest {
                session_id: opened.session_id.clone(),
                expected_revision: "0".to_owned(),
                events: vec![event("event-1")],
            })
            .unwrap();

        let inspector = SqliteSessionInspector::new(&database);
        let projected = inspect_turn_started(&inspector, Some(&opened.session_id)).unwrap();
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].revision, 1);

        let connection = Connection::open(database).unwrap();
        connection
            .execute(
                "UPDATE sessions SET revision = 2 WHERE session_id = ?1",
                [&opened.session_id],
            )
            .unwrap();
        let error = inspect_turn_started(&inspector, Some(&opened.session_id)).unwrap_err();
        assert!(error.contains("revision"));
    }

    #[test]
    fn missing_and_invalid_sessions_are_domain_errors() {
        let temporary = tempfile::tempdir().unwrap();
        let provider = provider(temporary.path().join("sessions.sqlite3"));
        assert!(matches!(
            provider.open_now(OpenSessionRequest {
                session_id: Some("missing".to_owned())
            }),
            Err(OperationFailure::Domain(OpenError::NotFound))
        ));
        assert!(matches!(
            provider.open_now(OpenSessionRequest {
                session_id: Some("bad/id".to_owned())
            }),
            Err(OperationFailure::Domain(OpenError::InvalidSessionId))
        ));
    }
}
