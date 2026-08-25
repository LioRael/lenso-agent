//! Deterministic Model Module for the headless read-only proof.

use futures::future::{LocalBoxFuture, ready};
use lenso_agent_native_support::FiniteOutputStream;
use lenso_capability_agent_model::{
    self as model_contract, CAPABILITY_ID, CompleteError, CompleteRequest,
    CompleteRequestMessagesItem, CompleteRequestMessagesItemRole, CompleteResponse,
    CompleteResponseKind, ModelInvocationError, ModelProvider,
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

#[lenso::module(configuration_schema = "config.schema.json", validate = validate_config)]
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
        request: CompleteRequest,
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
        request: &CompleteRequest,
    ) -> Result<Vec<CompleteResponse>, ModelInvocationError> {
        if request.model != self.config.model || request.max_output_tokens <= 0 {
            return Err(ModelInvocationError::Domain(
                CompleteError::UnsupportedModel,
            ));
        }
        let current_user_index = request
            .messages
            .iter()
            .rposition(|message| message.role == CompleteRequestMessagesItemRole::User)
            .ok_or(ModelInvocationError::Domain(CompleteError::InvalidRequest))?;
        let current_user = &request.messages[current_user_index].content;
        if current_user.starts_with("Answer directly:") {
            let plugin_prefix = request.messages.iter().any(|message| {
                message.role == CompleteRequestMessagesItemRole::System
                    && message
                        .content
                        .contains("Prefix direct answers with `Plugin: `.")
            });
            let filesystem_prefix = request.messages.iter().any(|message| {
                message.role == CompleteRequestMessagesItemRole::System
                    && message
                        .content
                        .contains("Prefix direct answers with `Filesystem: `.")
            });
            return Ok(direct_response(plugin_prefix, filesystem_prefix));
        }
        if current_user == "What did you summarize?" {
            let previous = request.messages[..current_user_index]
                .iter()
                .rev()
                .find(|message| message.role == CompleteRequestMessagesItemRole::Assistant)
                .map_or("Nothing yet.", |message| message.content.as_str());
            return Ok(previous_response(previous));
        }
        let tool_results = request.messages[current_user_index + 1..]
            .iter()
            .filter(|message| message.role == CompleteRequestMessagesItemRole::Tool)
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
        if current_user == "Edit and validate the workspace project." {
            return local_coding_response(request, &tool_results);
        }
        if current_user == "Use the text Plugin to uppercase Lenso plugin." {
            return text_plugin_response(request, &tool_results);
        }
        if current_user == "Use the workspace Plugin to read README.md." {
            return workspace_plugin_response(request, &tool_results);
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
        let has_workspace_tool = request
            .tools
            .iter()
            .any(|tool| tool.name == "workspace.read_text");
        if !has_workspace_tool {
            return Err(ModelInvocationError::Domain(CompleteError::InvalidRequest));
        }
        Ok(tool_request(1))
    }
}

fn text_plugin_response(
    request: &CompleteRequest,
    tool_results: &[&CompleteRequestMessagesItem],
) -> Result<Vec<CompleteResponse>, ModelInvocationError> {
    if !request
        .tools
        .iter()
        .any(|tool| tool.name == "text.uppercase")
    {
        return Err(ModelInvocationError::Domain(CompleteError::InvalidRequest));
    }
    match tool_results {
        [] => Ok(named_tool_request(
            "call-text-uppercase",
            "text.uppercase",
            r#"{"text":"Lenso plugin"}"#,
        )),
        [result] if result.content == "LENSO PLUGIN" => Ok(text_plugin_result()),
        _ => Err(ModelInvocationError::Domain(CompleteError::InvalidRequest)),
    }
}

fn workspace_plugin_response(
    request: &CompleteRequest,
    tool_results: &[&CompleteRequestMessagesItem],
) -> Result<Vec<CompleteResponse>, ModelInvocationError> {
    if !request
        .tools
        .iter()
        .any(|tool| tool.name == "plugin.workspace_read_text")
    {
        return Err(ModelInvocationError::Domain(CompleteError::InvalidRequest));
    }
    match tool_results {
        [] => Ok(named_tool_request(
            "call-plugin-workspace-read",
            "plugin.workspace_read_text",
            r#"{"path":"README.md"}"#,
        )),
        [result] if result.content == "# Plugin Fixture\n" => Ok(vec![
            response(
                "1",
                CompleteResponseKind::TextDelta,
                "Workspace Plugin result: # Plugin Fixture",
                "",
                "",
                "{}",
                "0",
                "0",
            ),
            response(
                "2",
                CompleteResponseKind::Usage,
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
    request: &CompleteRequest,
    tool_results: &[&CompleteRequestMessagesItem],
) -> Result<Vec<CompleteResponse>, ModelInvocationError> {
    let has_skill_tools = ["skills.list", "skills.read"]
        .iter()
        .all(|name| request.tools.iter().any(|tool| tool.name.as_str() == *name));
    let skill_catalog_in_prompt = request.messages.iter().any(|message| {
        message.role == CompleteRequestMessagesItemRole::System
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
            "skills.read",
            r#"{"name":"rust-review"}"#,
        )),
        [skill] if skill.content.contains("RUST REVIEW INSTRUCTION") => {
            Ok(skill_applied_response())
        }
        _ => Err(ModelInvocationError::Domain(CompleteError::InvalidRequest)),
    }
}

fn resource_skill_response(
    request: &CompleteRequest,
    tool_results: &[&CompleteRequestMessagesItem],
) -> Result<Vec<CompleteResponse>, ModelInvocationError> {
    let has_skill_tools = [
        "skills.list",
        "skills.read",
        "skills.list_resources",
        "skills.read_resource",
    ]
    .iter()
    .all(|name| request.tools.iter().any(|tool| tool.name.as_str() == *name));
    if !has_skill_tools
        || !readonly_skill_tool_profile(request)
        || !request.messages.iter().any(|message| {
            message.role == CompleteRequestMessagesItemRole::System
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
            "skills.read",
            r#"{"name":"rust-review"}"#,
        )),
        [skill] if skill.content.contains("references/checklist.md") => Ok(named_tool_request(
            "call-resources-list",
            "skills.list_resources",
            r#"{"name":"rust-review"}"#,
        )),
        [_, manifest]
            if manifest.content.contains("references/checklist.md")
                && !manifest.content.contains("RESOURCE CHECKLIST CONTENT") =>
        {
            Ok(named_tool_request(
                "call-resource-read",
                "skills.read_resource",
                r#"{"name":"rust-review","path":"references/checklist.md"}"#,
            ))
        }
        [_, _, resource] if resource.content.contains("RESOURCE CHECKLIST CONTENT") => {
            Ok(resource_applied_response())
        }
        _ => Err(ModelInvocationError::Domain(CompleteError::InvalidRequest)),
    }
}

fn readonly_skill_tool_profile(request: &CompleteRequest) -> bool {
    request.tools.iter().all(|tool| {
        matches!(
            tool.name.as_str(),
            "workspace.list"
                | "workspace.search"
                | "workspace.read_text"
                | "skills.list"
                | "skills.read"
                | "skills.list_resources"
                | "skills.read_resource"
        )
    })
}

fn workspace_navigation_response(
    request: &CompleteRequest,
    tool_results: &[&CompleteRequestMessagesItem],
) -> Result<Vec<CompleteResponse>, ModelInvocationError> {
    let has_workspace_tools = ["workspace.list", "workspace.search", "workspace.read_text"]
        .iter()
        .all(|name| request.tools.iter().any(|tool| tool.name.as_str() == *name));
    if !has_workspace_tools {
        return Err(ModelInvocationError::Domain(CompleteError::InvalidRequest));
    }
    match tool_results {
        [] => Ok(named_tool_request(
            "call-workspace-list",
            "workspace.list",
            "{}",
        )),
        [listing] if listing.content.contains("docs") => Ok(named_tool_request(
            "call-workspace-search",
            "workspace.search",
            r#"{"query":"NAVIGATION_TARGET"}"#,
        )),
        [_, search] if search.content.contains("docs/guide.md") => Ok(named_tool_request(
            "call-workspace-read",
            "workspace.read_text",
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

fn navigation_response(first_line: &str) -> Vec<CompleteResponse> {
    vec![
        response(
            "1",
            CompleteResponseKind::TextDelta,
            format!("Navigation result: {first_line}"),
            "",
            "",
            "{}",
            "0",
            "0",
        ),
        response(
            "2",
            CompleteResponseKind::Usage,
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
    request: &CompleteRequest,
    tool_results: &[&CompleteRequestMessagesItem],
) -> Result<Vec<CompleteResponse>, ModelInvocationError> {
    let has_mutation_tools = [
        "workspace.write_text",
        "workspace.edit_text",
        "workspace.read_text",
    ]
    .iter()
    .all(|name| request.tools.iter().any(|tool| tool.name.as_str() == *name));
    if !has_mutation_tools {
        return Err(ModelInvocationError::Domain(CompleteError::InvalidRequest));
    }
    match tool_results {
        [] => Ok(named_tool_request(
            "call-workspace-write",
            "workspace.write_text",
            r#"{"path":"note.txt","content":"before\n"}"#,
        )),
        [created] if created.content == "created note.txt" => Ok(named_tool_request(
            "call-workspace-edit",
            "workspace.edit_text",
            r#"{"path":"note.txt","old_text":"before","new_text":"after"}"#,
        )),
        [_, edited] if edited.content == "edited note.txt" => Ok(named_tool_request(
            "call-workspace-read-after-edit",
            "workspace.read_text",
            r#"{"path":"note.txt"}"#,
        )),
        [_, _, document] if document.content == "after\n" => Ok(mutation_response()),
        _ => Err(ModelInvocationError::Domain(CompleteError::InvalidRequest)),
    }
}

fn mutation_response() -> Vec<CompleteResponse> {
    vec![
        response(
            "1",
            CompleteResponseKind::TextDelta,
            "Workspace mutation result: after",
            "",
            "",
            "{}",
            "0",
            "0",
        ),
        response(
            "2",
            CompleteResponseKind::Usage,
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
    request: &CompleteRequest,
    tool_results: &[&CompleteRequestMessagesItem],
) -> Result<Vec<CompleteResponse>, ModelInvocationError> {
    let has_coding_tools = ["workspace.edit_text", "process.exec", "workspace.read_text"]
        .iter()
        .all(|name| request.tools.iter().any(|tool| tool.name.as_str() == *name));
    if !has_coding_tools {
        return Err(ModelInvocationError::Domain(CompleteError::InvalidRequest));
    }
    match tool_results {
        [] => Ok(named_tool_request(
            "call-local-coding-edit",
            "workspace.edit_text",
            r#"{"path":"src/lib.rs","old_text":"pub fn value() -> u32 { 1 }","new_text":"pub fn value() -> u32 { 2 }"}"#,
        )),
        [edited] if edited.content == "edited src/lib.rs" => Ok(named_tool_request(
            "call-local-coding-check",
            "process.exec",
            r#"{"program":"cargo","arguments":["check","--quiet"]}"#,
        )),
        [_, checked] if checked.content.starts_with("exit_code: 0\n") => Ok(named_tool_request(
            "call-local-coding-read",
            "workspace.read_text",
            r#"{"path":"src/lib.rs"}"#,
        )),
        [_, _, document] if document.content.contains("pub fn value() -> u32 { 2 }") => {
            Ok(local_coding_final_response())
        }
        _ => Err(ModelInvocationError::Domain(CompleteError::InvalidRequest)),
    }
}

fn local_coding_final_response() -> Vec<CompleteResponse> {
    vec![
        response(
            "1",
            CompleteResponseKind::TextDelta,
            "Local coding result: cargo check passed.",
            "",
            "",
            "{}",
            "0",
            "0",
        ),
        response(
            "2",
            CompleteResponseKind::Usage,
            "",
            "",
            "",
            "{}",
            "48",
            "12",
        ),
    ]
}

fn direct_response(plugin_prefix: bool, filesystem_prefix: bool) -> Vec<CompleteResponse> {
    let prefix = match (filesystem_prefix, plugin_prefix) {
        (true, true) => "Filesystem: Plugin: ",
        (true, false) => "Filesystem: ",
        (false, true) => "Plugin: ",
        (false, false) => "",
    };
    vec![
        response(
            "1",
            CompleteResponseKind::TextDelta,
            format!("{prefix}Direct "),
            "",
            "",
            "{}",
            "0",
            "0",
        ),
        response(
            "2",
            CompleteResponseKind::TextDelta,
            "answer.",
            "",
            "",
            "{}",
            "0",
            "0",
        ),
        response("3", CompleteResponseKind::Usage, "", "", "", "{}", "8", "2"),
    ]
}

fn previous_response(previous: &str) -> Vec<CompleteResponse> {
    vec![
        response(
            "1",
            CompleteResponseKind::TextDelta,
            format!("Previous answer: {previous}"),
            "",
            "",
            "{}",
            "0",
            "0",
        ),
        response(
            "2",
            CompleteResponseKind::Usage,
            "",
            "",
            "",
            "{}",
            "16",
            "8",
        ),
    ]
}

fn tool_request(index: usize) -> Vec<CompleteResponse> {
    named_tool_request(
        &format!("call-readme-{index}"),
        "workspace.read_text",
        r#"{"path":"README.md"}"#,
    )
}

fn named_tool_request(
    call_id: &str,
    tool_name: &str,
    arguments_json: &str,
) -> Vec<CompleteResponse> {
    vec![
        response(
            "1",
            CompleteResponseKind::ToolCall,
            "",
            call_id,
            tool_name,
            arguments_json,
            "0",
            "0",
        ),
        response(
            "2",
            CompleteResponseKind::Usage,
            "",
            "",
            "",
            "{}",
            "24",
            "8",
        ),
    ]
}

fn skill_applied_response() -> Vec<CompleteResponse> {
    vec![
        response(
            "1",
            CompleteResponseKind::TextDelta,
            "Skill applied: Rust review used the selected instructions.",
            "",
            "",
            "{}",
            "0",
            "0",
        ),
        response(
            "2",
            CompleteResponseKind::Usage,
            "",
            "",
            "",
            "{}",
            "28",
            "10",
        ),
    ]
}

fn resource_applied_response() -> Vec<CompleteResponse> {
    vec![
        response(
            "1",
            CompleteResponseKind::TextDelta,
            "Resource applied: Rust review used references/checklist.md.",
            "",
            "",
            "{}",
            "0",
            "0",
        ),
        response(
            "2",
            CompleteResponseKind::Usage,
            "",
            "",
            "",
            "{}",
            "36",
            "12",
        ),
    ]
}

fn summary_response(first_line: &str) -> Vec<CompleteResponse> {
    vec![
        response(
            "1",
            CompleteResponseKind::TextDelta,
            format!("README summary: {first_line}"),
            "",
            "",
            "{}",
            "0",
            "0",
        ),
        response(
            "2",
            CompleteResponseKind::Usage,
            "",
            "",
            "",
            "{}",
            "32",
            "12",
        ),
    ]
}

fn text_plugin_result() -> Vec<CompleteResponse> {
    vec![
        response(
            "1",
            CompleteResponseKind::TextDelta,
            "Text Plugin result: LENSO PLUGIN",
            "",
            "",
            "{}",
            "0",
            "0",
        ),
        response(
            "2",
            CompleteResponseKind::Usage,
            "",
            "",
            "",
            "{}",
            "24",
            "8",
        ),
    ]
}

#[allow(clippy::too_many_arguments)]
fn response(
    sequence: &str,
    kind: CompleteResponseKind,
    text: impl Into<String>,
    tool_call_id: &str,
    tool_name: &str,
    arguments_json: &str,
    input_tokens: &str,
    output_tokens: &str,
) -> CompleteResponse {
    CompleteResponse {
        sequence: sequence.to_owned(),
        kind,
        text: text.into(),
        tool_call_id: tool_call_id.to_owned(),
        tool_name: tool_name.to_owned(),
        arguments_json: arguments_json.to_owned(),
        input_tokens: input_tokens.to_owned(),
        output_tokens: output_tokens.to_owned(),
    }
}
