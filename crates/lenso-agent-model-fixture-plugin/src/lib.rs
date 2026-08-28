//! Deterministic Model Plugin for the headless read-only proof.

use futures::future::{LocalBoxFuture, ready};
use lenso_agent_native_support::FiniteOutputStream;
use lenso_capability_agent_model::{
    self as model_contract, CAPABILITY_ID, CompleteError, CompleteMessage, CompleteMessageInput,
    CompleteMessageKind, CompleteMessageRole, CompleteOpen, ModelInvocationError, ModelProvider,
};
use lenso_kernel::{InvocationContext, NativeStreamSession, RuntimeFailure};

/// Only model identifier supported by the deterministic fixture.
pub const MODEL_ID: &str = "fixture/readme-summary-v1";

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureConfig {
    model: String,
}

fn validate_config(config: &FixtureConfig) -> Result<(), RuntimeFailure> {
    if config.model != MODEL_ID {
        return Err(RuntimeFailure::InvalidResolvedPlan {
            detail: format!("fixture Model must be `{MODEL_ID}`"),
        });
    }
    Ok(())
}

#[lenso::plugin(configuration_schema = "config.schema.json", validate = validate_config)]
#[derive(Clone, Debug)]
struct FixtureModel {
    #[config]
    config: FixtureConfig,
}

#[lenso::provides(model_contract::Model)]
impl ModelProvider for FixtureModel {
    fn complete(
        &self,
        _context: InvocationContext,
        request: CompleteOpen,
    ) -> LocalBoxFuture<'static, Result<Box<dyn NativeStreamSession>, ModelInvocationError>> {
        let result = self.complete_now(&request).map(|messages| {
            Box::new(FiniteOutputStream::successful(CAPABILITY_ID, messages))
                as Box<dyn NativeStreamSession>
        });
        Box::pin(ready(result))
    }
}

impl FixtureModel {
    fn complete_now(
        &self,
        request: &CompleteOpen,
    ) -> Result<Vec<CompleteMessage>, ModelInvocationError> {
        if request.model != self.config.model || request.max_output_tokens <= 0 {
            return Err(ModelInvocationError::Domain(
                CompleteError::UnsupportedModel,
            ));
        }
        let current_user_index = request
            .messages
            .iter()
            .rposition(|message| message.role == CompleteMessageRole::User)
            .ok_or(ModelInvocationError::Domain(CompleteError::InvalidRequest))?;
        let current_user = &request.messages[current_user_index].content;
        if current_user.starts_with("Answer directly:") {
            return Ok(direct_fixture_response(request));
        }
        if current_user == "What did you summarize?" {
            let previous = request.messages[..current_user_index]
                .iter()
                .rev()
                .find(|message| message.role == CompleteMessageRole::Assistant)
                .map_or("Nothing yet.", |message| message.content.as_str());
            return Ok(previous_response(previous));
        }
        let tool_results = request.messages[current_user_index + 1..]
            .iter()
            .filter(|message| message.role == CompleteMessageRole::Tool)
            .collect::<Vec<_>>();
        if current_user == "Use a Skill to review Rust." {
            return skill_response(request, &tool_results);
        }
        if current_user == "Use a Skill resource to review Rust." {
            return resource_skill_response(request, &tool_results);
        }
        if current_user == "Navigate the workspace to find the navigation target." {
            return workspace_navigation_response(request, &tool_results);
        }
        if current_user == "Create and edit a workspace note." {
            return workspace_mutation_response(request, &tool_results);
        }
        if current_user == "Create one approved workspace note." {
            return approved_workspace_mutation_response(request, &tool_results);
        }
        if current_user == "Edit and validate the workspace project." {
            return local_coding_response(request, &tool_results);
        }
        if current_user == "Inspect and commit the prepared Git change." {
            return git_workflow_response(request, &tool_results);
        }
        if current_user == "Use the text Plugin to uppercase Lenso plugin." {
            return text_plugin_response(request, &tool_results);
        }
        if current_user == "Use the MCP fixture to ping." {
            return mcp_plugin_response(request, &tool_results);
        }
        if current_user.contains("Selected Context Prompt: fixture/review")
            && current_user.contains("Selected Context Resource: fixture/fixture://guide")
            && current_user.contains("Review carefully.")
            && current_user.contains("Fixture guide content.")
        {
            return Ok(context_source_result());
        }
        if current_user == "Ask me which mode to use." {
            return ask_user_response(request, &tool_results);
        }
        if current_user == "Use the workspace Plugin to read README.md." {
            return workspace_plugin_response(request, &tool_results);
        }
        if current_user == "Delegate a README.md summary." {
            return subagent_root_response(request, &tool_results);
        }
        if current_user == "Use Code Mode to compare README.md twice." {
            return code_mode_response(request, &tool_results);
        }
        if current_user == "Summarize README.md for the parent Agent." {
            return subagent_child_response(request, &tool_results);
        }
        if let Some(url) = current_user
            .strip_prefix("Use the network Plugin to fetch ")
            .and_then(|value| value.strip_suffix('.'))
        {
            return network_plugin_response(request, &tool_results, url);
        }
        if current_user == "Read README.md twice." && tool_results.len() < 2 {
            return Ok(tool_request(tool_results.len() + 1));
        }
        let tool_result = tool_results.last().copied();
        if let Some(tool_result) = tool_result {
            let first_line = tool_result
                .content
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("The README is empty.")
                .trim();
            return Ok(summary_response(first_line));
        }
        let has_workspace_tool = request.tools.iter().any(|tool| tool.name == "read");
        if !has_workspace_tool {
            return Err(ModelInvocationError::Domain(CompleteError::InvalidRequest));
        }
        Ok(tool_request(1))
    }
}

fn direct_fixture_response(request: &CompleteOpen) -> Vec<CompleteMessage> {
    let has_prefix = |prefix: &str| {
        request.messages.iter().any(|message| {
            message.role == CompleteMessageRole::System && message.content.contains(prefix)
        })
    };
    direct_response(
        has_prefix("Prefix direct answers with `Plugin: `."),
        has_prefix("Prefix direct answers with `Filesystem: `."),
    )
}

fn ask_user_response(
    request: &CompleteOpen,
    tool_results: &[&CompleteMessageInput],
) -> Result<Vec<CompleteMessage>, ModelInvocationError> {
    if !request.tools.iter().any(|tool| tool.name == "ask_user") {
        return Err(ModelInvocationError::Domain(CompleteError::InvalidRequest));
    }
    match tool_results {
        [] => Ok(named_tool_request(
            "call-ask-user",
            "ask_user",
            r#"{"question":"Which mode should I use?","options":["safe","fast"],"allow_freeform":false}"#,
        )),
        [result] => Ok(vec![
            response(
                "1",
                CompleteMessageKind::TextDelta,
                format!("Selected mode: {}", result.content),
                "",
                "",
                "{}",
                "0",
                "0",
            ),
            response("2", CompleteMessageKind::Usage, "", "", "", "{}", "16", "4"),
        ]),
        _ => Err(ModelInvocationError::Domain(CompleteError::InvalidRequest)),
    }
}

fn code_mode_response(
    request: &CompleteOpen,
    tool_results: &[&CompleteMessageInput],
) -> Result<Vec<CompleteMessage>, ModelInvocationError> {
    if !request.tools.iter().any(|tool| tool.name == "run_code") {
        return Err(ModelInvocationError::Domain(CompleteError::InvalidRequest));
    }
    match tool_results {
        [] => Ok(named_tool_request(
            "call-code-mode",
            "run_code",
            r#"{"code":"local values = parallel({{name='read_text', arguments={path='README.md'}}, {name='read_text', arguments={path='README.md'}}}); return {first=values[1], same=values[1] == values[2]}"}"#,
        )),
        [result] if is_expected_code_mode_result(&result.content) => Ok(vec![
            response(
                "1",
                CompleteMessageKind::TextDelta,
                "Code Mode result: README copies match",
                "",
                "",
                "{}",
                "0",
                "0",
            ),
            response("2", CompleteMessageKind::Usage, "", "", "", "{}", "28", "8"),
        ]),
        _ => Err(ModelInvocationError::Domain(CompleteError::InvalidRequest)),
    }
}

fn is_expected_code_mode_result(content: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(content)
        .is_ok_and(|value| value["first"] == "# Plugin Fixture\n" && value["same"] == true)
}

fn subagent_root_response(
    request: &CompleteOpen,
    tool_results: &[&CompleteMessageInput],
) -> Result<Vec<CompleteMessage>, ModelInvocationError> {
    if !request.tools.iter().any(|tool| tool.name == "delegate") {
        return Err(ModelInvocationError::Domain(CompleteError::InvalidRequest));
    }
    match tool_results {
        [] => Ok(named_tool_request(
            "call-delegate-readme",
            "delegate",
            r#"{"task":"Summarize README.md for the parent Agent."}"#,
        )),
        [result] if result.content == "Child summary: # Plugin Fixture" => Ok(vec![
            response(
                "1",
                CompleteMessageKind::TextDelta,
                "Delegated result: Child summary: # Plugin Fixture",
                "",
                "",
                "{}",
                "0",
                "0",
            ),
            response(
                "2",
                CompleteMessageKind::Usage,
                "",
                "",
                "",
                "{}",
                "28",
                "12",
            ),
        ]),
        _ => Err(ModelInvocationError::Domain(CompleteError::InvalidRequest)),
    }
}

fn subagent_child_response(
    request: &CompleteOpen,
    tool_results: &[&CompleteMessageInput],
) -> Result<Vec<CompleteMessage>, ModelInvocationError> {
    if !request.tools.iter().any(|tool| tool.name == "read_text") {
        return Err(ModelInvocationError::Domain(CompleteError::InvalidRequest));
    }
    match tool_results {
        [] => Ok(named_tool_request(
            "call-child-readme",
            "read_text",
            r#"{"path":"README.md"}"#,
        )),
        [result] if result.content == "# Plugin Fixture\n" => Ok(vec![
            response(
                "1",
                CompleteMessageKind::TextDelta,
                "Child summary: # Plugin Fixture",
                "",
                "",
                "{}",
                "0",
                "0",
            ),
            response("2", CompleteMessageKind::Usage, "", "", "", "{}", "20", "8"),
        ]),
        _ => Err(ModelInvocationError::Domain(CompleteError::InvalidRequest)),
    }
}

fn text_plugin_response(
    request: &CompleteOpen,
    tool_results: &[&CompleteMessageInput],
) -> Result<Vec<CompleteMessage>, ModelInvocationError> {
    if !request.tools.iter().any(|tool| tool.name == "uppercase") {
        return Err(ModelInvocationError::Domain(CompleteError::InvalidRequest));
    }
    match tool_results {
        [] => Ok(named_tool_request(
            "call-text-uppercase",
            "uppercase",
            r#"{"text":"Lenso plugin"}"#,
        )),
        [result] if !result.content.is_empty() => Ok(text_plugin_result(&result.content)),
        _ => Err(ModelInvocationError::Domain(CompleteError::InvalidRequest)),
    }
}

fn mcp_plugin_response(
    request: &CompleteOpen,
    tool_results: &[&CompleteMessageInput],
) -> Result<Vec<CompleteMessage>, ModelInvocationError> {
    if !request
        .tools
        .iter()
        .any(|tool| tool.name == "mcp__fixture__ping")
    {
        return Err(ModelInvocationError::Domain(CompleteError::InvalidRequest));
    }
    match tool_results {
        [] => Ok(named_tool_request(
            "call-mcp-ping",
            "mcp__fixture__ping",
            "{}",
        )),
        [result] if result.content == "pong" => Ok(mcp_plugin_result()),
        _ => Err(ModelInvocationError::Domain(CompleteError::InvalidRequest)),
    }
}

fn workspace_plugin_response(
    request: &CompleteOpen,
    tool_results: &[&CompleteMessageInput],
) -> Result<Vec<CompleteMessage>, ModelInvocationError> {
    if !request
        .tools
        .iter()
        .any(|tool| tool.name == "plugin_workspace_read_text")
    {
        return Err(ModelInvocationError::Domain(CompleteError::InvalidRequest));
    }
    match tool_results {
        [] => Ok(named_tool_request(
            "call-plugin-workspace-read",
            "plugin_workspace_read_text",
            r#"{"path":"README.md"}"#,
        )),
        [result] if result.content == "# Plugin Fixture\n" => Ok(vec![
            response(
                "1",
                CompleteMessageKind::TextDelta,
                "Workspace Plugin result: # Plugin Fixture",
                "",
                "",
                "{}",
                "0",
                "0",
            ),
            response(
                "2",
                CompleteMessageKind::Usage,
                "",
                "",
                "",
                "{}",
                "28",
                "10",
            ),
        ]),
        _ => Err(ModelInvocationError::Domain(CompleteError::InvalidRequest)),
    }
}

fn network_plugin_response(
    request: &CompleteOpen,
    tool_results: &[&CompleteMessageInput],
    url: &str,
) -> Result<Vec<CompleteMessage>, ModelInvocationError> {
    if !request
        .tools
        .iter()
        .any(|tool| tool.name == "plugin_http_get")
    {
        return Err(ModelInvocationError::Domain(CompleteError::InvalidRequest));
    }
    match tool_results {
        [] => {
            let arguments = serde_json::json!({ "url": url }).to_string();
            Ok(named_tool_request(
                "call-plugin-http-get",
                "plugin_http_get",
                &arguments,
            ))
        }
        [result] if result.content == "network fixture" => Ok(vec![
            response(
                "1",
                CompleteMessageKind::TextDelta,
                "Network Plugin result: network fixture",
                "",
                "",
                "{}",
                "0",
                "0",
            ),
            response(
                "2",
                CompleteMessageKind::Usage,
                "",
                "",
                "",
                "{}",
                "28",
                "10",
            ),
        ]),
        _ => Err(ModelInvocationError::Domain(CompleteError::InvalidRequest)),
    }
}

fn skill_response(
    request: &CompleteOpen,
    tool_results: &[&CompleteMessageInput],
) -> Result<Vec<CompleteMessage>, ModelInvocationError> {
    let has_skill_tools = ["skill_list", "skill"]
        .iter()
        .all(|name| request.tools.iter().any(|tool| tool.name.as_str() == *name));
    let skill_catalog_in_prompt = request.messages.iter().any(|message| {
        message.role == CompleteMessageRole::System
            && message.content.contains("`rust-review`")
            && message
                .content
                .contains("Review Rust changes with project conventions.")
            && !message.content.contains("RUST REVIEW INSTRUCTION")
            && !message
                .content
                .contains("UNSELECTED SKILL CONTENT MUST NOT REACH THE MODEL")
    });
    let leaked_unselected_skill = tool_results.iter().any(|result| {
        result
            .content
            .contains("UNSELECTED SKILL CONTENT MUST NOT REACH THE MODEL")
    });
    if !has_skill_tools
        || !readonly_skill_tool_profile(request)
        || !skill_catalog_in_prompt
        || leaked_unselected_skill
    {
        return Err(ModelInvocationError::Domain(CompleteError::InvalidRequest));
    }
    match tool_results {
        [] => Ok(named_tool_request(
            "call-skills-read",
            "skill",
            r#"{"name":"rust-review"}"#,
        )),
        [skill] if skill.content.contains("RUST REVIEW INSTRUCTION") => {
            Ok(skill_applied_response())
        }
        _ => Err(ModelInvocationError::Domain(CompleteError::InvalidRequest)),
    }
}

fn resource_skill_response(
    request: &CompleteOpen,
    tool_results: &[&CompleteMessageInput],
) -> Result<Vec<CompleteMessage>, ModelInvocationError> {
    let has_skill_tools = ["skill_list", "skill", "skill_resources", "skill_resource"]
        .iter()
        .all(|name| request.tools.iter().any(|tool| tool.name.as_str() == *name));
    if !has_skill_tools
        || !readonly_skill_tool_profile(request)
        || !request.messages.iter().any(|message| {
            message.role == CompleteMessageRole::System
                && message.content.contains("`rust-review`")
                && !message.content.contains("RUST REVIEW INSTRUCTION")
        })
        || tool_results.iter().any(|result| {
            result
                .content
                .contains("UNREAD RESOURCE CONTENT MUST NOT REACH THE MODEL")
        })
    {
        return Err(ModelInvocationError::Domain(CompleteError::InvalidRequest));
    }
    match tool_results {
        [] => Ok(named_tool_request(
            "call-resources-read-skill",
            "skill",
            r#"{"name":"rust-review"}"#,
        )),
        [skill] if skill.content.contains("references/checklist.md") => Ok(named_tool_request(
            "call-resources-list",
            "skill_resources",
            r#"{"name":"rust-review"}"#,
        )),
        [_, manifest]
            if manifest.content.contains("references/checklist.md")
                && !manifest.content.contains("RESOURCE CHECKLIST CONTENT") =>
        {
            Ok(named_tool_request(
                "call-resource-read",
                "skill_resource",
                r#"{"name":"rust-review","path":"references/checklist.md"}"#,
            ))
        }
        [_, _, resource] if resource.content.contains("RESOURCE CHECKLIST CONTENT") => {
            Ok(resource_applied_response())
        }
        _ => Err(ModelInvocationError::Domain(CompleteError::InvalidRequest)),
    }
}

fn readonly_skill_tool_profile(request: &CompleteOpen) -> bool {
    request.tools.iter().all(|tool| {
        matches!(
            tool.name.as_str(),
            "ask_user"
                | "list"
                | "search"
                | "read"
                | "skill_list"
                | "skill"
                | "skill_resources"
                | "skill_resource"
        )
    })
}

fn workspace_navigation_response(
    request: &CompleteOpen,
    tool_results: &[&CompleteMessageInput],
) -> Result<Vec<CompleteMessage>, ModelInvocationError> {
    let has_workspace_tools = ["list", "search", "read"]
        .iter()
        .all(|name| request.tools.iter().any(|tool| tool.name.as_str() == *name));
    if !has_workspace_tools {
        return Err(ModelInvocationError::Domain(CompleteError::InvalidRequest));
    }
    match tool_results {
        [] => Ok(named_tool_request("call-workspace-list", "list", "{}")),
        [listing] if listing.content.contains("docs") => Ok(named_tool_request(
            "call-workspace-search",
            "search",
            r#"{"query":"NAVIGATION_TARGET"}"#,
        )),
        [_, search] if search.content.contains("docs/guide.md") => Ok(named_tool_request(
            "call-workspace-read",
            "read",
            r#"{"path":"docs/guide.md"}"#,
        )),
        [_, _, document] if document.content.contains("NAVIGATION_TARGET") => {
            let first_line = document
                .content
                .lines()
                .next()
                .unwrap_or("The target is empty.");
            Ok(navigation_response(first_line))
        }
        _ => Err(ModelInvocationError::Domain(CompleteError::InvalidRequest)),
    }
}

fn navigation_response(first_line: &str) -> Vec<CompleteMessage> {
    vec![
        response(
            "1",
            CompleteMessageKind::TextDelta,
            format!("Navigation result: {first_line}"),
            "",
            "",
            "{}",
            "0",
            "0",
        ),
        response(
            "2",
            CompleteMessageKind::Usage,
            "",
            "",
            "",
            "{}",
            "32",
            "12",
        ),
    ]
}

fn workspace_mutation_response(
    request: &CompleteOpen,
    tool_results: &[&CompleteMessageInput],
) -> Result<Vec<CompleteMessage>, ModelInvocationError> {
    let has_mutation_tools = ["create_file", "edit", "read"]
        .iter()
        .all(|name| request.tools.iter().any(|tool| tool.name.as_str() == *name));
    if !has_mutation_tools {
        return Err(ModelInvocationError::Domain(CompleteError::InvalidRequest));
    }
    match tool_results {
        [] => Ok(named_tool_request(
            "call-workspace-write",
            "create_file",
            r#"{"path":"note.txt","content":"before\n"}"#,
        )),
        [created] if created.content == "created note.txt" => Ok(named_tool_request(
            "call-workspace-edit",
            "edit",
            r#"{"path":"note.txt","old_text":"before","new_text":"after"}"#,
        )),
        [_, edited] if edited.content == "edited note.txt" => Ok(named_tool_request(
            "call-workspace-read-after-edit",
            "read",
            r#"{"path":"note.txt"}"#,
        )),
        [_, _, document] if document.content == "after\n" => Ok(mutation_response()),
        _ => Err(ModelInvocationError::Domain(CompleteError::InvalidRequest)),
    }
}

fn approved_workspace_mutation_response(
    request: &CompleteOpen,
    tool_results: &[&CompleteMessageInput],
) -> Result<Vec<CompleteMessage>, ModelInvocationError> {
    if !request.tools.iter().any(|tool| tool.name == "create_file") {
        return Err(ModelInvocationError::Domain(CompleteError::InvalidRequest));
    }
    match tool_results {
        [] => Ok(named_tool_request(
            "call-approved-workspace-write",
            "create_file",
            r#"{"path":"approved-note.txt","content":"approved\n"}"#,
        )),
        [created] if created.content == "created approved-note.txt" => Ok(vec![
            response(
                "1",
                CompleteMessageKind::TextDelta,
                "Approved workspace note created",
                "",
                "",
                "{}",
                "0",
                "0",
            ),
            response("2", CompleteMessageKind::Usage, "", "", "", "{}", "20", "8"),
        ]),
        _ => Err(ModelInvocationError::Domain(CompleteError::InvalidRequest)),
    }
}

fn mutation_response() -> Vec<CompleteMessage> {
    vec![
        response(
            "1",
            CompleteMessageKind::TextDelta,
            "Workspace mutation result: after",
            "",
            "",
            "{}",
            "0",
            "0",
        ),
        response(
            "2",
            CompleteMessageKind::Usage,
            "",
            "",
            "",
            "{}",
            "32",
            "12",
        ),
    ]
}

fn local_coding_response(
    request: &CompleteOpen,
    tool_results: &[&CompleteMessageInput],
) -> Result<Vec<CompleteMessage>, ModelInvocationError> {
    let has_coding_tools = ["edit", "run_process", "read"]
        .iter()
        .all(|name| request.tools.iter().any(|tool| tool.name.as_str() == *name));
    if !has_coding_tools {
        return Err(ModelInvocationError::Domain(CompleteError::InvalidRequest));
    }
    match tool_results {
        [] => Ok(named_tool_request(
            "call-local-coding-edit",
            "edit",
            r#"{"path":"src/lib.rs","old_text":"pub fn value() -> u32 { 1 }","new_text":"pub fn value() -> u32 { 2 }"}"#,
        )),
        [edited] if edited.content == "edited src/lib.rs" => Ok(named_tool_request(
            "call-local-coding-check",
            "run_process",
            r#"{"program":"cargo","arguments":["check","--quiet"]}"#,
        )),
        [_, checked] if checked.content.starts_with("exit_code: 0\n") => Ok(named_tool_request(
            "call-local-coding-read",
            "read",
            r#"{"path":"src/lib.rs"}"#,
        )),
        [_, _, document] if document.content.contains("pub fn value() -> u32 { 2 }") => {
            Ok(local_coding_final_response())
        }
        _ => Err(ModelInvocationError::Domain(CompleteError::InvalidRequest)),
    }
}

fn local_coding_final_response() -> Vec<CompleteMessage> {
    vec![
        response(
            "1",
            CompleteMessageKind::TextDelta,
            "Local coding result: cargo check passed.",
            "",
            "",
            "{}",
            "0",
            "0",
        ),
        response(
            "2",
            CompleteMessageKind::Usage,
            "",
            "",
            "",
            "{}",
            "48",
            "12",
        ),
    ]
}

fn git_workflow_response(
    request: &CompleteOpen,
    tool_results: &[&CompleteMessageInput],
) -> Result<Vec<CompleteMessage>, ModelInvocationError> {
    let has_git_tools = ["git_status", "git_stage", "git_commit", "git_log"]
        .iter()
        .all(|name| request.tools.iter().any(|tool| tool.name.as_str() == *name));
    if !has_git_tools {
        return Err(ModelInvocationError::Domain(CompleteError::InvalidRequest));
    }
    match tool_results {
        [] => Ok(named_tool_request("call-git-status", "git_status", "{}")),
        [status] if status.content.contains(" M note.txt") => Ok(named_tool_request(
            "call-git-stage",
            "git_stage",
            r#"{"paths":["note.txt"]}"#,
        )),
        [_, staged] if staged.content == "Git operation completed successfully." => {
            Ok(named_tool_request(
                "call-git-commit",
                "git_commit",
                r#"{"message":"test: bounded Git Plugin commit"}"#,
            ))
        }
        [_, _, committed] if committed.content.contains("bounded Git Plugin commit") => Ok(
            named_tool_request("call-git-log", "git_log", r#"{"max_entries":1}"#),
        ),
        [_, _, _, log] if log.content.contains("test: bounded Git Plugin commit") => Ok(vec![
            response(
                "1",
                CompleteMessageKind::TextDelta,
                "Git Plugin result: prepared change committed.",
                "",
                "",
                "{}",
                "0",
                "0",
            ),
            response(
                "2",
                CompleteMessageKind::Usage,
                "",
                "",
                "",
                "{}",
                "48",
                "12",
            ),
        ]),
        _ => Err(ModelInvocationError::Domain(CompleteError::InvalidRequest)),
    }
}

fn direct_response(plugin_prefix: bool, filesystem_prefix: bool) -> Vec<CompleteMessage> {
    let prefix = match (filesystem_prefix, plugin_prefix) {
        (true, true) => "Filesystem: Plugin: ",
        (true, false) => "Filesystem: ",
        (false, true) => "Plugin: ",
        (false, false) => "",
    };
    vec![
        response(
            "1",
            CompleteMessageKind::ReasoningSummaryDelta,
            "I’ll answer directly from the current context.",
            "",
            "",
            "{}",
            "0",
            "0",
        ),
        response(
            "2",
            CompleteMessageKind::TextDelta,
            format!("{prefix}Direct "),
            "",
            "",
            "{}",
            "0",
            "0",
        ),
        response(
            "3",
            CompleteMessageKind::TextDelta,
            "answer.",
            "",
            "",
            "{}",
            "0",
            "0",
        ),
        response("4", CompleteMessageKind::Usage, "", "", "", "{}", "8", "2"),
    ]
}

fn previous_response(previous: &str) -> Vec<CompleteMessage> {
    vec![
        response(
            "1",
            CompleteMessageKind::TextDelta,
            format!("Previous answer: {previous}"),
            "",
            "",
            "{}",
            "0",
            "0",
        ),
        response("2", CompleteMessageKind::Usage, "", "", "", "{}", "16", "8"),
    ]
}

fn tool_request(index: usize) -> Vec<CompleteMessage> {
    named_tool_request(
        &format!("call-readme-{index}"),
        "read",
        r#"{"path":"README.md"}"#,
    )
}

fn named_tool_request(
    call_id: &str,
    tool_name: &str,
    arguments_json: &str,
) -> Vec<CompleteMessage> {
    vec![
        response(
            "1",
            CompleteMessageKind::ReasoningSummaryDelta,
            format!("I’ll use {tool_name} for the requested information."),
            "",
            "",
            "{}",
            "0",
            "0",
        ),
        response(
            "2",
            CompleteMessageKind::ToolCall,
            "",
            call_id,
            tool_name,
            arguments_json,
            "0",
            "0",
        ),
        response("3", CompleteMessageKind::Usage, "", "", "", "{}", "24", "8"),
    ]
}

fn skill_applied_response() -> Vec<CompleteMessage> {
    vec![
        response(
            "1",
            CompleteMessageKind::TextDelta,
            "Skill applied: Rust review used the selected instructions.",
            "",
            "",
            "{}",
            "0",
            "0",
        ),
        response(
            "2",
            CompleteMessageKind::Usage,
            "",
            "",
            "",
            "{}",
            "28",
            "10",
        ),
    ]
}

fn resource_applied_response() -> Vec<CompleteMessage> {
    vec![
        response(
            "1",
            CompleteMessageKind::TextDelta,
            "Resource applied: Rust review used references/checklist.md.",
            "",
            "",
            "{}",
            "0",
            "0",
        ),
        response(
            "2",
            CompleteMessageKind::Usage,
            "",
            "",
            "",
            "{}",
            "36",
            "12",
        ),
    ]
}

fn summary_response(first_line: &str) -> Vec<CompleteMessage> {
    vec![
        response(
            "1",
            CompleteMessageKind::ReasoningSummaryDelta,
            "I’ll summarize the relevant Tool result.",
            "",
            "",
            "{}",
            "0",
            "0",
        ),
        response(
            "2",
            CompleteMessageKind::TextDelta,
            format!("README summary: {first_line}"),
            "",
            "",
            "{}",
            "0",
            "0",
        ),
        response(
            "3",
            CompleteMessageKind::Usage,
            "",
            "",
            "",
            "{}",
            "32",
            "12",
        ),
    ]
}

fn text_plugin_result(content: &str) -> Vec<CompleteMessage> {
    vec![
        response(
            "1",
            CompleteMessageKind::TextDelta,
            format!("Text Plugin result: {content}"),
            "",
            "",
            "{}",
            "0",
            "0",
        ),
        response("2", CompleteMessageKind::Usage, "", "", "", "{}", "24", "8"),
    ]
}

fn mcp_plugin_result() -> Vec<CompleteMessage> {
    vec![
        response(
            "1",
            CompleteMessageKind::TextDelta,
            "MCP result: pong",
            "",
            "",
            "{}",
            "0",
            "0",
        ),
        response("2", CompleteMessageKind::Usage, "", "", "", "{}", "24", "8"),
    ]
}

fn context_source_result() -> Vec<CompleteMessage> {
    vec![
        response(
            "1",
            CompleteMessageKind::TextDelta,
            "Context result: prompt and resource applied",
            "",
            "",
            "{}",
            "0",
            "0",
        ),
        response("2", CompleteMessageKind::Usage, "", "", "", "{}", "24", "8"),
    ]
}

#[allow(clippy::too_many_arguments)]
fn response(
    sequence: &str,
    kind: CompleteMessageKind,
    text: impl Into<String>,
    tool_call_id: &str,
    tool_name: &str,
    arguments_json: &str,
    input_tokens: &str,
    output_tokens: &str,
) -> CompleteMessage {
    CompleteMessage {
        sequence: sequence.to_owned(),
        kind,
        text: text.into(),
        tool_call_id: tool_call_id.to_owned(),
        tool_name: tool_name.to_owned(),
        arguments_json: arguments_json
            .to_owned()
            .try_into()
            .expect("fixture Tool arguments must be valid JSON"),
        input_tokens: input_tokens.to_owned(),
        output_tokens: output_tokens.to_owned(),
    }
}
