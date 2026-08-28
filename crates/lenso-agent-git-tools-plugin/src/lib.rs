//! Semantic Git tools projected over one explicitly bound Process provider.

use std::path::{Component, Path};

use lenso::prelude::*;
use lenso_capability_agent_process::{
    self as process_contract, CatalogRequest as ProcessCatalogRequest, ProcessRunInvocationError,
    RunError, RunRequest, RunResponse,
};
use lenso_capability_agent_tool_provider::{
    self as tool_contract, CatalogRequest, CatalogResponse, ContentType, ExecuteError,
    ExecuteRequest, ExecuteResponse, ExecutionFailedPayload, ToolDefinition, ToolExecutionClass,
};
use lenso_kernel::RuntimeFailure;

pub const STATUS_TOOL: &str = "git_status";
pub const DIFF_TOOL: &str = "git_diff";
pub const LOG_TOOL: &str = "git_log";
pub const STAGE_TOOL: &str = "git_stage";
pub const COMMIT_TOOL: &str = "git_commit";
pub const BRANCHES_TOOL: &str = "git_branches";
pub const BRANCH_CREATE_TOOL: &str = "git_branch_create";
pub const BRANCH_SWITCH_TOOL: &str = "git_branch_switch";
pub const MERGE_TOOL: &str = "git_merge";
pub const REBASE_TOOL: &str = "git_rebase";
pub const FETCH_TOOL: &str = "git_fetch";
pub const PUSH_TOOL: &str = "git_push";

const MAX_PATHS: usize = 256;
const MAX_PATH_BYTES: usize = 4096;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct GitToolsConfig {
    default_timeout_ms: u64,
    max_log_entries: u32,
    max_commit_message_bytes: usize,
    #[serde(default)]
    enable_branch_management: bool,
    #[serde(default)]
    enable_history_integration: bool,
    #[serde(default)]
    allowed_network_remotes: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArguments {}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DiffArguments {
    #[serde(default)]
    staged: bool,
    #[serde(default)]
    paths: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LogArguments {
    max_entries: Option<u32>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StageArguments {
    paths: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CommitArguments {
    message: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RevisionArguments {
    revision: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BranchCreateArguments {
    name: String,
    start_point: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct BranchSwitchArguments {
    name: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteArguments {
    remote: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PushArguments {
    remote: String,
    branch: String,
}

fn validate_config(config: &GitToolsConfig) -> Result<(), RuntimeFailure> {
    if !(1..=600_000).contains(&config.default_timeout_ms) {
        return Err(invalid_plan(
            "default_timeout_ms must be between 1 and 600000",
        ));
    }
    if !(1..=200).contains(&config.max_log_entries) {
        return Err(invalid_plan("max_log_entries must be between 1 and 200"));
    }
    if !(1..=16_384).contains(&config.max_commit_message_bytes) {
        return Err(invalid_plan(
            "max_commit_message_bytes must be between 1 and 16384",
        ));
    }
    if config.allowed_network_remotes.len() > 32
        || config
            .allowed_network_remotes
            .iter()
            .any(|remote| !valid_remote_name(remote))
    {
        return Err(invalid_plan(
            "allowed_network_remotes must contain at most 32 safe remote names",
        ));
    }
    Ok(())
}

#[lenso::plugin(
    lifecycle,
    configuration_schema = "config.schema.json",
    validate = validate_config
)]
#[derive(Clone, Debug)]
struct GitToolsPlugin {
    #[config]
    config: GitToolsConfig,
    process: Port<process_contract::ProcessClient>,
}

impl Lifecycle for GitToolsPlugin {
    async fn activate(&self, _context: ActivateContext) -> Result<(), RuntimeFailure> {
        let catalog = self
            .process
            .catalog(ProcessCatalogRequest {})
            .await
            .map_err(|error| RuntimeFailure::PluginFailure {
                detail: format!("process catalog is unavailable: {error:?}"),
            })?;
        if !catalog.programs.iter().any(|program| program.name == "git") {
            return Err(invalid_plan(
                "Git Tools requires its Process provider to authorize `git`",
            ));
        }
        Ok(())
    }
}

#[lenso::provides(tool_contract::ToolProvider)]
impl GitToolsPlugin {
    fn catalog(
        &self,
        _context: Ctx,
        _request: CatalogRequest,
    ) -> impl std::future::Future<Output = PluginResult<CatalogResponse, tool_contract::CatalogError>>
    {
        let _ = self;
        futures::future::ready(Ok(CatalogResponse {
            tools: tool_definitions(&self.config),
        }))
    }

    async fn execute(
        &self,
        context: Ctx,
        request: ExecuteRequest,
    ) -> PluginResult<ExecuteResponse, ExecuteError> {
        let arguments = command_for(&self.config, &request)?;
        let response = self
            .process
            .run_with_context(
                context,
                RunRequest {
                    program: "git".to_owned(),
                    arguments,
                    cwd: ".".to_owned(),
                    timeout_ms: self.config.default_timeout_ms.to_string(),
                },
            )
            .await
            .map_err(map_process_invocation_error)?;
        response_for(request.name.as_str(), response)
    }
}

fn tool_definitions(config: &GitToolsConfig) -> Vec<ToolDefinition> {
    let mut tools = base_tool_definitions();
    if config.enable_branch_management {
        tools.extend(branch_tool_definitions());
    }
    if config.enable_history_integration {
        tools.extend(history_tool_definitions());
    }
    if !config.allowed_network_remotes.is_empty() {
        tools.extend(network_tool_definitions(&config.allowed_network_remotes));
    }
    tools
}

fn base_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        tool(
            STATUS_TOOL,
            "Show the repository branch and concise working-tree status.",
            &object_schema(&serde_json::json!({}), &[]),
            ToolExecutionClass::ParallelSafe,
        ),
        tool(
            DIFF_TOOL,
            "Show a bounded working-tree or staged diff for optional literal paths.",
            &object_schema(
                &serde_json::json!({
                    "staged": { "type": "boolean", "default": false },
                    "paths": path_array_schema(0)
                }),
                &[],
            ),
            ToolExecutionClass::ParallelSafe,
        ),
        tool(
            LOG_TOOL,
            "Show recent commits with stable hashes, timestamps, authors, and subjects.",
            &object_schema(
                &serde_json::json!({
                    "max_entries": { "type": "integer", "minimum": 1, "maximum": 200 }
                }),
                &[],
            ),
            ToolExecutionClass::ParallelSafe,
        ),
        tool(
            STAGE_TOOL,
            "Stage explicit literal repository-relative paths. This never stages the whole repository implicitly.",
            &object_schema(
                &serde_json::json!({ "paths": path_array_schema(1) }),
                &["paths"],
            ),
            ToolExecutionClass::Exclusive,
        ),
        tool(
            COMMIT_TOOL,
            "Commit only the already staged changes with one bounded message. Repository hooks and signing are disabled.",
            &object_schema(
                &serde_json::json!({
                    "message": { "type": "string", "minLength": 1, "maxLength": 16384 }
                }),
                &["message"],
            ),
            ToolExecutionClass::Exclusive,
        ),
    ]
}

fn branch_tool_definitions() -> [ToolDefinition; 3] {
    [
        tool(
            BRANCHES_TOOL,
            "List local branches and their commit and upstream identities.",
            &object_schema(&serde_json::json!({}), &[]),
            ToolExecutionClass::ParallelSafe,
        ),
        tool(
            BRANCH_CREATE_TOOL,
            "Create one local branch at an optional validated revision without switching to it.",
            &object_schema(
                &serde_json::json!({
                    "name": reference_schema(),
                    "start_point": reference_schema()
                }),
                &["name"],
            ),
            ToolExecutionClass::Exclusive,
        ),
        tool(
            BRANCH_SWITCH_TOOL,
            "Switch to one existing local branch. This never discards working-tree changes.",
            &object_schema(
                &serde_json::json!({ "name": reference_schema() }),
                &["name"],
            ),
            ToolExecutionClass::Exclusive,
        ),
    ]
}

fn history_tool_definitions() -> [ToolDefinition; 2] {
    [
        tool(
            MERGE_TOOL,
            "Merge one validated revision into the current branch without opening an editor.",
            &object_schema(
                &serde_json::json!({ "revision": reference_schema() }),
                &["revision"],
            ),
            ToolExecutionClass::Exclusive,
        ),
        tool(
            REBASE_TOOL,
            "Rebase the current branch onto one validated revision without interactive commands.",
            &object_schema(
                &serde_json::json!({ "revision": reference_schema() }),
                &["revision"],
            ),
            ToolExecutionClass::Exclusive,
        ),
    ]
}

fn network_tool_definitions(allowed_remotes: &[String]) -> [ToolDefinition; 2] {
    let remote_schema = serde_json::json!({
        "type": "string",
        "enum": allowed_remotes
    });
    [
        tool(
            FETCH_TOOL,
            "Fetch refs without tags from one explicitly allowed Git remote.",
            &object_schema(
                &serde_json::json!({ "remote": remote_schema.clone() }),
                &["remote"],
            ),
            ToolExecutionClass::Exclusive,
        ),
        tool(
            PUSH_TOOL,
            "Push the current HEAD without force to one named branch on an explicitly allowed remote.",
            &object_schema(
                &serde_json::json!({
                    "remote": remote_schema,
                    "branch": reference_schema()
                }),
                &["remote", "branch"],
            ),
            ToolExecutionClass::Exclusive,
        ),
    ]
}

fn reference_schema() -> serde_json::Value {
    serde_json::json!({ "type": "string", "minLength": 1, "maxLength": 255 })
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
            .expect("Git Tool schemas must be valid JSON"),
        execution,
    }
}

fn object_schema(properties: &serde_json::Value, required: &[&str]) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
        "required": required
    })
}

fn path_array_schema(min_items: usize) -> serde_json::Value {
    serde_json::json!({
        "type": "array",
        "minItems": min_items,
        "maxItems": MAX_PATHS,
        "items": { "type": "string", "minLength": 1, "maxLength": MAX_PATH_BYTES }
    })
}

fn command_for(
    config: &GitToolsConfig,
    request: &ExecuteRequest,
) -> PluginResult<Vec<String>, ExecuteError> {
    let mut command = safe_git_prefix();
    match request.name.as_str() {
        STATUS_TOOL => {
            decode::<EmptyArguments>(request)?;
            command.extend(strings(["status", "--short", "--branch"]));
        }
        DIFF_TOOL => {
            let arguments = decode::<DiffArguments>(request)?;
            validate_paths(&arguments.paths, false)?;
            command.extend(strings(["diff", "--no-ext-diff", "--no-textconv"]));
            if arguments.staged {
                command.push("--cached".to_owned());
            }
            append_paths(&mut command, arguments.paths);
        }
        LOG_TOOL => {
            let arguments = decode::<LogArguments>(request)?;
            let max_entries = arguments.max_entries.unwrap_or(config.max_log_entries);
            if max_entries == 0 || max_entries > config.max_log_entries {
                return Err(PluginError::domain(ExecuteError::InvalidArguments));
            }
            command.extend(strings([
                "log",
                "--date=iso-strict",
                "--pretty=format:%H%x09%ad%x09%an%x09%s",
            ]));
            command.push(format!("--max-count={max_entries}"));
        }
        STAGE_TOOL => {
            let arguments = decode::<StageArguments>(request)?;
            validate_paths(&arguments.paths, true)?;
            command.extend(strings(["add"]));
            append_paths(&mut command, arguments.paths);
        }
        COMMIT_TOOL => {
            let arguments = decode::<CommitArguments>(request)?;
            let message = arguments.message.trim();
            if message.is_empty()
                || message.len() > config.max_commit_message_bytes
                || message.contains('\0')
            {
                return Err(PluginError::domain(ExecuteError::InvalidArguments));
            }
            command.extend(strings(["commit", "--no-gpg-sign", "--message"]));
            command.push(message.to_owned());
        }
        BRANCHES_TOOL if config.enable_branch_management => {
            decode::<EmptyArguments>(request)?;
            command.extend(strings([
                "branch",
                "--list",
                "--format=%(refname:short)%09%(objectname)%09%(upstream:short)",
            ]));
        }
        BRANCH_CREATE_TOOL if config.enable_branch_management => {
            let arguments = decode::<BranchCreateArguments>(request)?;
            validate_reference(&arguments.name)?;
            command.extend(strings(["branch"]));
            command.push(arguments.name);
            if let Some(start_point) = arguments.start_point {
                validate_reference(&start_point)?;
                command.push(start_point);
            }
        }
        BRANCH_SWITCH_TOOL if config.enable_branch_management => {
            let arguments = decode::<BranchSwitchArguments>(request)?;
            validate_reference(&arguments.name)?;
            command.extend(strings(["switch"]));
            command.push(arguments.name);
        }
        MERGE_TOOL if config.enable_history_integration => {
            let arguments = decode::<RevisionArguments>(request)?;
            validate_reference(&arguments.revision)?;
            command.extend(strings(["merge", "--no-edit", "--no-verify"]));
            command.push(arguments.revision);
        }
        REBASE_TOOL if config.enable_history_integration => {
            let arguments = decode::<RevisionArguments>(request)?;
            validate_reference(&arguments.revision)?;
            command.extend(strings(["rebase"]));
            command.push(arguments.revision);
        }
        FETCH_TOOL if !config.allowed_network_remotes.is_empty() => {
            let arguments = decode::<RemoteArguments>(request)?;
            validate_allowed_remote(config, &arguments.remote)?;
            command.extend(strings(["fetch", "--no-tags"]));
            command.push(arguments.remote);
        }
        PUSH_TOOL if !config.allowed_network_remotes.is_empty() => {
            let arguments = decode::<PushArguments>(request)?;
            validate_allowed_remote(config, &arguments.remote)?;
            validate_reference(&arguments.branch)?;
            command.extend(strings(["push", "--porcelain"]));
            command.push(arguments.remote);
            command.push(format!("HEAD:refs/heads/{}", arguments.branch));
        }
        _ => return Err(PluginError::domain(ExecuteError::NotFound)),
    }
    Ok(command)
}

fn safe_git_prefix() -> Vec<String> {
    strings([
        "--no-pager",
        "--literal-pathspecs",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "commit.gpgSign=false",
        "-c",
        "color.ui=false",
        "-c",
        "core.editor=true",
        "-c",
        "sequence.editor=true",
    ])
}

fn validate_reference(value: &str) -> PluginResult<(), ExecuteError> {
    let invalid = value.is_empty()
        || value.len() > 255
        || value.starts_with(['-', '/', '.'])
        || value.ends_with(['/', '.'])
        || Path::new(value)
            .extension()
            .is_some_and(|extension| extension == "lock")
        || value.contains("..")
        || value.contains("@{")
        || value.contains("//")
        || value.bytes().any(|byte| {
            byte.is_ascii_control()
                || byte.is_ascii_whitespace()
                || matches!(byte, b'~' | b'^' | b':' | b'?' | b'*' | b'[' | b']' | b'\\')
        });
    if invalid {
        return Err(PluginError::domain(ExecuteError::InvalidArguments));
    }
    Ok(())
}

fn valid_remote_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn validate_allowed_remote(
    config: &GitToolsConfig,
    remote: &str,
) -> PluginResult<(), ExecuteError> {
    if !config
        .allowed_network_remotes
        .iter()
        .any(|allowed| allowed == remote)
    {
        return Err(PluginError::domain(ExecuteError::PermissionDenied));
    }
    Ok(())
}

fn decode<T: serde::de::DeserializeOwned>(
    request: &ExecuteRequest,
) -> PluginResult<T, ExecuteError> {
    serde_json::from_str(request.arguments_json.as_str())
        .map_err(|_| PluginError::domain(ExecuteError::InvalidArguments))
}

fn validate_paths(paths: &[String], require_nonempty: bool) -> PluginResult<(), ExecuteError> {
    if paths.len() > MAX_PATHS || (require_nonempty && paths.is_empty()) {
        return Err(PluginError::domain(ExecuteError::InvalidArguments));
    }
    for value in paths {
        let path = Path::new(value);
        if value.is_empty()
            || value.len() > MAX_PATH_BYTES
            || value.contains('\0')
            || path.is_absolute()
            || matches!(value.as_str(), "." | "..")
            || path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(PluginError::domain(ExecuteError::InvalidArguments));
        }
    }
    Ok(())
}

fn append_paths(command: &mut Vec<String>, paths: Vec<String>) {
    if !paths.is_empty() {
        command.push("--".to_owned());
        command.extend(paths);
    }
}

fn strings<const N: usize>(values: [&str; N]) -> Vec<String> {
    values.into_iter().map(str::to_owned).collect()
}

fn response_for(
    tool_name: &str,
    response: RunResponse,
) -> PluginResult<ExecuteResponse, ExecuteError> {
    if response.exit_code != "0" {
        return Err(PluginError::domain(execution_failed(
            "git_failed",
            "Git rejected the requested operation.",
            &serde_json::json!({
                "exit_code": response.exit_code,
                "stderr": response.stderr,
            }),
        )));
    }
    let content = if response.stdout.is_empty() {
        "Git operation completed successfully.".to_owned()
    } else {
        response.stdout
    };
    Ok(ExecuteResponse {
        content,
        content_type: ContentType::Text,
        metadata_json: serde_json::json!({
            "tool": tool_name,
            "exit_code": response.exit_code,
            "duration_ms": response.duration_ms,
        })
        .to_string()
        .try_into()
        .expect("Git Tool metadata must be valid JSON"),
    })
}

fn map_process_invocation_error(error: ProcessRunInvocationError) -> PluginError<ExecuteError> {
    match error {
        ProcessRunInvocationError::Domain(error) => PluginError::domain(match error {
            RunError::InvalidRequest => ExecuteError::InvalidArguments,
            RunError::ProgramNotAllowed | RunError::InvalidWorkingDirectory => {
                ExecuteError::PermissionDenied
            }
            RunError::OutputLimitExceeded => ExecuteError::OutputLimitExceeded,
            RunError::Timeout => {
                execution_failed("git_timeout", "Git timed out.", &serde_json::json!({}))
            }
            RunError::Terminated => execution_failed(
                "git_terminated",
                "Git was terminated.",
                &serde_json::json!({}),
            ),
            RunError::Unknown(detail) => execution_failed(
                "git_unknown",
                "Git execution failed.",
                &serde_json::json!({ "detail": detail }),
            ),
        }),
        ProcessRunInvocationError::Runtime(error) => PluginError::runtime(error),
    }
}

fn execution_failed(reason_code: &str, message: &str, details: &serde_json::Value) -> ExecuteError {
    ExecuteError::ExecutionFailed {
        payload: ExecutionFailedPayload {
            reason_code: reason_code.to_owned(),
            message: message.to_owned(),
            details_json: details
                .to_string()
                .try_into()
                .expect("Git error details must be valid JSON"),
        },
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

    fn config() -> GitToolsConfig {
        GitToolsConfig {
            default_timeout_ms: 30_000,
            max_log_entries: 50,
            max_commit_message_bytes: 4096,
            enable_branch_management: false,
            enable_history_integration: false,
            allowed_network_remotes: Vec::new(),
        }
    }

    fn advanced_config() -> GitToolsConfig {
        GitToolsConfig {
            enable_branch_management: true,
            enable_history_integration: true,
            allowed_network_remotes: vec!["origin".to_owned()],
            ..config()
        }
    }

    fn request(name: &str, arguments: &str) -> ExecuteRequest {
        ExecuteRequest {
            name: name.to_owned(),
            arguments_json: arguments.to_owned().try_into().unwrap(),
        }
    }

    #[test]
    fn descriptor_exposes_one_tool_provider_and_requires_process() {
        let descriptor: serde_json::Value = serde_json::from_str(PLUGIN_DESCRIPTOR_JSON).unwrap();
        assert_eq!(descriptor["plugin_id"], "lenso.agent.git-tools");
        assert_eq!(
            descriptor["required_capabilities"][0]["capability_id"],
            "lenso.agent.process@1"
        );
        assert_eq!(
            descriptor["provided_capabilities"][0]["capability_id"],
            "lenso.agent.tool-provider@2"
        );
    }

    #[test]
    fn catalog_has_three_parallel_reads_and_two_exclusive_mutations() {
        let definitions = tool_definitions(&config());
        assert_eq!(definitions.len(), 5);
        assert!(
            definitions[..3]
                .iter()
                .all(|tool| tool.execution == ToolExecutionClass::ParallelSafe)
        );
        assert!(
            definitions[3..]
                .iter()
                .all(|tool| tool.execution == ToolExecutionClass::Exclusive)
        );
    }

    #[test]
    fn stage_uses_literal_explicit_paths() {
        let command = command_for(
            &config(),
            &request(STAGE_TOOL, r#"{"paths":["src/lib.rs","-literal"]}"#),
        )
        .unwrap();
        assert!(command.starts_with(&safe_git_prefix()));
        assert_eq!(
            &command[command.len() - 4..],
            ["add", "--", "src/lib.rs", "-literal"]
        );
    }

    #[test]
    fn stage_rejects_whole_repository_and_parent_paths() {
        assert!(command_for(&config(), &request(STAGE_TOOL, r#"{"paths":["."]}"#)).is_err());
        assert!(
            command_for(
                &config(),
                &request(STAGE_TOOL, r#"{"paths":["../outside"]}"#)
            )
            .is_err()
        );
    }

    #[test]
    fn commit_disables_hooks_and_signing() {
        let command = command_for(
            &config(),
            &request(COMMIT_TOOL, r#"{"message":"feat: bounded commit"}"#),
        )
        .unwrap();
        assert!(
            command
                .iter()
                .any(|value| value == "core.hooksPath=/dev/null")
        );
        assert!(command.iter().any(|value| value == "commit.gpgSign=false"));
        assert_eq!(
            &command[command.len() - 4..],
            [
                "commit",
                "--no-gpg-sign",
                "--message",
                "feat: bounded commit"
            ]
        );
    }

    #[test]
    fn advanced_tools_are_absent_by_default_and_appear_by_explicit_policy() {
        assert_eq!(tool_definitions(&config()).len(), 5);
        let definitions = tool_definitions(&advanced_config());
        assert_eq!(definitions.len(), 12);
        assert!(definitions.iter().any(|tool| tool.name == REBASE_TOOL));
        assert!(definitions.iter().any(|tool| tool.name == PUSH_TOOL));
    }

    #[test]
    fn push_has_one_allowed_remote_and_a_non_force_refspec() {
        let command = command_for(
            &advanced_config(),
            &request(PUSH_TOOL, r#"{"remote":"origin","branch":"feat/safe"}"#),
        )
        .unwrap();
        assert_eq!(
            &command[command.len() - 4..],
            ["push", "--porcelain", "origin", "HEAD:refs/heads/feat/safe"]
        );
        assert!(
            command_for(
                &advanced_config(),
                &request(PUSH_TOOL, r#"{"remote":"upstream","branch":"main"}"#)
            )
            .is_err()
        );
    }

    #[test]
    fn revision_operations_reject_option_and_revision_expression_injection() {
        for revision in ["--onto", "main..evil", "topic@{upstream}", "bad name"] {
            let arguments = serde_json::json!({ "revision": revision }).to_string();
            assert!(
                command_for(&advanced_config(), &request(REBASE_TOOL, &arguments)).is_err(),
                "expected `{revision}` to be rejected"
            );
        }
    }
}
