//! Semantic GitHub workflow tools over one explicitly bound `gh` Process provider.

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

pub const ISSUE_GET_TOOL: &str = "github_issue_get";
pub const ISSUE_CREATE_TOOL: &str = "github_issue_create";
pub const ISSUE_COMMENT_TOOL: &str = "github_issue_comment";
pub const ISSUE_CLOSE_TOOL: &str = "github_issue_close";
pub const PR_GET_TOOL: &str = "github_pr_get";
pub const PR_CREATE_TOOL: &str = "github_pr_create";
pub const PR_MERGE_TOOL: &str = "github_pr_merge";
pub const CI_STATUS_TOOL: &str = "github_ci_status";
pub const CI_RERUN_TOOL: &str = "github_ci_rerun";

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct GitHubConfig {
    allowed_repositories: Vec<String>,
    default_timeout_ms: u64,
    #[serde(default)]
    enable_mutations: bool,
    max_body_bytes: usize,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NumberArguments {
    repository: String,
    number: u64,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CommentArguments {
    repository: String,
    number: u64,
    body: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct IssueCreateArguments {
    repository: String,
    title: String,
    #[serde(default)]
    body: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PullRequestCreateArguments {
    repository: String,
    title: String,
    head: String,
    base: String,
    #[serde(default)]
    body: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PullRequestMergeArguments {
    repository: String,
    number: u64,
    method: MergeMethod,
}

#[derive(Clone, Copy, Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum MergeMethod {
    Merge,
    Rebase,
    Squash,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CiStatusArguments {
    repository: String,
    #[serde(default)]
    branch: Option<String>,
    #[serde(default = "default_run_limit")]
    limit: u8,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CiRerunArguments {
    repository: String,
    run_id: u64,
    #[serde(default)]
    failed_only: bool,
}

const fn default_run_limit() -> u8 {
    10
}

fn validate_config(config: &GitHubConfig) -> Result<(), RuntimeFailure> {
    if config.allowed_repositories.is_empty()
        || config.allowed_repositories.len() > 32
        || config
            .allowed_repositories
            .iter()
            .any(|repository| !valid_repository(repository))
        || config
            .allowed_repositories
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        || !(1..=300_000).contains(&config.default_timeout_ms)
        || !(1..=65_536).contains(&config.max_body_bytes)
    {
        return Err(RuntimeFailure::InvalidResolvedPlan {
            detail: "GitHub workflow configuration is invalid or unbounded".to_owned(),
        });
    }
    Ok(())
}

#[lenso::plugin(
    lifecycle,
    configuration_schema = "config.schema.json",
    validate = validate_config
)]
#[derive(Clone, Debug)]
struct GitHubWorkflows {
    #[config]
    config: GitHubConfig,
    process: Port<process_contract::ProcessClient>,
}

impl Lifecycle for GitHubWorkflows {
    async fn activate(&self, _context: ActivateContext) -> Result<(), RuntimeFailure> {
        let catalog = self
            .process
            .catalog(ProcessCatalogRequest {})
            .await
            .map_err(|error| RuntimeFailure::PluginFailure {
                detail: format!("Process catalog is unavailable: {error:?}"),
            })?;
        if !catalog.programs.iter().any(|program| program.name == "gh") {
            return Err(RuntimeFailure::InvalidResolvedPlan {
                detail: "GitHub Workflows requires its Process Provider to authorize `gh`"
                    .to_owned(),
            });
        }
        Ok(())
    }
}

#[lenso::provides(tool_contract::ToolProvider)]
impl GitHubWorkflows {
    fn catalog(
        &self,
        _context: Ctx,
        _request: CatalogRequest,
    ) -> impl std::future::Future<Output = PluginResult<CatalogResponse, tool_contract::CatalogError>>
    {
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
                    program: "gh".to_owned(),
                    arguments,
                    cwd: ".".to_owned(),
                    timeout_ms: self.config.default_timeout_ms.to_string(),
                },
            )
            .await
            .map_err(map_process_error)?;
        map_response(request.name.as_str(), response)
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the complete Tool catalog is intentionally reviewable in one declarative table"
)]
fn tool_definitions(config: &GitHubConfig) -> Vec<ToolDefinition> {
    let repository = serde_json::json!({
        "type": "string",
        "enum": config.allowed_repositories,
    });
    let mut tools = vec![
        tool(
            ISSUE_GET_TOOL,
            "Read one GitHub Issue, including its current state and comments URL.",
            &schema(
                &serde_json::json!({
                    "repository": repository.clone(),
                    "number": { "type": "integer", "minimum": 1 }
                }),
                &["repository", "number"],
            ),
            ToolExecutionClass::ParallelSafe,
        ),
        tool(
            PR_GET_TOOL,
            "Read one GitHub pull request and its merge and check-rollup state.",
            &schema(
                &serde_json::json!({
                    "repository": repository.clone(),
                    "number": { "type": "integer", "minimum": 1 }
                }),
                &["repository", "number"],
            ),
            ToolExecutionClass::ParallelSafe,
        ),
        tool(
            CI_STATUS_TOOL,
            "List bounded recent GitHub Actions runs for an allowed repository and optional branch.",
            &schema(
                &serde_json::json!({
                    "repository": repository.clone(),
                    "branch": { "type": "string", "minLength": 1, "maxLength": 255 },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 50, "default": 10 }
                }),
                &["repository"],
            ),
            ToolExecutionClass::ParallelSafe,
        ),
    ];
    if config.enable_mutations {
        tools.extend([
            tool(
                ISSUE_CREATE_TOOL,
                "Create one GitHub Issue with an explicit bounded title and body.",
                &schema(
                    &serde_json::json!({
                        "repository": repository.clone(),
                        "title": { "type": "string", "minLength": 1, "maxLength": 256 },
                        "body": { "type": "string", "maxLength": config.max_body_bytes }
                    }),
                    &["repository", "title"],
                ),
                ToolExecutionClass::Exclusive,
            ),
            tool(
                ISSUE_COMMENT_TOOL,
                "Add one bounded comment to an allowed GitHub Issue or pull request.",
                &schema(
                    &serde_json::json!({
                        "repository": repository.clone(),
                        "number": { "type": "integer", "minimum": 1 },
                        "body": { "type": "string", "minLength": 1, "maxLength": config.max_body_bytes }
                    }),
                    &["repository", "number", "body"],
                ),
                ToolExecutionClass::Exclusive,
            ),
            tool(
                ISSUE_CLOSE_TOOL,
                "Close one explicit GitHub Issue in an allowed repository.",
                &schema(
                    &serde_json::json!({
                        "repository": repository.clone(),
                        "number": { "type": "integer", "minimum": 1 }
                    }),
                    &["repository", "number"],
                ),
                ToolExecutionClass::Exclusive,
            ),
            tool(
                PR_CREATE_TOOL,
                "Create one pull request from explicit head and base branches in an allowed repository.",
                &schema(
                    &serde_json::json!({
                        "repository": repository.clone(),
                        "title": { "type": "string", "minLength": 1, "maxLength": 256 },
                        "head": { "type": "string", "minLength": 1, "maxLength": 255 },
                        "base": { "type": "string", "minLength": 1, "maxLength": 255 },
                        "body": { "type": "string", "maxLength": config.max_body_bytes }
                    }),
                    &["repository", "title", "head", "base"],
                ),
                ToolExecutionClass::Exclusive,
            ),
            tool(
                PR_MERGE_TOOL,
                "Merge one explicit GitHub pull request using the selected merge method.",
                &schema(
                    &serde_json::json!({
                        "repository": repository.clone(),
                        "number": { "type": "integer", "minimum": 1 },
                        "method": { "type": "string", "enum": ["merge", "rebase", "squash"] }
                    }),
                    &["repository", "number", "method"],
                ),
                ToolExecutionClass::Exclusive,
            ),
            tool(
                CI_RERUN_TOOL,
                "Rerun one explicit GitHub Actions run, optionally only its failed jobs.",
                &schema(
                    &serde_json::json!({
                        "repository": repository,
                        "run_id": { "type": "integer", "minimum": 1 },
                        "failed_only": { "type": "boolean", "default": false }
                    }),
                    &["repository", "run_id"],
                ),
                ToolExecutionClass::Exclusive,
            ),
        ]);
    }
    tools
}

#[allow(
    clippy::too_many_lines,
    reason = "the complete gh argv allowlist is intentionally reviewable in one match"
)]
fn command_for(
    config: &GitHubConfig,
    request: &ExecuteRequest,
) -> PluginResult<Vec<String>, ExecuteError> {
    match request.name.as_str() {
        ISSUE_GET_TOOL => {
            let arguments = decode::<NumberArguments>(request)?;
            allowed(config, &arguments.repository)?;
            positive(arguments.number)?;
            Ok(vec![
                "api".to_owned(),
                format!("repos/{}/issues/{}", arguments.repository, arguments.number),
            ])
        }
        PR_GET_TOOL => {
            let arguments = decode::<NumberArguments>(request)?;
            allowed(config, &arguments.repository)?;
            positive(arguments.number)?;
            Ok(vec![
                "pr".to_owned(),
                "view".to_owned(),
                arguments.number.to_string(),
                "--repo".to_owned(),
                arguments.repository,
                "--json".to_owned(),
                "number,title,state,url,headRefName,baseRefName,mergeStateStatus,statusCheckRollup"
                    .to_owned(),
            ])
        }
        CI_STATUS_TOOL => {
            let arguments = decode::<CiStatusArguments>(request)?;
            allowed(config, &arguments.repository)?;
            if !(1..=50).contains(&arguments.limit) {
                return invalid_arguments();
            }
            let mut command = vec![
                "run".to_owned(),
                "list".to_owned(),
                "--repo".to_owned(),
                arguments.repository,
                "--limit".to_owned(),
                arguments.limit.to_string(),
                "--json".to_owned(),
                "databaseId,name,status,conclusion,headBranch,headSha,url,createdAt,updatedAt"
                    .to_owned(),
            ];
            if let Some(branch) = arguments.branch {
                validate_text(&branch, 255, false)?;
                command.extend(["--branch".to_owned(), branch]);
            }
            Ok(command)
        }
        ISSUE_COMMENT_TOOL if config.enable_mutations => {
            let arguments = decode::<CommentArguments>(request)?;
            allowed(config, &arguments.repository)?;
            positive(arguments.number)?;
            validate_text(&arguments.body, config.max_body_bytes, true)?;
            Ok(vec![
                "api".to_owned(),
                format!(
                    "repos/{}/issues/{}/comments",
                    arguments.repository, arguments.number
                ),
                "--method".to_owned(),
                "POST".to_owned(),
                "--field".to_owned(),
                format!("body={}", arguments.body),
            ])
        }
        ISSUE_CREATE_TOOL if config.enable_mutations => {
            let arguments = decode::<IssueCreateArguments>(request)?;
            allowed(config, &arguments.repository)?;
            validate_text(&arguments.title, 256, true)?;
            validate_text(&arguments.body, config.max_body_bytes, false)?;
            Ok(vec![
                "api".to_owned(),
                format!("repos/{}/issues", arguments.repository),
                "--method".to_owned(),
                "POST".to_owned(),
                "--field".to_owned(),
                format!("title={}", arguments.title),
                "--field".to_owned(),
                format!("body={}", arguments.body),
            ])
        }
        ISSUE_CLOSE_TOOL if config.enable_mutations => {
            let arguments = decode::<NumberArguments>(request)?;
            allowed(config, &arguments.repository)?;
            positive(arguments.number)?;
            Ok(vec![
                "api".to_owned(),
                format!("repos/{}/issues/{}", arguments.repository, arguments.number),
                "--method".to_owned(),
                "PATCH".to_owned(),
                "--field".to_owned(),
                "state=closed".to_owned(),
            ])
        }
        PR_CREATE_TOOL if config.enable_mutations => {
            let arguments = decode::<PullRequestCreateArguments>(request)?;
            allowed(config, &arguments.repository)?;
            validate_text(&arguments.title, 256, true)?;
            validate_text(&arguments.head, 255, true)?;
            validate_text(&arguments.base, 255, true)?;
            validate_text(&arguments.body, config.max_body_bytes, false)?;
            Ok(vec![
                "api".to_owned(),
                format!("repos/{}/pulls", arguments.repository),
                "--method".to_owned(),
                "POST".to_owned(),
                "--field".to_owned(),
                format!("title={}", arguments.title),
                "--field".to_owned(),
                format!("head={}", arguments.head),
                "--field".to_owned(),
                format!("base={}", arguments.base),
                "--field".to_owned(),
                format!("body={}", arguments.body),
            ])
        }
        PR_MERGE_TOOL if config.enable_mutations => {
            let arguments = decode::<PullRequestMergeArguments>(request)?;
            allowed(config, &arguments.repository)?;
            positive(arguments.number)?;
            let method = match arguments.method {
                MergeMethod::Merge => "--merge",
                MergeMethod::Rebase => "--rebase",
                MergeMethod::Squash => "--squash",
            };
            Ok(vec![
                "pr".to_owned(),
                "merge".to_owned(),
                arguments.number.to_string(),
                "--repo".to_owned(),
                arguments.repository,
                method.to_owned(),
            ])
        }
        CI_RERUN_TOOL if config.enable_mutations => {
            let arguments = decode::<CiRerunArguments>(request)?;
            allowed(config, &arguments.repository)?;
            positive(arguments.run_id)?;
            let mut command = vec![
                "run".to_owned(),
                "rerun".to_owned(),
                arguments.run_id.to_string(),
                "--repo".to_owned(),
                arguments.repository,
            ];
            if arguments.failed_only {
                command.push("--failed".to_owned());
            }
            Ok(command)
        }
        _ => Err(PluginError::domain(ExecuteError::NotFound)),
    }
}

fn valid_repository(value: &str) -> bool {
    let Some((owner, repository)) = value.split_once('/') else {
        return false;
    };
    !owner.is_empty()
        && !repository.is_empty()
        && value.len() <= 200
        && !repository.contains('/')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/'))
}

fn allowed(config: &GitHubConfig, repository: &str) -> PluginResult<(), ExecuteError> {
    if config
        .allowed_repositories
        .binary_search(&repository.to_owned())
        .is_err()
    {
        return Err(PluginError::domain(ExecuteError::PermissionDenied));
    }
    Ok(())
}

fn positive(value: u64) -> PluginResult<(), ExecuteError> {
    if value == 0 {
        return invalid_arguments();
    }
    Ok(())
}

fn validate_text(value: &str, maximum: usize, nonempty: bool) -> PluginResult<(), ExecuteError> {
    if value.len() > maximum || value.contains('\0') || (nonempty && value.trim().is_empty()) {
        return invalid_arguments();
    }
    Ok(())
}

fn decode<T: serde::de::DeserializeOwned>(
    request: &ExecuteRequest,
) -> PluginResult<T, ExecuteError> {
    serde_json::from_str(request.arguments_json.as_str())
        .map_err(|_| PluginError::domain(ExecuteError::InvalidArguments))
}

fn invalid_arguments<T>() -> PluginResult<T, ExecuteError> {
    Err(PluginError::domain(ExecuteError::InvalidArguments))
}

fn schema(properties: &serde_json::Value, required: &[&str]) -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
        "required": required,
    })
}

fn tool(
    name: &str,
    description: &str,
    input_schema: &serde_json::Value,
    execution: ToolExecutionClass,
) -> ToolDefinition {
    ToolDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema_json: input_schema
            .to_string()
            .try_into()
            .expect("GitHub Tool schema must be valid JSON"),
        execution,
    }
}

fn map_response(
    tool_name: &str,
    response: RunResponse,
) -> PluginResult<ExecuteResponse, ExecuteError> {
    if response.exit_code != "0" {
        return Err(PluginError::domain(ExecuteError::ExecutionFailed {
            payload: ExecutionFailedPayload {
                reason_code: "github_failed".to_owned(),
                message: "GitHub rejected the requested operation.".to_owned(),
                details_json: serde_json::json!({
                    "exit_code": response.exit_code,
                    "stderr": response.stderr,
                })
                .to_string()
                .try_into()
                .expect("GitHub error details must be valid JSON"),
            },
        }));
    }
    Ok(ExecuteResponse {
        content: if response.stdout.is_empty() {
            "GitHub operation completed successfully.".to_owned()
        } else {
            response.stdout
        },
        content_type: ContentType::Text,
        metadata_json: serde_json::json!({
            "tool": tool_name,
            "exit_code": response.exit_code,
            "duration_ms": response.duration_ms,
        })
        .to_string()
        .try_into()
        .expect("GitHub metadata must be valid JSON"),
    })
}

fn map_process_error(error: ProcessRunInvocationError) -> PluginError<ExecuteError> {
    match error {
        ProcessRunInvocationError::Domain(error) => PluginError::domain(match error {
            RunError::InvalidRequest => ExecuteError::InvalidArguments,
            RunError::ProgramNotAllowed | RunError::InvalidWorkingDirectory => {
                ExecuteError::PermissionDenied
            }
            RunError::OutputLimitExceeded => ExecuteError::OutputLimitExceeded,
            RunError::Timeout => execution_failure("github_timeout"),
            RunError::Terminated => execution_failure("github_terminated"),
            RunError::Unknown(_) => execution_failure("github_unknown"),
        }),
        ProcessRunInvocationError::Runtime(error) => PluginError::runtime(error),
    }
}

fn execution_failure(reason_code: &str) -> ExecuteError {
    ExecuteError::ExecutionFailed {
        payload: ExecutionFailedPayload {
            reason_code: reason_code.to_owned(),
            message: "GitHub workflow execution failed.".to_owned(),
            details_json: "{}".try_into().expect("empty JSON object must be valid"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(enable_mutations: bool) -> GitHubConfig {
        GitHubConfig {
            allowed_repositories: vec!["LioRael/lenso-agent-harness".to_owned()],
            default_timeout_ms: 30_000,
            enable_mutations,
            max_body_bytes: 16_384,
        }
    }

    fn request(name: &str, arguments: &serde_json::Value) -> ExecuteRequest {
        ExecuteRequest {
            name: name.to_owned(),
            arguments_json: arguments.to_string().try_into().unwrap(),
        }
    }

    #[test]
    fn descriptor_requires_process_and_provides_tools() {
        let descriptor: serde_json::Value = serde_json::from_str(PLUGIN_DESCRIPTOR_JSON).unwrap();
        assert_eq!(descriptor["plugin_id"], "lenso.agent.github-workflows");
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
    fn read_only_catalog_omits_mutations() {
        let tools = tool_definitions(&config(false));
        assert_eq!(tools.len(), 3);
        assert!(
            tools
                .iter()
                .all(|tool| tool.execution == ToolExecutionClass::ParallelSafe)
        );
    }

    #[test]
    fn commands_bind_every_operation_to_an_allowed_repository() {
        let command = command_for(
            &config(true),
            &request(
                PR_CREATE_TOOL,
                &serde_json::json!({
                    "repository": "LioRael/lenso-agent-harness",
                    "title": "feat: workflow",
                    "head": "feature",
                    "base": "main",
                    "body": "Bounded body"
                }),
            ),
        )
        .unwrap();
        assert_eq!(command[0], "api");
        assert_eq!(command[1], "repos/LioRael/lenso-agent-harness/pulls");
        assert!(
            command_for(
                &config(true),
                &request(
                    ISSUE_GET_TOOL,
                    &serde_json::json!({ "repository": "other/repo", "number": 1 }),
                ),
            )
            .is_err()
        );
    }
}
