//! Generation-local allocation of isolated Git checkouts for child Agents.

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
    process::Stdio,
    rc::Rc,
};

use lenso::prelude::*;
use lenso_capability_agent_tool_provider::{
    self as tool_contract, CatalogRequest, CatalogResponse, ContentType, ExecuteError,
    ExecuteRequest, ExecuteResponse, ExecutionFailedPayload, ToolDefinition, ToolExecutionClass,
};
use lenso_capability_agent_worktree::{
    self as worktree_contract, AllocateError, AllocateRequest, AllocateResponse,
    WorkspaceAllocationKind,
};
use lenso_kernel::RuntimeFailure;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

pub const LIST_WORKTREES_TOOL: &str = "list_worktrees";
pub const REVIEW_WORKTREE_TOOL: &str = "review_worktree";
pub const INTEGRATE_WORKTREE_TOOL: &str = "integrate_worktree";

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WorktreeConfig {
    repository_root: PathBuf,
    worktree_root: PathBuf,
    mutation_agents: Vec<String>,
    max_worktrees: usize,
    timeout_ms: u64,
    max_review_bytes: usize,
}

#[derive(Clone, Debug)]
struct PreparedPaths {
    repository_root: PathBuf,
    worktree_root: PathBuf,
    git: PathBuf,
    git_target: PathBuf,
}

#[derive(Clone, Debug)]
struct Allocation {
    agent: String,
    workspace: PathBuf,
    branch: String,
    base_commit: String,
    state: AllocationState,
    reviewed: Option<ReviewedRevision>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AllocationState {
    Allocating,
    Ready,
}

#[derive(Clone, Debug)]
struct ReviewedRevision {
    commit: String,
    diff_sha256: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskArguments {
    task_id: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct IntegrateArguments {
    task_id: String,
    reviewed_commit: String,
    diff_sha256: String,
}

#[lenso::plugin(
    lifecycle,
    configuration_schema = "config.schema.json",
    validate = validate_config
)]
#[derive(Clone, Debug)]
struct WorktreeProviderPlugin {
    #[config]
    config: WorktreeConfig,
    prepared: Rc<RefCell<Option<PreparedPaths>>>,
    allocations: Rc<RefCell<BTreeMap<String, Allocation>>>,
}

fn validate_config(config: &WorktreeConfig) -> Result<(), RuntimeFailure> {
    let mut agents = BTreeSet::new();
    if config.mutation_agents.is_empty()
        || config.mutation_agents.len() > 16
        || config
            .mutation_agents
            .iter()
            .any(|agent| agent.is_empty() || agent.len() > 256 || !agents.insert(agent.as_str()))
        || !(1..=32).contains(&config.max_worktrees)
        || !(1..=120_000).contains(&config.timeout_ms)
        || !(1_024..=1_048_576).contains(&config.max_review_bytes)
    {
        return Err(invalid_plan("worktree Provider configuration is invalid"));
    }
    Ok(())
}

impl Lifecycle for WorktreeProviderPlugin {
    #[allow(clippy::unused_async_trait_impl)]
    async fn prepare(&self, _context: PrepareContext) -> Result<(), RuntimeFailure> {
        let repository_root = fs::canonicalize(&self.config.repository_root).map_err(|error| {
            invalid_plan(format!("worktree repository is unavailable: {error}"))
        })?;
        if !repository_root.is_dir() {
            return Err(invalid_plan("worktree repository root is not a directory"));
        }
        fs::create_dir_all(&self.config.worktree_root).map_err(|error| {
            invalid_plan(format!(
                "failed to create worktree allocation root: {error}"
            ))
        })?;
        let worktree_root = fs::canonicalize(&self.config.worktree_root).map_err(|error| {
            invalid_plan(format!("worktree allocation root is invalid: {error}"))
        })?;
        if !worktree_root.is_dir()
            || worktree_root == repository_root
            || worktree_root.starts_with(&repository_root)
        {
            return Err(invalid_plan(
                "worktree allocation root must be a directory outside the source Workspace",
            ));
        }
        let git = resolve_git()?;
        let git_target = fs::canonicalize(&git)
            .map_err(|error| invalid_plan(format!("Git executable is unavailable: {error}")))?;
        self.prepared.replace(Some(PreparedPaths {
            repository_root,
            worktree_root,
            git,
            git_target,
        }));
        Ok(())
    }

    #[allow(clippy::unused_async_trait_impl)]
    async fn activate(&self, _context: ActivateContext) -> Result<(), RuntimeFailure> {
        Ok(())
    }
}

#[lenso::provides(worktree_contract::Worktree, tool_contract::ToolProvider)]
impl WorktreeProviderPlugin {
    async fn allocate(
        &self,
        context: Ctx,
        request: AllocateRequest,
    ) -> PluginResult<AllocateResponse, AllocateError> {
        self.allocate_workspace(context, request).await
    }

    #[allow(
        clippy::unused_self,
        reason = "the Tool Provider contract requires an instance receiver"
    )]
    fn catalog(
        &self,
        _context: Ctx,
        _request: CatalogRequest,
    ) -> impl std::future::Future<Output = PluginResult<CatalogResponse, tool_contract::CatalogError>>
    {
        futures::future::ready(Ok(CatalogResponse {
            tools: vec![
                tool(
                    LIST_WORKTREES_TOOL,
                    "List isolated child worktrees retained by this App Generation.",
                    &empty_schema(),
                    ToolExecutionClass::ParallelSafe,
                ),
                tool(
                    REVIEW_WORKTREE_TOOL,
                    "Review one clean child worktree against its allocation base and lock the exact commit and diff digest for integration.",
                    &task_schema(),
                    ToolExecutionClass::Exclusive,
                ),
                tool(
                    INTEGRATE_WORKTREE_TOOL,
                    "Integrate one previously reviewed child branch into the parent Workspace, then remove its clean checkout.",
                    &integrate_schema(),
                    ToolExecutionClass::Exclusive,
                ),
            ],
        }))
    }

    async fn execute(
        &self,
        context: Ctx,
        request: ExecuteRequest,
    ) -> PluginResult<ExecuteResponse, ExecuteError> {
        match request.name.as_str() {
            LIST_WORKTREES_TOOL => {
                decode_empty(&request)?;
                Ok(self.list_response())
            }
            REVIEW_WORKTREE_TOOL => {
                let arguments = decode::<TaskArguments>(&request)?;
                self.review(context, &arguments.task_id).await
            }
            INTEGRATE_WORKTREE_TOOL => {
                let arguments = decode::<IntegrateArguments>(&request)?;
                self.integrate(context, arguments).await
            }
            _ => Err(PluginError::domain(ExecuteError::NotFound)),
        }
    }
}

impl WorktreeProviderPlugin {
    async fn allocate_workspace(
        &self,
        context: Ctx,
        request: AllocateRequest,
    ) -> PluginResult<AllocateResponse, AllocateError> {
        let paths = self.paths().map_err(PluginError::runtime)?;
        if !valid_task_id(&request.task_id)
            || request.agent.is_empty()
            || request.agent.len() > 256
            || !Path::new(&request.source_workspace).is_absolute()
        {
            return Err(PluginError::domain(AllocateError::InvalidRequest));
        }
        let source = fs::canonicalize(&request.source_workspace)
            .map_err(|_| PluginError::domain(AllocateError::SourceWorkspaceMismatch))?;
        if source != paths.repository_root {
            return Err(PluginError::domain(AllocateError::SourceWorkspaceMismatch));
        }
        if !self.config.mutation_agents.contains(&request.agent) {
            return Ok(AllocateResponse {
                kind: WorkspaceAllocationKind::Current,
                workspace: utf8_path(&paths.repository_root).map_err(PluginError::runtime)?,
                branch: None,
            });
        }

        let branch = format!("lenso/task/{}", request.task_id);
        let workspace = paths.worktree_root.join(&request.task_id);
        {
            let mut allocations = self.allocations.borrow_mut();
            if allocations.contains_key(&request.task_id) {
                return Err(PluginError::domain(AllocateError::TaskAlreadyAllocated));
            }
            if allocations.len() >= self.config.max_worktrees || workspace.exists() {
                return Err(PluginError::domain(AllocateError::CapacityExceeded));
            }
            allocations.insert(
                request.task_id.clone(),
                Allocation {
                    agent: request.agent.clone(),
                    workspace: workspace.clone(),
                    branch: branch.clone(),
                    base_commit: String::new(),
                    state: AllocationState::Allocating,
                    reviewed: None,
                },
            );
        }

        let result = async {
            let base = self
                .run_git(
                    context.clone(),
                    &paths.repository_root,
                    strings(["rev-parse", "HEAD"]),
                )
                .await
                .map_err(PluginError::runtime)?;
            ensure_success(&base)
                .map_err(|()| PluginError::domain(AllocateError::GitOperationFailed))?;
            let base_commit = base.stdout.trim().to_owned();
            if !canonical_commit(&base_commit) {
                return Err(PluginError::runtime(RuntimeFailure::PluginFailure {
                    detail: "worktree Provider received an invalid base commit from Git".to_owned(),
                }));
            }
            let add = self
                .run_git(
                    context,
                    &paths.repository_root,
                    vec![
                        "worktree".to_owned(),
                        "add".to_owned(),
                        "--no-track".to_owned(),
                        "-b".to_owned(),
                        branch.clone(),
                        utf8_path(&workspace).map_err(PluginError::runtime)?,
                        base_commit.clone(),
                    ],
                )
                .await
                .map_err(PluginError::runtime)?;
            ensure_success(&add)
                .map_err(|()| PluginError::domain(AllocateError::GitOperationFailed))?;
            Ok::<_, PluginError<AllocateError>>(base_commit)
        }
        .await;

        match result {
            Ok(base_commit) => {
                if let Some(allocation) = self.allocations.borrow_mut().get_mut(&request.task_id) {
                    allocation.base_commit = base_commit;
                    allocation.state = AllocationState::Ready;
                }
                Ok(AllocateResponse {
                    kind: WorkspaceAllocationKind::IsolatedWorktree,
                    workspace: utf8_path(&workspace).map_err(PluginError::runtime)?,
                    branch: Some(Some(branch)),
                })
            }
            Err(error) => {
                self.allocations.borrow_mut().remove(&request.task_id);
                Err(error)
            }
        }
    }

    async fn review(
        &self,
        context: Ctx,
        task_id: &str,
    ) -> PluginResult<ExecuteResponse, ExecuteError> {
        let allocation = self.ready_allocation(task_id)?;
        let status = self
            .run_git(
                context.clone(),
                &allocation.workspace,
                strings(["status", "--porcelain=v1"]),
            )
            .await
            .map_err(map_runtime_tool_error)?;
        ensure_tool_success(&status, "worktree_status_failed")?;
        if !status.stdout.is_empty() {
            return Err(tool_failed(
                "worktree_uncommitted_changes",
                "Commit or restore every child-worktree change before review",
            ));
        }
        let head = self
            .run_git(
                context.clone(),
                &allocation.workspace,
                strings(["rev-parse", "HEAD"]),
            )
            .await
            .map_err(map_runtime_tool_error)?;
        ensure_tool_success(&head, "worktree_head_failed")?;
        let commit = head.stdout.trim().to_owned();
        if !canonical_commit(&commit) {
            return Err(tool_failed(
                "worktree_head_invalid",
                "Git returned an invalid worktree commit",
            ));
        }
        let diff = self
            .run_git(
                context,
                &allocation.workspace,
                vec![
                    "diff".to_owned(),
                    "--no-ext-diff".to_owned(),
                    "--no-textconv".to_owned(),
                    format!("{}..{commit}", allocation.base_commit),
                ],
            )
            .await
            .map_err(map_runtime_tool_error)?;
        ensure_tool_success(&diff, "worktree_diff_failed")?;
        if diff.stdout.len() > self.config.max_review_bytes {
            return Err(tool_failed(
                "worktree_review_too_large",
                "Worktree diff exceeds the configured review bound",
            ));
        }
        let diff_sha256 = format!("{:x}", Sha256::digest(diff.stdout.as_bytes()));
        if let Some(registered) = self.allocations.borrow_mut().get_mut(task_id) {
            registered.reviewed = Some(ReviewedRevision {
                commit: commit.clone(),
                diff_sha256: diff_sha256.clone(),
            });
        }
        Ok(ExecuteResponse {
            content: diff.stdout,
            content_type: ContentType::Text,
            metadata_json: serde_json::json!({
                "task_id": task_id,
                "agent": allocation.agent,
                "branch": allocation.branch,
                "base_commit": allocation.base_commit,
                "reviewed_commit": commit,
                "diff_sha256": diff_sha256,
            })
            .to_string()
            .try_into()
            .expect("worktree review metadata must be valid JSON"),
        })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "review validation, merge, abort, and cleanup form one fail-closed integration transaction"
    )]
    async fn integrate(
        &self,
        context: Ctx,
        arguments: IntegrateArguments,
    ) -> PluginResult<ExecuteResponse, ExecuteError> {
        if !canonical_commit(&arguments.reviewed_commit)
            || !canonical_sha256(&arguments.diff_sha256)
        {
            return Err(PluginError::domain(ExecuteError::InvalidArguments));
        }
        let allocation = self.ready_allocation(&arguments.task_id)?;
        let Some(reviewed) = allocation.reviewed.as_ref() else {
            return Err(tool_failed(
                "worktree_not_reviewed",
                "Review the exact child worktree before integration",
            ));
        };
        if reviewed.commit != arguments.reviewed_commit
            || reviewed.diff_sha256 != arguments.diff_sha256
        {
            return Err(tool_failed(
                "worktree_review_mismatch",
                "The requested integration does not match the retained review",
            ));
        }
        let head = self
            .run_git(
                context.clone(),
                &allocation.workspace,
                strings(["rev-parse", "HEAD"]),
            )
            .await
            .map_err(map_runtime_tool_error)?;
        ensure_tool_success(&head, "worktree_head_failed")?;
        let child_status = self
            .run_git(
                context.clone(),
                &allocation.workspace,
                strings(["status", "--porcelain=v1"]),
            )
            .await
            .map_err(map_runtime_tool_error)?;
        ensure_tool_success(&child_status, "worktree_status_failed")?;
        if head.stdout.trim() != reviewed.commit || !child_status.stdout.is_empty() {
            return Err(tool_failed(
                "worktree_changed_after_review",
                "The child worktree changed after review",
            ));
        }
        let paths = self.paths().map_err(map_runtime_tool_error)?;
        let parent_status = self
            .run_git(
                context.clone(),
                &paths.repository_root,
                strings(["status", "--porcelain=v1"]),
            )
            .await
            .map_err(map_runtime_tool_error)?;
        ensure_tool_success(&parent_status, "parent_workspace_status_failed")?;
        if !parent_status.stdout.is_empty() {
            return Err(tool_failed(
                "parent_workspace_dirty",
                "The parent Workspace must be clean before integrating a child worktree",
            ));
        }
        let merge = self
            .run_git(
                context.clone(),
                &paths.repository_root,
                vec![
                    "merge".to_owned(),
                    "--no-edit".to_owned(),
                    "--no-verify".to_owned(),
                    "--no-ff".to_owned(),
                    reviewed.commit.clone(),
                ],
            )
            .await
            .map_err(map_runtime_tool_error)?;
        if merge.exit_code != 0 {
            let _ = self
                .run_git(
                    context,
                    &paths.repository_root,
                    strings(["merge", "--abort"]),
                )
                .await;
            return Err(tool_failed(
                "worktree_merge_conflict",
                "Git could not integrate the reviewed child revision",
            ));
        }
        let remove = self
            .run_git(
                context.clone(),
                &paths.repository_root,
                vec![
                    "worktree".to_owned(),
                    "remove".to_owned(),
                    utf8_path(&allocation.workspace).map_err(map_runtime_tool_error)?,
                ],
            )
            .await
            .map_err(map_runtime_tool_error)?;
        ensure_tool_success(&remove, "worktree_remove_failed")?;
        let delete_branch = self
            .run_git(
                context,
                &paths.repository_root,
                vec![
                    "branch".to_owned(),
                    "--delete".to_owned(),
                    allocation.branch.clone(),
                ],
            )
            .await
            .map_err(map_runtime_tool_error)?;
        ensure_tool_success(&delete_branch, "worktree_branch_cleanup_failed")?;
        self.allocations.borrow_mut().remove(&arguments.task_id);
        Ok(ExecuteResponse {
            content: format!("integrated worktree {}", arguments.task_id),
            content_type: ContentType::Text,
            metadata_json: serde_json::json!({
                "task_id": arguments.task_id,
                "reviewed_commit": reviewed.commit,
                "diff_sha256": reviewed.diff_sha256,
                "status": "integrated"
            })
            .to_string()
            .try_into()
            .expect("worktree integration metadata must be valid JSON"),
        })
    }

    fn list_response(&self) -> ExecuteResponse {
        let worktrees = self
            .allocations
            .borrow()
            .iter()
            .filter(|(_, allocation)| allocation.state == AllocationState::Ready)
            .map(|(task_id, allocation)| {
                serde_json::json!({
                    "task_id": task_id,
                    "agent": allocation.agent,
                    "workspace": allocation.workspace,
                    "branch": allocation.branch,
                    "base_commit": allocation.base_commit,
                    "reviewed_commit": allocation.reviewed.as_ref().map(|review| &review.commit),
                    "diff_sha256": allocation.reviewed.as_ref().map(|review| &review.diff_sha256),
                })
            })
            .collect::<Vec<_>>();
        ExecuteResponse {
            content: serde_json::json!({"worktrees": worktrees}).to_string(),
            content_type: ContentType::Text,
            metadata_json: serde_json::json!({"worktree_count": worktrees.len()})
                .to_string()
                .try_into()
                .expect("worktree list metadata must be valid JSON"),
        }
    }

    fn paths(&self) -> Result<PreparedPaths, RuntimeFailure> {
        self.prepared
            .borrow()
            .clone()
            .ok_or(RuntimeFailure::Unavailable {
                capability: worktree_contract::CAPABILITY_ID,
            })
    }

    fn ready_allocation(&self, task_id: &str) -> PluginResult<Allocation, ExecuteError> {
        if !valid_task_id(task_id) {
            return Err(PluginError::domain(ExecuteError::InvalidArguments));
        }
        self.allocations
            .borrow()
            .get(task_id)
            .filter(|allocation| allocation.state == AllocationState::Ready)
            .cloned()
            .ok_or_else(|| {
                tool_failed(
                    "worktree_not_found",
                    "No ready worktree exists for this child task",
                )
            })
    }

    async fn run_git(
        &self,
        context: Ctx,
        cwd: &Path,
        arguments: Vec<String>,
    ) -> Result<GitOutput, RuntimeFailure> {
        let paths = self.paths()?;
        let arguments = git_arguments_for(&paths.repository_root, cwd, arguments)?;
        self.execute_git(context, &paths, arguments).await
    }

    async fn execute_git(
        &self,
        context: Ctx,
        paths: &PreparedPaths,
        arguments: Vec<String>,
    ) -> Result<GitOutput, RuntimeFailure> {
        let current =
            fs::canonicalize(&paths.git).map_err(|error| RuntimeFailure::PluginFailure {
                detail: format!("Git executable is unavailable: {error}"),
            })?;
        if current != paths.git_target {
            return Err(RuntimeFailure::PluginFailure {
                detail: "Git executable identity changed after Worktree readiness".to_owned(),
            });
        }
        let mut command = tokio::process::Command::new(&paths.git);
        command
            .args([
                "--no-pager",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "color.ui=false",
            ])
            .args(arguments)
            .current_dir(&paths.repository_root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|error| RuntimeFailure::PluginFailure {
                detail: format!("bounded Git worktree operation failed to start: {error}"),
            })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| RuntimeFailure::Internal {
                detail: "bounded Git worktree operation has no stdout pipe".to_owned(),
            })?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| RuntimeFailure::Internal {
                detail: "bounded Git worktree operation has no stderr pipe".to_owned(),
            })?;
        let limit = u64::try_from(self.config.max_review_bytes)
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let observe = async move {
            let stdout_read = async move {
                let mut bytes = Vec::new();
                stdout.take(limit).read_to_end(&mut bytes).await?;
                Ok::<_, std::io::Error>(bytes)
            };
            let stderr_read = async move {
                let mut bytes = Vec::new();
                stderr.take(limit).read_to_end(&mut bytes).await?;
                Ok::<_, std::io::Error>(bytes)
            };
            let (stdout, stderr) = tokio::try_join!(stdout_read, stderr_read)?;
            let status = child.wait().await?;
            Ok::<_, std::io::Error>((status, stdout, stderr))
        };
        let cancellation = context.cancellation();
        let output = tokio::select! {
            () = cancellation.cancelled() => return Err(RuntimeFailure::Cancelled { request_id: context.request_id() }),
            result = tokio::time::timeout(std::time::Duration::from_millis(self.config.timeout_ms), observe) => result,
        }
        .map_err(|_| RuntimeFailure::PluginFailure {
            detail: "bounded Git worktree operation timed out".to_owned(),
        })?
        .map_err(|error| RuntimeFailure::PluginFailure {
            detail: format!("bounded Git worktree operation failed: {error}"),
        })?;
        let (status, stdout, stderr) = output;
        if stdout.len() > self.config.max_review_bytes
            || stderr.len() > self.config.max_review_bytes
        {
            return Err(RuntimeFailure::PluginFailure {
                detail: "bounded Git worktree output exceeded its configured limit".to_owned(),
            });
        }
        Ok(GitOutput {
            exit_code: status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
        })
    }
}

#[derive(Debug)]
struct GitOutput {
    exit_code: i32,
    stdout: String,
}

fn resolve_git() -> Result<PathBuf, RuntimeFailure> {
    let search = env::var_os("PATH")
        .ok_or_else(|| invalid_plan("PATH is unavailable while resolving Git"))?;
    env::split_paths(&search)
        .map(|directory| directory.join("git"))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| invalid_plan("Git executable was not found in PATH"))
}

fn git_arguments_for(
    repository_root: &Path,
    cwd: &Path,
    mut arguments: Vec<String>,
) -> Result<Vec<String>, RuntimeFailure> {
    if cwd == repository_root {
        Ok(arguments)
    } else {
        let mut prefixed = vec!["-C".to_owned(), utf8_path(cwd)?];
        prefixed.append(&mut arguments);
        Ok(prefixed)
    }
}

fn tool(
    name: &str,
    description: &str,
    schema: &serde_json::Value,
    execution: ToolExecutionClass,
) -> ToolDefinition {
    ToolDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema_json: schema
            .to_string()
            .try_into()
            .expect("worktree Tool schema must be valid JSON"),
        execution,
    }
}

fn empty_schema() -> serde_json::Value {
    serde_json::json!({"type":"object","additionalProperties":false,"properties":{},"required":[]})
}

fn task_schema() -> serde_json::Value {
    serde_json::json!({"type":"object","additionalProperties":false,"properties":{"task_id":{"type":"string","minLength":1,"maxLength":64}},"required":["task_id"]})
}

fn integrate_schema() -> serde_json::Value {
    serde_json::json!({"type":"object","additionalProperties":false,"properties":{"task_id":{"type":"string","minLength":1,"maxLength":64},"reviewed_commit":{"type":"string","minLength":40,"maxLength":64},"diff_sha256":{"type":"string","minLength":64,"maxLength":64}},"required":["task_id","reviewed_commit","diff_sha256"]})
}

fn decode_empty(request: &ExecuteRequest) -> PluginResult<(), ExecuteError> {
    let value = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(
        request.arguments_json.as_str(),
    )
    .map_err(|_| PluginError::domain(ExecuteError::InvalidArguments))?;
    if value.is_empty() {
        Ok(())
    } else {
        Err(PluginError::domain(ExecuteError::InvalidArguments))
    }
}

fn decode<T: serde::de::DeserializeOwned>(
    request: &ExecuteRequest,
) -> PluginResult<T, ExecuteError> {
    serde_json::from_str(request.arguments_json.as_str())
        .map_err(|_| PluginError::domain(ExecuteError::InvalidArguments))
}

fn valid_task_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn canonical_commit(value: &str) -> bool {
    (40..=64).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn canonical_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn utf8_path(path: &Path) -> Result<String, RuntimeFailure> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| RuntimeFailure::PluginFailure {
            detail: "worktree paths must be valid UTF-8".to_owned(),
        })
}

fn strings<const N: usize>(values: [&str; N]) -> Vec<String> {
    values.into_iter().map(str::to_owned).collect()
}

fn ensure_success(response: &GitOutput) -> Result<(), ()> {
    (response.exit_code == 0).then_some(()).ok_or(())
}

fn ensure_tool_success(response: &GitOutput, code: &str) -> PluginResult<(), ExecuteError> {
    if response.exit_code == 0 {
        Ok(())
    } else {
        Err(tool_failed(code, "The bounded Git operation failed"))
    }
}

fn map_runtime_tool_error(error: RuntimeFailure) -> PluginError<ExecuteError> {
    PluginError::runtime(error)
}

fn tool_failed(code: &str, message: &str) -> PluginError<ExecuteError> {
    PluginError::domain(ExecuteError::ExecutionFailed {
        payload: ExecutionFailedPayload {
            reason_code: code.to_owned(),
            message: message.to_owned(),
            details_json: "{}"
                .to_owned()
                .try_into()
                .expect("static details must be valid JSON"),
        },
    })
}

fn invalid_plan(detail: impl Into<String>) -> RuntimeFailure {
    RuntimeFailure::InvalidResolvedPlan {
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;
    use lenso_kernel::CancellationToken;

    fn git(cwd: &Path, arguments: &[&str]) -> std::process::Output {
        let output = Command::new("git")
            .current_dir(cwd)
            .args(arguments)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn context(request_id: u64) -> Ctx {
        Ctx::new(request_id, None, CancellationToken::new())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn concurrent_allocations_are_isolated_and_integration_requires_exact_review() {
        let temporary = tempfile::tempdir().unwrap();
        let repository_root = temporary.path().join("repository");
        let worktree_root = temporary.path().join("worktrees");
        fs::create_dir_all(&repository_root).unwrap();
        fs::create_dir_all(&worktree_root).unwrap();
        fs::write(repository_root.join("README.md"), "# Worktree Provider\n").unwrap();
        git(&repository_root, &["init", "--quiet"]);
        git(&repository_root, &["config", "user.name", "Lenso Test"]);
        git(
            &repository_root,
            &["config", "user.email", "test@example.invalid"],
        );
        git(&repository_root, &["add", "README.md"]);
        git(&repository_root, &["commit", "--quiet", "-m", "initial"]);

        let repository_root = fs::canonicalize(repository_root).unwrap();
        let worktree_root = fs::canonicalize(worktree_root).unwrap();
        let git_path = resolve_git().unwrap();
        let provider = WorktreeProviderPlugin {
            config: WorktreeConfig {
                repository_root: repository_root.clone(),
                worktree_root: worktree_root.clone(),
                mutation_agents: vec!["worker-a".to_owned(), "worker-b".to_owned()],
                max_worktrees: 2,
                timeout_ms: 30_000,
                max_review_bytes: 65_536,
            },
            prepared: Rc::new(RefCell::new(Some(PreparedPaths {
                repository_root: repository_root.clone(),
                worktree_root,
                git: git_path.clone(),
                git_target: fs::canonicalize(git_path).unwrap(),
            }))),
            allocations: Rc::new(RefCell::new(BTreeMap::new())),
        };
        let request = |task_id: &str, agent: &str| AllocateRequest {
            task_id: task_id.to_owned(),
            agent: agent.to_owned(),
            source_workspace: utf8_path(&repository_root).unwrap(),
        };
        let (worker_a, worker_b) = futures::join!(
            provider.allocate_workspace(context(1), request("task-a", "worker-a")),
            provider.allocate_workspace(context(2), request("task-b", "worker-b")),
        );
        let worker_a = PathBuf::from(worker_a.unwrap().workspace);
        let worker_b = PathBuf::from(worker_b.unwrap().workspace);
        assert_ne!(worker_a, worker_b);

        fs::write(worker_a.join("worker-a.txt"), "worker-a\n").unwrap();
        git(&worker_a, &["add", "worker-a.txt"]);
        git(&worker_a, &["commit", "--quiet", "-m", "test: worker-a"]);
        fs::write(worker_b.join("worker-b.txt"), "worker-b\n").unwrap();
        git(&worker_b, &["add", "worker-b.txt"]);
        git(&worker_b, &["commit", "--quiet", "-m", "test: worker-b"]);
        assert!(!repository_root.join("worker-a.txt").exists());
        assert!(!repository_root.join("worker-b.txt").exists());

        let review = provider.review(context(3), "task-a").await.unwrap();
        assert!(review.content.contains("worker-a.txt"));
        let metadata: serde_json::Value =
            serde_json::from_str(review.metadata_json.as_str()).unwrap();
        let reviewed_commit = metadata["reviewed_commit"].as_str().unwrap().to_owned();
        let diff_sha256 = metadata["diff_sha256"].as_str().unwrap().to_owned();
        assert!(
            provider
                .integrate(
                    context(4),
                    IntegrateArguments {
                        task_id: "task-a".to_owned(),
                        reviewed_commit: reviewed_commit.clone(),
                        diff_sha256: "0".repeat(64),
                    },
                )
                .await
                .is_err()
        );
        provider
            .integrate(
                context(5),
                IntegrateArguments {
                    task_id: "task-a".to_owned(),
                    reviewed_commit,
                    diff_sha256,
                },
            )
            .await
            .unwrap();
        assert_eq!(
            fs::read_to_string(repository_root.join("worker-a.txt")).unwrap(),
            "worker-a\n"
        );
        assert!(!worker_a.exists());
        assert!(worker_b.exists());
        assert!(provider.allocations.borrow().contains_key("task-b"));
        assert!(!provider.allocations.borrow().contains_key("task-a"));
    }
}
