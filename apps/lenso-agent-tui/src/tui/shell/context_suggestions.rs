//! Bounded projection of composed Context Sources into startup suggestions.

use super::{AgentApp, Suggestion, SuggestionKind};

pub(super) async fn context_source_suggestions(app: &AgentApp) -> Result<Vec<Suggestion>, String> {
    let snapshot = app.tui_context_sources().await?;
    let mut suggestions = Vec::new();
    for (index, prompt) in snapshot.prompts.into_iter().enumerate() {
        let schema: serde_json::Value = serde_json::from_str(prompt.arguments_schema_json.as_str())
            .map_err(|error| format!("Context Prompt schema is invalid: {error}"))?;
        if schema["required"]
            .as_array()
            .is_some_and(|required| !required.is_empty())
            || !safe_context_token(&prompt.source)
            || !safe_context_token(&prompt.name)
        {
            continue;
        }
        suggestions.push(Suggestion {
            id: format!("mcp.prompt.{index}"),
            kind: SuggestionKind::Prompt,
            label: format!("/prompt:{}/{}", prompt.source, prompt.name),
            insert_text: format!("/mcp-prompt {}/{}", prompt.source, prompt.name),
            description: prompt.description,
        });
    }
    for (index, resource) in snapshot.resources.into_iter().enumerate() {
        if !safe_context_token(&resource.source) || resource.uri.contains(char::is_whitespace) {
            continue;
        }
        suggestions.push(Suggestion {
            id: format!("mcp.resource.{index}"),
            kind: SuggestionKind::Resource,
            label: format!("/resource:{}/{}", resource.source, resource.name),
            insert_text: format!("/mcp-resource {}={}", resource.source, resource.uri),
            description: resource.description,
        });
    }
    Ok(suggestions)
}

fn safe_context_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}
