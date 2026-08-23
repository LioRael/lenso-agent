//! Durable file-backed Session Module.

use std::{cell::RefCell, fs, path::PathBuf, rc::Rc};

use futures::future::ready;
use lenso_capability_agent_session::{
    AppendError, AppendErrorRevisionConflictPayload, AppendRequest, AppendRequestEventsItem,
    AppendResponse, OpenError, OpenRequest, OpenResponse, ReadError, ReadRequest, ReadResponse,
    ReadResponseEventsItem, ReadResponseEventsItemKind, SessionAppend, SessionEndpoint,
    SessionOpen, SessionProvider, SessionRead,
};
use lenso_kernel::{
    InvocationContext, ModuleFuture, ModuleLifecycle, NativeRequestEndpoint, PrepareContext,
    RuntimeFailure,
};
use lenso_native_adapter::{NativeModuleFactory, NativeModuleFactoryContext, NativeModuleInstance};

/// Runtime package identity selected by App Composition.
pub const PACKAGE_ID: &str = "lenso.agent.session.file";
/// Exact linked package version.
pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FileSessionConfig {
    directory: PathBuf,
}

/// Native factory for a durable file-backed Session store.
#[derive(Clone, Debug, Default)]
pub struct FileSessionFactory;

impl NativeModuleFactory for FileSessionFactory {
    fn package_id(&self) -> &'static str {
        PACKAGE_ID
    }

    fn package_version(&self) -> &'static str {
        PACKAGE_VERSION
    }

    fn instantiate(
        &self,
        context: NativeModuleFactoryContext<'_>,
    ) -> Result<NativeModuleInstance, RuntimeFailure> {
        if context.entrypoint() != "default" {
            return Err(invalid_plan("unsupported file Session entrypoint"));
        }
        let config = serde_json::from_str::<FileSessionConfig>(context.configuration()).map_err(
            |error| invalid_plan(format!("invalid file Session configuration: {error}")),
        )?;
        if config.directory.as_os_str().is_empty() {
            return Err(invalid_plan("Session directory must not be empty"));
        }
        let provider = FileSessionProvider {
            directory: config.directory,
            operation_lock: Rc::new(RefCell::new(())),
        };
        let endpoint =
            Rc::new(SessionEndpoint::new(provider.clone())) as Rc<dyn NativeRequestEndpoint>;
        Ok(NativeModuleInstance::with_lifecycle(
            vec![endpoint],
            FileSessionLifecycle { provider },
        ))
    }
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
            return Err(RuntimeFailure::ModuleFailure {
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
            RuntimeFailure::ModuleFailure {
                detail: format!("Session store is corrupt: {error}"),
            }
        })?;
        if session.schema_version != 1 || session.session_id != session_id {
            return Err(RuntimeFailure::ModuleFailure {
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

    fn open_now(&self, request: OpenRequest) -> Result<OpenResponse, OperationFailure<OpenError>> {
        let _operation = self.operation_lock.borrow_mut();
        if let Some(session_id) = request.session_id {
            if !valid_session_id(&session_id) {
                return Err(OpenError::InvalidSessionId.into());
            }
            let Some(session) = self.load(&session_id).map_err(OperationFailure::Runtime)? else {
                return Err(OpenError::NotFound.into());
            };
            return Ok(OpenResponse {
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
        Ok(OpenResponse {
            created: true,
            revision: "0".to_owned(),
            session_id,
        })
    }

    fn append_now(
        &self,
        request: AppendRequest,
    ) -> Result<AppendResponse, OperationFailure<AppendError>> {
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
            return Ok(AppendResponse {
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
        Ok(AppendResponse {
            revision: session.revision.to_string(),
        })
    }

    fn read_now(&self, request: ReadRequest) -> Result<ReadResponse, OperationFailure<ReadError>> {
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
        Ok(ReadResponse {
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
        request: AppendRequest,
    ) -> lenso_kernel::NativeRequestFuture<SessionAppend> {
        Box::pin(ready(native_result(self.append_now(request))))
    }

    fn open(
        &self,
        _context: InvocationContext,
        request: OpenRequest,
    ) -> lenso_kernel::NativeRequestFuture<SessionOpen> {
        Box::pin(ready(native_result(self.open_now(request))))
    }

    fn read(
        &self,
        _context: InvocationContext,
        request: ReadRequest,
    ) -> lenso_kernel::NativeRequestFuture<SessionRead> {
        Box::pin(ready(native_result(self.read_now(request))))
    }
}

#[derive(Debug)]
struct FileSessionLifecycle {
    provider: FileSessionProvider,
}

impl ModuleLifecycle for FileSessionLifecycle {
    fn prepare(&self, _context: PrepareContext) -> ModuleFuture {
        Box::pin(ready(self.provider.prepare_store()))
    }
}

fn validate_event(
    event: AppendRequestEventsItem,
    expected_revision: u64,
    offset: usize,
) -> Result<StoredEvent, AppendError> {
    if event.event_id.is_empty()
        || event.event_id.len() > 128
        || serde_json::from_str::<serde_json::Value>(&event.payload_json).is_err()
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
        payload_json: event.payload_json,
    })
}

fn read_event(event: StoredEvent) -> Result<ReadResponseEventsItem, ReadError> {
    Ok(ReadResponseEventsItem {
        revision: event.revision.to_string(),
        event_id: event.event_id,
        kind: read_event_kind(&event.kind).ok_or(ReadError::InvalidCursor)?,
        turn_id: event.turn_id,
        occurred_at: event.occurred_at,
        payload_json: event.payload_json,
    })
}

fn event_kind(kind: &lenso_capability_agent_session::AppendRequestEventsItemKind) -> &'static str {
    use lenso_capability_agent_session::AppendRequestEventsItemKind as Kind;
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

fn read_event_kind(kind: &str) -> Option<ReadResponseEventsItemKind> {
    Some(match kind {
        "session_created" => ReadResponseEventsItemKind::SessionCreated,
        "turn_started" => ReadResponseEventsItemKind::TurnStarted,
        "model_requested" => ReadResponseEventsItemKind::ModelRequested,
        "model_output" => ReadResponseEventsItemKind::ModelOutput,
        "tool_requested" => ReadResponseEventsItemKind::ToolRequested,
        "tool_result" => ReadResponseEventsItemKind::ToolResult,
        "turn_completed" => ReadResponseEventsItemKind::TurnCompleted,
        "turn_failed" => ReadResponseEventsItemKind::TurnFailed,
        "turn_cancelled" => ReadResponseEventsItemKind::TurnCancelled,
        _ => return None,
    })
}

fn valid_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id.len() <= 128
        && session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn storage_failure(action: &str, error: &std::io::Error) -> RuntimeFailure {
    RuntimeFailure::ModuleFailure {
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
    use lenso_capability_agent_session::AppendRequestEventsItemKind;

    fn event(id: &str) -> AppendRequestEventsItem {
        AppendRequestEventsItem {
            event_id: id.to_owned(),
            kind: AppendRequestEventsItemKind::TurnStarted,
            turn_id: Some("turn-1".to_owned()),
            occurred_at: "2026-08-24T00:00:00Z".to_owned(),
            payload_json: r#"{"input":"hello"}"#.to_owned(),
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
        let opened = provider.open_now(OpenRequest { session_id: None }).unwrap();
        let appended = provider
            .append_now(AppendRequest {
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
            .open_now(OpenRequest {
                session_id: Some(opened.session_id.clone()),
            })
            .unwrap();
        assert!(!reopened.created);
        assert_eq!(reopened.revision, "1");
        let read = fresh_generation
            .read_now(ReadRequest {
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
        let opened = provider.open_now(OpenRequest { session_id: None }).unwrap();
        let request = AppendRequest {
            session_id: opened.session_id.clone(),
            expected_revision: "0".to_owned(),
            events: vec![event("event-1")],
        };
        provider.append_now(request.clone()).unwrap();
        assert_eq!(provider.append_now(request).unwrap().revision, "1");
        let conflict = provider
            .append_now(AppendRequest {
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
        let opened = provider.open_now(OpenRequest { session_id: None }).unwrap();
        fs::remove_dir_all(&directory).unwrap();
        fs::write(&directory, b"not a directory").unwrap();
        let failure = provider
            .read_now(ReadRequest {
                session_id: opened.session_id,
                after_revision: "0".to_owned(),
                limit: 1,
            })
            .unwrap_err();
        assert!(matches!(failure, OperationFailure::Runtime(_)));
    }
}
