//! Durable, generation-bound, one-shot approval Tool Hook.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use fs2::FileExt;
use lenso::prelude::*;
use lenso_capability_agent_tool_hook::{
    self as hook_contract, AfterExecuteRequest, AfterExecuteResponse, BeforeExecuteError,
    BeforeExecuteRequest, BeforeExecuteResponse, HookDecision, HookOutcome, ToolHookProvider,
};
use lenso_kernel::{InvocationContext, RuntimeFailure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const GENERATION_DIGEST_EXTENSION: &str = "lenso.app.generation-spec-digest@1";
const STATE_FILE: &str = "approvals.json";
const LOCK_FILE: &str = ".approvals.lock";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovalConfig {
    directory: PathBuf,
    default_decision: PolicyDecision,
    #[serde(default)]
    allow_tools: Vec<String>,
    #[serde(default)]
    ask_tools: Vec<String>,
    #[serde(default)]
    deny_tools: Vec<String>,
    max_records: usize,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum PolicyDecision {
    Allow,
    Ask,
    Deny,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Rejected,
    Consumed,
    Succeeded,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ApprovalRecord {
    pub approval_id: String,
    pub generation_digest: String,
    pub action_digest: String,
    pub tool_name: String,
    pub arguments_json: String,
    pub status: ApprovalStatus,
    pub created_at_unix: i64,
    pub updated_at_unix: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ApprovalState {
    schema_version: u32,
    records: Vec<ApprovalRecord>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApprovalDecision {
    Approve,
    Reject,
}

#[lenso::module(
    lifecycle,
    configuration_schema = "config.schema.json",
    validate = validate_config
)]
#[derive(Clone, Debug)]
struct ApprovalHookModule {
    #[config]
    config: ApprovalConfig,
}

#[lenso::provides(hook_contract::ToolHook)]
impl ToolHookProvider for ApprovalHookModule {
    fn before_execute(
        &self,
        context: InvocationContext,
        request: BeforeExecuteRequest,
    ) -> lenso_kernel::NativeRequestFuture<hook_contract::ToolHookBeforeExecute> {
        let config = self.config.clone();
        Box::pin(async move { before(&config, &context, &request) })
    }

    fn after_execute(
        &self,
        _context: InvocationContext,
        request: AfterExecuteRequest,
    ) -> lenso_kernel::NativeRequestFuture<hook_contract::ToolHookAfterExecute> {
        let directory = self.config.directory.clone();
        Box::pin(async move { after(&directory, &request).map(|()| Ok(AfterExecuteResponse {})) })
    }
}

impl Lifecycle for ApprovalHookModule {
    #[allow(clippy::unused_async_trait_impl)]
    async fn activate(&self, _context: ActivateContext) -> Result<(), RuntimeFailure> {
        prepare_store(&self.config.directory)
    }
}

fn validate_config(config: &ApprovalConfig) -> Result<(), RuntimeFailure> {
    if config.directory.as_os_str().is_empty() {
        return Err(invalid_plan("approval directory must not be empty"));
    }
    if config.max_records == 0 || config.max_records > 10_000 {
        return Err(invalid_plan("max_records must be between 1 and 10000"));
    }
    let mut names = std::collections::BTreeSet::new();
    for name in config
        .allow_tools
        .iter()
        .chain(&config.ask_tools)
        .chain(&config.deny_tools)
    {
        if name.is_empty() || !names.insert(name) {
            return Err(invalid_plan(
                "Tool policy names must be non-empty and disjoint",
            ));
        }
    }
    Ok(())
}

fn before(
    config: &ApprovalConfig,
    context: &InvocationContext,
    request: &BeforeExecuteRequest,
) -> Result<Result<BeforeExecuteResponse, BeforeExecuteError>, RuntimeFailure> {
    let generation_digest = generation_digest(context)?;
    let policy = policy_for(config, &request.tool_name);
    if matches!(policy, PolicyDecision::Allow) {
        return Ok(Ok(response(
            HookDecision::Allow,
            "policy_allow",
            "Tool is allowed",
            "{}",
        )));
    }
    if matches!(policy, PolicyDecision::Deny) {
        return Ok(Ok(response(
            HookDecision::Deny,
            "policy_deny",
            "Tool is denied by policy",
            "{}",
        )));
    }
    let action_digest = action_digest(
        generation_digest,
        &request.tool_name,
        request.arguments_json.as_str(),
    );
    let record = with_state(&config.directory, |state| {
        if let Some(index) = state.records.iter().position(|record| {
            record.generation_digest == generation_digest
                && record.action_digest == action_digest
                && matches!(
                    record.status,
                    ApprovalStatus::Pending | ApprovalStatus::Approved | ApprovalStatus::Rejected
                )
        }) {
            let record = &mut state.records[index];
            if record.status == ApprovalStatus::Approved {
                record.status = ApprovalStatus::Consumed;
                record.updated_at_unix = now();
            }
            return Ok(record.clone());
        }
        if state.records.len() >= config.max_records {
            return Err("approval store reached max_records".to_owned());
        }
        let record = ApprovalRecord {
            approval_id: uuid::Uuid::new_v4().to_string(),
            generation_digest: generation_digest.to_owned(),
            action_digest,
            tool_name: request.tool_name.clone(),
            arguments_json: request.arguments_json.to_string(),
            status: ApprovalStatus::Pending,
            created_at_unix: now(),
            updated_at_unix: now(),
        };
        state.records.push(record.clone());
        Ok(record)
    })?;
    let context_json = serde_json::json!({
        "approval_id": record.approval_id,
        "action_digest": record.action_digest,
    })
    .to_string();
    Ok(Ok(match record.status {
        ApprovalStatus::Consumed => response(
            HookDecision::Allow,
            "approval_consumed",
            "One-shot approval consumed",
            &context_json,
        ),
        ApprovalStatus::Rejected => response(
            HookDecision::Deny,
            "approval_rejected",
            "Approval was rejected",
            &context_json,
        ),
        ApprovalStatus::Pending | ApprovalStatus::Approved => response(
            HookDecision::Ask,
            "approval_required",
            "Tool requires one-shot approval",
            &context_json,
        ),
        ApprovalStatus::Succeeded | ApprovalStatus::Failed => response(
            HookDecision::Ask,
            "approval_required",
            "Previous one-shot approval is spent",
            &context_json,
        ),
    }))
}

fn after(directory: &Path, request: &AfterExecuteRequest) -> Result<(), RuntimeFailure> {
    let context = serde_json::from_str::<serde_json::Value>(request.context_json.as_str())
        .map_err(|_| store_failure("Hook returned invalid approval context"))?;
    let Some(approval_id) = context
        .get("approval_id")
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(());
    };
    with_state(directory, |state| {
        if let Some(record) = state
            .records
            .iter_mut()
            .find(|record| record.approval_id == approval_id)
            && record.status == ApprovalStatus::Consumed
        {
            record.status = if request.outcome == HookOutcome::Success {
                ApprovalStatus::Succeeded
            } else {
                ApprovalStatus::Failed
            };
            record.updated_at_unix = now();
        }
        Ok(())
    })
}

pub fn list_approvals(directory: &Path) -> Result<Vec<ApprovalRecord>, String> {
    prepare_store(directory).map_err(|error| format!("{error:?}"))?;
    with_state(directory, |state| Ok(state.records.clone())).map_err(|error| format!("{error:?}"))
}

pub fn decide_approval(
    directory: &Path,
    approval_id: &str,
    decision: ApprovalDecision,
) -> Result<ApprovalRecord, String> {
    if uuid::Uuid::parse_str(approval_id).is_err() {
        return Err("approval ID is invalid".to_owned());
    }
    prepare_store(directory).map_err(|error| format!("{error:?}"))?;
    with_state(directory, |state| {
        let record = state
            .records
            .iter_mut()
            .find(|record| record.approval_id == approval_id)
            .ok_or_else(|| "approval was not found".to_owned())?;
        if record.status != ApprovalStatus::Pending {
            return Err(format!("approval is already {:?}", record.status));
        }
        record.status = match decision {
            ApprovalDecision::Approve => ApprovalStatus::Approved,
            ApprovalDecision::Reject => ApprovalStatus::Rejected,
        };
        record.updated_at_unix = now();
        Ok(record.clone())
    })
    .map_err(|error| format!("{error:?}"))
}

fn response(
    decision: HookDecision,
    reason_code: &str,
    message: &str,
    context_json: &str,
) -> BeforeExecuteResponse {
    BeforeExecuteResponse {
        decision,
        reason_code: reason_code.to_owned(),
        message: message.to_owned(),
        context_json: context_json
            .to_owned()
            .try_into()
            .expect("static Hook context must be JSON"),
    }
}

fn policy_for(config: &ApprovalConfig, tool_name: &str) -> PolicyDecision {
    if config.deny_tools.iter().any(|name| name == tool_name) {
        PolicyDecision::Deny
    } else if config.ask_tools.iter().any(|name| name == tool_name) {
        PolicyDecision::Ask
    } else if config.allow_tools.iter().any(|name| name == tool_name) {
        PolicyDecision::Allow
    } else {
        config.default_decision
    }
}

fn generation_digest(context: &InvocationContext) -> Result<&str, RuntimeFailure> {
    context
        .extension(GENERATION_DIGEST_EXTENSION)
        .and_then(|value| std::str::from_utf8(value).ok())
        .filter(|value| {
            value.strip_prefix("sha256:").is_some_and(|hash| {
                hash.len() == 64
                    && hash
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
        })
        .ok_or_else(|| store_failure("Tool Hook is missing canonical Generation provenance"))
}

fn action_digest(generation: &str, tool_name: &str, arguments_json: &str) -> String {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "generation_digest": generation,
        "tool_name": tool_name,
        "arguments": serde_json::from_str::<serde_json::Value>(arguments_json)
            .expect("arguments were normalized"),
    }))
    .expect("action is serializable");
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn prepare_store(directory: &Path) -> Result<(), RuntimeFailure> {
    fs::create_dir_all(directory)
        .map_err(|error| store_failure(&format!("failed to create approval store: {error}")))?;
    let metadata = fs::symlink_metadata(directory)
        .map_err(|error| store_failure(&format!("failed to inspect approval store: {error}")))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(store_failure("approval store is not a regular directory"));
    }
    Ok(())
}

fn with_state<T>(
    directory: &Path,
    operation: impl FnOnce(&mut ApprovalState) -> Result<T, String>,
) -> Result<T, RuntimeFailure> {
    prepare_store(directory)?;
    let lock_path = directory.join(LOCK_FILE);
    reject_symlink_or_special_file(&lock_path, "approval lock")?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)
        .map_err(|error| store_failure(&format!("failed to open approval lock: {error}")))?;
    lock.lock_exclusive()
        .map_err(|error| store_failure(&format!("failed to lock approval store: {error}")))?;
    let state_path = directory.join(STATE_FILE);
    reject_symlink_or_special_file(&state_path, "approval state")?;
    let mut state = match fs::read(&state_path) {
        Ok(bytes) => serde_json::from_slice::<ApprovalState>(&bytes)
            .map_err(|error| store_failure(&format!("approval store is corrupt: {error}")))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ApprovalState {
            schema_version: 1,
            records: Vec::new(),
        },
        Err(error) => {
            return Err(store_failure(&format!(
                "failed to read approval store: {error}"
            )));
        }
    };
    if state.schema_version != 1 {
        return Err(store_failure("approval store schema is unsupported"));
    }
    let result = operation(&mut state).map_err(|detail| store_failure(&detail))?;
    let bytes = serde_json::to_vec_pretty(&state)
        .map_err(|error| store_failure(&format!("failed to encode approval store: {error}")))?;
    let temp_path = directory.join(format!(".{STATE_FILE}.{}.tmp", uuid::Uuid::new_v4()));
    let mut temp = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)
        .map_err(|error| store_failure(&format!("failed to create approval state: {error}")))?;
    temp.write_all(&bytes)
        .and_then(|()| temp.sync_all())
        .map_err(|error| store_failure(&format!("failed to persist approval state: {error}")))?;
    fs::rename(&temp_path, &state_path)
        .map_err(|error| store_failure(&format!("failed to commit approval state: {error}")))?;
    FileExt::unlock(&lock)
        .map_err(|error| store_failure(&format!("failed to unlock approval store: {error}")))?;
    Ok(result)
}

fn reject_symlink_or_special_file(path: &Path, label: &str) -> Result<(), RuntimeFailure> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(store_failure(&format!("{label} is not a regular file"))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(store_failure(&format!(
            "failed to inspect {label}: {error}"
        ))),
    }
}

fn now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

fn invalid_plan(detail: &str) -> RuntimeFailure {
    RuntimeFailure::InvalidResolvedPlan {
        detail: detail.to_owned(),
    }
}

fn store_failure(detail: &str) -> RuntimeFailure {
    RuntimeFailure::ModuleFailure {
        detail: detail.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lenso_kernel::CancellationToken;

    fn context() -> InvocationContext {
        InvocationContext::new(1, None, CancellationToken::new())
            .with_extension(
                GENERATION_DIGEST_EXTENSION,
                format!("sha256:{}", "a".repeat(64)).into_bytes(),
            )
            .unwrap()
    }

    fn request() -> BeforeExecuteRequest {
        BeforeExecuteRequest {
            execution_id: uuid::Uuid::new_v4().to_string(),
            tool_name: "create_file".to_owned(),
            arguments_json: r#"{"content":"approved\n","path":"approved-note.txt"}"#
                .to_owned()
                .try_into()
                .unwrap(),
        }
    }

    #[test]
    fn approval_is_exact_one_shot_and_terminal_records_do_not_block_a_new_request() {
        let directory = tempfile::tempdir().unwrap();
        let config = ApprovalConfig {
            directory: directory.path().to_path_buf(),
            default_decision: PolicyDecision::Ask,
            allow_tools: Vec::new(),
            ask_tools: Vec::new(),
            deny_tools: Vec::new(),
            max_records: 10,
        };
        let pending = before(&config, &context(), &request()).unwrap().unwrap();
        assert_eq!(pending.decision, HookDecision::Ask);
        let first = list_approvals(directory.path()).unwrap().pop().unwrap();
        decide_approval(
            directory.path(),
            &first.approval_id,
            ApprovalDecision::Approve,
        )
        .unwrap();

        let allowed = before(&config, &context(), &request()).unwrap().unwrap();
        assert_eq!(allowed.decision, HookDecision::Allow);
        after(
            directory.path(),
            &AfterExecuteRequest {
                execution_id: "execution".to_owned(),
                tool_name: "create_file".to_owned(),
                arguments_json: request().arguments_json,
                context_json: allowed.context_json,
                outcome: HookOutcome::Success,
                content: "created".to_owned(),
                metadata_json: "{}".to_owned().try_into().unwrap(),
                provider_code: String::new(),
            },
        )
        .unwrap();
        assert_eq!(
            list_approvals(directory.path()).unwrap()[0].status,
            ApprovalStatus::Succeeded
        );

        let next = before(&config, &context(), &request()).unwrap().unwrap();
        assert_eq!(next.decision, HookDecision::Ask);
        let records = list_approvals(directory.path()).unwrap();
        assert_eq!(records.len(), 2);
        assert_ne!(records[0].approval_id, records[1].approval_id);
    }

    #[test]
    fn rejected_exact_action_fails_closed_for_the_generation() {
        let directory = tempfile::tempdir().unwrap();
        let config = ApprovalConfig {
            directory: directory.path().to_path_buf(),
            default_decision: PolicyDecision::Ask,
            allow_tools: Vec::new(),
            ask_tools: Vec::new(),
            deny_tools: Vec::new(),
            max_records: 10,
        };
        before(&config, &context(), &request()).unwrap().unwrap();
        let record = list_approvals(directory.path()).unwrap().pop().unwrap();
        decide_approval(
            directory.path(),
            &record.approval_id,
            ApprovalDecision::Reject,
        )
        .unwrap();
        let denied = before(&config, &context(), &request()).unwrap().unwrap();
        assert_eq!(denied.decision, HookDecision::Deny);
    }
}
