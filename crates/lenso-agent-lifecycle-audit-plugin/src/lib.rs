//! Append-only local audit Adapter for typed Agent lifecycle events.

use std::{
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use fs2::FileExt;
use lenso::prelude::*;
use lenso_capability_agent_lifecycle::{
    self as lifecycle_contract, LifecycleProvider, ObserveRequest, ObserveResponse,
};
use lenso_kernel::{InvocationContext, RuntimeFailure};

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AuditConfig {
    path: PathBuf,
}

fn validate_config(config: &AuditConfig) -> Result<(), RuntimeFailure> {
    if config.path.as_os_str().is_empty() {
        return Err(invalid_plan("lifecycle audit path must not be empty"));
    }
    Ok(())
}

#[lenso::plugin(
    lifecycle,
    configuration_schema = "config.schema.json",
    validate = validate_config
)]
#[derive(Clone, Debug)]
struct LifecycleAuditPlugin {
    #[config]
    config: AuditConfig,
}

#[lenso::provides(lifecycle_contract::Lifecycle)]
impl LifecycleProvider for LifecycleAuditPlugin {
    fn observe(
        &self,
        _: InvocationContext,
        request: ObserveRequest,
    ) -> lenso_kernel::NativeRequestFuture<lifecycle_contract::Lifecycle> {
        let path = self.config.path.clone();
        Box::pin(async move { append_event(&path, &request).map(|()| Ok(ObserveResponse {})) })
    }
}

impl Lifecycle for LifecycleAuditPlugin {
    #[allow(clippy::unused_async_trait_impl)]
    async fn prepare(&self, _: PrepareContext) -> Result<(), RuntimeFailure> {
        prepare_path(&self.config.path)
    }
}

fn prepare_path(path: &Path) -> Result<(), RuntimeFailure> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| invalid_plan("lifecycle audit path must have a parent directory"))?;
    fs::create_dir_all(parent).map_err(io_failure)?;
    if path.exists() {
        let metadata = fs::symlink_metadata(path).map_err(io_failure)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(store_failure("lifecycle audit path is not a regular file"));
        }
    }
    Ok(())
}

fn append_event(path: &Path, request: &ObserveRequest) -> Result<(), RuntimeFailure> {
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)
        .map_err(io_failure)?;
    file.lock_exclusive().map_err(io_failure)?;
    let result = (|| {
        let mut line = serde_json::to_vec(request).map_err(|error| {
            store_failure(format!("failed to encode lifecycle audit event: {error}"))
        })?;
        if existing_event(&file, &request.event_id, &line)? {
            return Ok(());
        }
        line.push(b'\n');
        file.write_all(&line).map_err(io_failure)?;
        file.sync_data().map_err(io_failure)
    })();
    let unlock = file.unlock().map_err(io_failure);
    result.and(unlock)
}

fn existing_event(
    file: &fs::File,
    event_id: &str,
    expected: &[u8],
) -> Result<bool, RuntimeFailure> {
    let mut reader = BufReader::new(file.try_clone().map_err(io_failure)?);
    reader.seek(SeekFrom::Start(0)).map_err(io_failure)?;
    let mut line = Vec::new();
    while reader.read_until(b'\n', &mut line).map_err(io_failure)? != 0 {
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        let stored = serde_json::from_slice::<serde_json::Value>(&line)
            .map_err(|error| store_failure(format!("lifecycle audit is corrupt: {error}")))?;
        if stored.get("event_id").and_then(serde_json::Value::as_str) == Some(event_id) {
            if line == expected {
                return Ok(true);
            }
            return Err(store_failure(
                "lifecycle audit contains a conflicting event ID",
            ));
        }
        line.clear();
    }
    Ok(false)
}

fn io_failure(error: impl std::fmt::Display) -> RuntimeFailure {
    store_failure(format!("lifecycle audit storage failed: {error}"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use lenso_capability_agent_lifecycle::LifecycleEventKind;

    #[test]
    fn audit_appends_one_durable_json_line_per_event() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("audit/events.jsonl");
        prepare_path(&path).unwrap();
        let event = ObserveRequest {
            event_id: "session/session-1/started".to_owned(),
            kind: LifecycleEventKind::SessionStarted,
            session_id: "session-1".to_owned(),
            turn_id: None,
            occurred_at: "2026-08-29T00:00:00Z".to_owned(),
            generation_spec_digest: format!("sha256:{}", "a".repeat(64)),
            payload_json: "{}".to_owned().try_into().unwrap(),
        };
        append_event(&path, &event).unwrap();
        append_event(&path, &event).unwrap();
        let lines = fs::read_to_string(path).unwrap();
        assert_eq!(lines.lines().count(), 1);
        assert!(lines.contains("session_started"));
    }
}
