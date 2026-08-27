//! Durable file-backed Session Plugin.

use std::{
    cell::RefCell,
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
};

use futures::future::ready;
use lenso::prelude::*;
use lenso_capability_agent_session::{
    self as session_contract, AppendError, AppendErrorRevisionConflictPayload,
    AppendSessionRequest, AppendSessionRequestEventsItem, AppendSessionResponse, OpenError,
    OpenSessionRequest, OpenSessionResponse, ReadError, ReadSessionRequest, ReadSessionResponse,
    ReadSessionResponseEventsItem, ReadSessionResponseEventsItemKind, SessionAppend, SessionOpen,
    SessionProvider, SessionRead,
};
use lenso_kernel::{InvocationContext, RuntimeFailure};

/// One validated `turn_started` event projected from the private file store.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredTurnStartedEvent {
    /// Durable Session revision.
    pub revision: u64,
    /// Optional Turn identity as stored through the portable Session contract.
    pub turn_id: Option<String>,
    /// Opaque payload owned by the event producer.
    pub payload_json: String,
}

/// One validated `turn_started` event together with its owning Session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredSessionTurnStartedEvent {
    /// Durable Session identity.
    pub session_id: String,
    /// Projected event.
    pub event: StoredTurnStartedEvent,
}

/// Validate one file Session and project only its `turn_started` events.
pub fn inspect_turn_started_events(
    directory: &Path,
    session_id: &str,
) -> Result<Vec<StoredTurnStartedEvent>, String> {
    if !valid_session_id(session_id) {
        return Err("Session ID is invalid".to_owned());
    }
    let directory_metadata = fs::symlink_metadata(directory)
        .map_err(|error| format!("failed to inspect Session directory: {error}"))?;
    if !directory_metadata.is_dir() || directory_metadata.file_type().is_symlink() {
        return Err("Session directory is not a regular directory".to_owned());
    }
    let path = directory.join(format!("{session_id}.json"));
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| format!("failed to inspect Session record: {error}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Session record is not a regular file".to_owned());
    }
    let provider = FileSessionProvider {
        directory: directory.to_path_buf(),
        operation_lock: Rc::new(RefCell::new(())),
    };
    let session = provider
        .load(session_id)
        .map_err(|error| format!("failed to load Session record: {error:?}"))?
        .ok_or_else(|| format!("Session `{session_id}` was not found"))?;
    validate_stored_session(&session)?;
    Ok(session
        .events
        .into_iter()
        .filter(|event| event.kind == "turn_started")
        .map(|event| StoredTurnStartedEvent {
            revision: event.revision,
            turn_id: event.turn_id,
            payload_json: event.payload_json,
        })
        .collect())
}

/// Validate every durable file Session and project its `turn_started` events.
pub fn inspect_all_turn_started_events(
    directory: &Path,
) -> Result<Vec<StoredSessionTurnStartedEvent>, String> {
    let directory_metadata = fs::symlink_metadata(directory)
        .map_err(|error| format!("failed to inspect Session directory: {error}"))?;
    if !directory_metadata.is_dir() || directory_metadata.file_type().is_symlink() {
        return Err("Session directory is not a regular directory".to_owned());
    }
    let mut entries = fs::read_dir(directory)
        .map_err(|error| format!("failed to enumerate Session directory: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("failed to enumerate Session directory: {error}"))?;
    entries.sort_by_key(fs::DirEntry::file_name);

    let mut projected = Vec::new();
    for entry in entries {
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "Session directory contains a non-UTF-8 name".to_owned())?;
        if name.starts_with('.')
            && Path::new(&name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("tmp"))
        {
            continue;
        }
        let session_id = name
            .strip_suffix(".json")
            .filter(|session_id| valid_session_id(session_id))
            .ok_or_else(|| format!("Session entry `{name}` is not a durable Session record"))?;
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| format!("failed to inspect Session record: {error}"))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(format!("Session record `{name}` is not a regular file"));
        }
        projected.extend(
            inspect_turn_started_events(directory, session_id)?
                .into_iter()
                .map(|event| StoredSessionTurnStartedEvent {
                    session_id: session_id.to_owned(),
                    event,
                }),
        );
    }
    Ok(projected)
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FileSessionConfig {
    directory: PathBuf,
}

fn validate_config(config: &FileSessionConfig) -> Result<(), RuntimeFailure> {
    if config.directory.as_os_str().is_empty() {
        return Err(invalid_plan("Session directory must not be empty"));
    }
    Ok(())
}

#[lenso::plugin(
    lifecycle,
    configuration_schema = "config.schema.json",
    validate = validate_config
)]
#[derive(Clone, Debug)]
struct FileSessionPlugin {
    #[config]
    config: FileSessionConfig,
    provider: Rc<RefCell<Option<FileSessionProvider>>>,
}

#[derive(Clone, Debug)]
struct FileSessionProvider {
    directory: PathBuf,
    operation_lock: Rc<RefCell<()>>,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
struct StoredSession {
    schema_version: u32,
    session_id: String,
    revision: u64,
    events: Vec<StoredEvent>,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
struct StoredEvent {
    revision: u64,
    event_id: String,
    kind: String,
    turn_id: Option<String>,
    occurred_at: String,
    payload_json: String,
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

impl FileSessionProvider {
    fn prepare_store(&self) -> Result<(), RuntimeFailure> {
        fs::create_dir_all(&self.directory).map_err(|error| storage_failure("create", &error))?;
        let metadata =
            fs::metadata(&self.directory).map_err(|error| storage_failure("inspect", &error))?;
        if !metadata.is_dir() {
            return Err(RuntimeFailure::PluginFailure {
                detail: "Session storage path is not a directory".to_owned(),
            });
        }
        Ok(())
    }

    fn path_for(&self, session_id: &str) -> PathBuf {
        self.directory.join(format!("{session_id}.json"))
    }

    fn load(&self, session_id: &str) -> Result<Option<StoredSession>, RuntimeFailure> {
        let path = self.path_for(session_id);
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(storage_failure("read", &error)),
        };
        let session = serde_json::from_slice::<StoredSession>(&bytes).map_err(|error| {
            RuntimeFailure::PluginFailure {
                detail: format!("Session store is corrupt: {error}"),
            }
        })?;
        if session.schema_version != 1 || session.session_id != session_id {
            return Err(RuntimeFailure::PluginFailure {
                detail: "Session store identity or schema is invalid".to_owned(),
            });
        }
        Ok(Some(session))
    }

    fn persist(&self, session: &StoredSession) -> Result<(), RuntimeFailure> {
        let bytes =
            serde_json::to_vec_pretty(session).map_err(|error| RuntimeFailure::Internal {
                detail: format!("failed to encode Session state: {error}"),
            })?;
        let temporary = self.directory.join(format!(
            ".{}.{}.tmp",
            session.session_id,
            uuid::Uuid::new_v4()
        ));
        fs::write(&temporary, bytes).map_err(|error| storage_failure("write", &error))?;
        fs::rename(&temporary, self.path_for(&session.session_id))
            .map_err(|error| storage_failure("commit", &error))
    }

    fn open_now(
        &self,
        request: OpenSessionRequest,
    ) -> Result<OpenSessionResponse, OperationFailure<OpenError>> {
        let _operation = self.operation_lock.borrow_mut();
        if let Some(session_id) = request.session_id {
            if !valid_session_id(&session_id) {
                return Err(OpenError::InvalidSessionId.into());
            }
            let Some(session) = self.load(&session_id).map_err(OperationFailure::Runtime)? else {
                return Err(OpenError::NotFound.into());
            };
            return Ok(OpenSessionResponse {
                created: false,
                revision: session.revision.to_string(),
                session_id,
            });
        }
        let session_id = uuid::Uuid::new_v4().to_string();
        let session = StoredSession {
            schema_version: 1,
            session_id: session_id.clone(),
            revision: 0,
            events: Vec::new(),
        };
        self.persist(&session).map_err(OperationFailure::Runtime)?;
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
        let Some(mut session) = self
            .load(&request.session_id)
            .map_err(OperationFailure::Runtime)?
        else {
            return Err(AppendError::NotFound.into());
        };
        let proposed = request
            .events
            .into_iter()
            .enumerate()
            .map(|(offset, event)| validate_event(event, expected, offset))
            .collect::<Result<Vec<_>, _>>()?;
        let mut duplicate_count = 0;
        for event in &proposed {
            if let Some(existing) = session
                .events
                .iter()
                .find(|existing| existing.event_id == event.event_id)
            {
                if existing.event_id == event.event_id
                    && existing.kind == event.kind
                    && existing.turn_id == event.turn_id
                    && existing.occurred_at == event.occurred_at
                    && existing.payload_json == event.payload_json
                {
                    duplicate_count += 1;
                } else {
                    return Err(AppendError::InvalidEvent.into());
                }
            }
        }
        if duplicate_count == proposed.len() {
            return Ok(AppendSessionResponse {
                revision: session.revision.to_string(),
            });
        }
        if duplicate_count != 0 {
            return Err(AppendError::InvalidEvent.into());
        }
        if session.revision != expected {
            return Err(AppendError::RevisionConflict {
                payload: AppendErrorRevisionConflictPayload {
                    current_revision: session.revision.to_string(),
                },
            }
            .into());
        }
        session.revision = session
            .revision
            .checked_add(u64::try_from(proposed.len()).map_err(|_| AppendError::InvalidEvent)?)
            .ok_or(AppendError::InvalidEvent)?;
        session.events.extend(proposed);
        self.persist(&session).map_err(OperationFailure::Runtime)?;
        Ok(AppendSessionResponse {
            revision: session.revision.to_string(),
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
        let Some(session) = self
            .load(&request.session_id)
            .map_err(OperationFailure::Runtime)?
        else {
            return Err(ReadError::NotFound.into());
        };
        if after > session.revision {
            return Err(ReadError::InvalidCursor.into());
        }
        let limit = usize::try_from(request.limit).map_err(|_| ReadError::InvalidCursor)?;
        let events = session
            .events
            .into_iter()
            .filter(|event| event.revision > after)
            .take(limit)
            .map(read_event)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ReadSessionResponse {
            session_id: request.session_id,
            revision: session.revision.to_string(),
            events,
        })
    }
}

impl SessionProvider for FileSessionProvider {
    fn append(
        &self,
        _context: InvocationContext,
        request: AppendSessionRequest,
    ) -> lenso_kernel::NativeRequestFuture<SessionAppend> {
        Box::pin(ready(native_result(self.append_now(request))))
    }

    fn open(
        &self,
        _context: InvocationContext,
        request: OpenSessionRequest,
    ) -> lenso_kernel::NativeRequestFuture<SessionOpen> {
        Box::pin(ready(native_result(self.open_now(request))))
    }

    fn read(
        &self,
        _context: InvocationContext,
        request: ReadSessionRequest,
    ) -> lenso_kernel::NativeRequestFuture<SessionRead> {
        Box::pin(ready(native_result(self.read_now(request))))
    }
}

#[lenso::provides(session_contract::Session)]
impl SessionProvider for FileSessionPlugin {
    fn append(
        &self,
        _context: InvocationContext,
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
        _context: InvocationContext,
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

    fn read(
        &self,
        _context: InvocationContext,
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

impl Lifecycle for FileSessionPlugin {
    #[allow(clippy::unused_async_trait_impl)]
    async fn prepare(&self, _context: PrepareContext) -> Result<(), RuntimeFailure> {
        let provider = FileSessionProvider {
            directory: self.config.directory.clone(),
            operation_lock: Rc::new(RefCell::new(())),
        };
        provider.prepare_store()?;
        self.provider.replace(Some(provider));
        Ok(())
    }
}

fn validate_event(
    event: AppendSessionRequestEventsItem,
    expected_revision: u64,
    offset: usize,
) -> Result<StoredEvent, AppendError> {
    if event.event_id.is_empty()
        || event.event_id.len() > 128
        || serde_json::from_str::<serde_json::Value>(event.payload_json.as_str()).is_err()
    {
        return Err(AppendError::InvalidEvent);
    }
    let increment = u64::try_from(offset)
        .map_err(|_| AppendError::InvalidEvent)?
        .checked_add(1)
        .ok_or(AppendError::InvalidEvent)?;
    let revision = expected_revision
        .checked_add(increment)
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

fn event_kind(
    kind: &lenso_capability_agent_session::AppendSessionRequestEventsItemKind,
) -> &'static str {
    use lenso_capability_agent_session::AppendSessionRequestEventsItemKind as Kind;
    match kind {
        Kind::SessionCreated => "session_created",
        Kind::TurnStarted => "turn_started",
        Kind::ModelRequested => "model_requested",
        Kind::ModelOutput => "model_output",
        Kind::ToolRequested => "tool_requested",
        Kind::ToolResult => "tool_result",
        Kind::TurnCompleted => "turn_completed",
        Kind::TurnFailed => "turn_failed",
        Kind::TurnCancelled => "turn_cancelled",
    }
}

fn read_event_kind(kind: &str) -> Option<ReadSessionResponseEventsItemKind> {
    Some(match kind {
        "session_created" => ReadSessionResponseEventsItemKind::SessionCreated,
        "turn_started" => ReadSessionResponseEventsItemKind::TurnStarted,
        "model_requested" => ReadSessionResponseEventsItemKind::ModelRequested,
        "model_output" => ReadSessionResponseEventsItemKind::ModelOutput,
        "tool_requested" => ReadSessionResponseEventsItemKind::ToolRequested,
        "tool_result" => ReadSessionResponseEventsItemKind::ToolResult,
        "turn_completed" => ReadSessionResponseEventsItemKind::TurnCompleted,
        "turn_failed" => ReadSessionResponseEventsItemKind::TurnFailed,
        "turn_cancelled" => ReadSessionResponseEventsItemKind::TurnCancelled,
        _ => return None,
    })
}

fn validate_stored_session(session: &StoredSession) -> Result<(), String> {
    if session.revision != u64::try_from(session.events.len()).unwrap_or(u64::MAX) {
        return Err("Session revision does not close its event count".to_owned());
    }
    let mut event_ids = BTreeSet::new();
    for (offset, event) in session.events.iter().enumerate() {
        let expected_revision = u64::try_from(offset)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| "Session revision space overflowed".to_owned())?;
        if event.revision != expected_revision
            || !event_ids.insert(&event.event_id)
            || read_event_kind(&event.kind).is_none()
            || serde_json::from_str::<serde_json::Value>(&event.payload_json).is_err()
        {
            return Err("Session event sequence or payload is invalid".to_owned());
        }
    }
    Ok(())
}

fn valid_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id.len() <= 128
        && session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn storage_failure(action: &str, error: &std::io::Error) -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: format!("Session storage {action} failed: {error}"),
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
    use lenso_capability_agent_session::AppendSessionRequestEventsItemKind;

    fn event(id: &str) -> AppendSessionRequestEventsItem {
        AppendSessionRequestEventsItem {
            event_id: id.to_owned(),
            kind: AppendSessionRequestEventsItemKind::TurnStarted,
            turn_id: Some("turn-1".to_owned()),
            occurred_at: "2026-08-24T00:00:00Z".to_owned(),
            payload_json: format!(
                r#"{{"generation_spec_digest":"sha256:{}","input":"hello"}}"#,
                "a".repeat(64)
            )
            .try_into()
            .unwrap(),
        }
    }

    #[test]
    fn session_persists_reopens_and_reads_in_revision_order() {
        let temporary = tempfile::tempdir().unwrap();
        let provider = FileSessionProvider {
            directory: temporary.path().join("sessions"),
            operation_lock: Rc::new(RefCell::new(())),
        };
        provider.prepare_store().unwrap();
        let opened = provider
            .open_now(OpenSessionRequest { session_id: None })
            .unwrap();
        let appended = provider
            .append_now(AppendSessionRequest {
                session_id: opened.session_id.clone(),
                expected_revision: "0".to_owned(),
                events: vec![event("event-1")],
            })
            .unwrap();
        assert_eq!(appended.revision, "1");

        let fresh_generation = FileSessionProvider {
            directory: provider.directory.clone(),
            operation_lock: Rc::new(RefCell::new(())),
        };
        let reopened = fresh_generation
            .open_now(OpenSessionRequest {
                session_id: Some(opened.session_id.clone()),
            })
            .unwrap();
        assert!(!reopened.created);
        assert_eq!(reopened.revision, "1");
        let read = fresh_generation
            .read_now(ReadSessionRequest {
                session_id: opened.session_id,
                after_revision: "0".to_owned(),
                limit: 10,
            })
            .unwrap();
        assert_eq!(read.events.len(), 1);
        assert_eq!(read.events[0].revision, "1");
    }

    #[test]
    fn append_is_idempotent_and_revision_conflicts_are_domain_errors() {
        let temporary = tempfile::tempdir().unwrap();
        let provider = FileSessionProvider {
            directory: temporary.path().join("sessions"),
            operation_lock: Rc::new(RefCell::new(())),
        };
        provider.prepare_store().unwrap();
        let opened = provider
            .open_now(OpenSessionRequest { session_id: None })
            .unwrap();
        let request = AppendSessionRequest {
            session_id: opened.session_id.clone(),
            expected_revision: "0".to_owned(),
            events: vec![event("event-1")],
        };
        provider.append_now(request.clone()).unwrap();
        assert_eq!(provider.append_now(request).unwrap().revision, "1");
        let conflict = provider
            .append_now(AppendSessionRequest {
                session_id: opened.session_id,
                expected_revision: "0".to_owned(),
                events: vec![event("event-2")],
            })
            .unwrap_err();
        assert!(matches!(
            conflict,
            OperationFailure::Domain(AppendError::RevisionConflict { .. })
        ));
    }

    #[test]
    fn storage_loss_remains_a_runtime_failure() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("sessions");
        let provider = FileSessionProvider {
            directory: directory.clone(),
            operation_lock: Rc::new(RefCell::new(())),
        };
        provider.prepare_store().unwrap();
        let opened = provider
            .open_now(OpenSessionRequest { session_id: None })
            .unwrap();
        fs::remove_dir_all(&directory).unwrap();
        fs::write(&directory, b"not a directory").unwrap();
        let failure = provider
            .read_now(ReadSessionRequest {
                session_id: opened.session_id,
                after_revision: "0".to_owned(),
                limit: 1,
            })
            .unwrap_err();
        assert!(matches!(failure, OperationFailure::Runtime(_)));
    }

    #[test]
    fn inspection_validates_the_private_store_and_projects_only_turn_starts() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("sessions");
        let provider = FileSessionProvider {
            directory: directory.clone(),
            operation_lock: Rc::new(RefCell::new(())),
        };
        provider.prepare_store().unwrap();
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

        let events = inspect_turn_started_events(&directory, &opened.session_id).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].revision, 1);
        assert_eq!(events[0].turn_id.as_deref(), Some("turn-1"));
        assert!(events[0].payload_json.contains("generation_spec_digest"));

        let path = directory.join(format!("{}.json", opened.session_id));
        let mut corrupted: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        corrupted["revision"] = 2.into();
        fs::write(path, serde_json::to_vec(&corrupted).unwrap()).unwrap();
        let error = inspect_turn_started_events(&directory, &opened.session_id).unwrap_err();
        assert!(error.contains("does not close"));
    }

    #[test]
    fn all_session_inspection_is_deterministic_and_fails_closed() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("sessions");
        let provider = FileSessionProvider {
            directory: directory.clone(),
            operation_lock: Rc::new(RefCell::new(())),
        };
        provider.prepare_store().unwrap();
        for _ in 0..2 {
            let opened = provider
                .open_now(OpenSessionRequest { session_id: None })
                .unwrap();
            provider
                .append_now(AppendSessionRequest {
                    session_id: opened.session_id,
                    expected_revision: "0".to_owned(),
                    events: vec![event("event-1")],
                })
                .unwrap();
        }
        fs::write(directory.join(".transaction.tmp"), b"in progress").unwrap();

        let events = inspect_all_turn_started_events(&directory).unwrap();
        assert_eq!(events.len(), 2);
        assert!(events[0].session_id < events[1].session_id);

        fs::write(directory.join("unexpected"), b"invalid").unwrap();
        let error = inspect_all_turn_started_events(&directory).unwrap_err();
        assert!(error.contains("not a durable Session record"));
    }
}
