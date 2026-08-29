//! Turn submission, local commands, and explicit Context Source composition.

use super::{
    ActiveTurn, AgentApp, ContextRole, Instant, MAX_INPUT_CHARACTERS, RUN_TURN_OPERATION,
    ReadResourceRequest, RenderPromptRequest, RunScope, RunTurnRequest, ScrollState,
    TranscriptEntry, TuiOptions, TuiState, UiPhase, current_timestamp,
};

pub(super) async fn submit(
    app: &AgentApp,
    options: &TuiOptions,
    state: &mut TuiState,
) -> Result<(), String> {
    let started_at = Instant::now();
    let input = state.take_input();
    if input.chars().count() > MAX_INPUT_CHARACTERS {
        return Err(format!(
            "Agent input exceeds the {MAX_INPUT_CHARACTERS}-character limit"
        ));
    }
    match input.trim() {
        "/help" => {
            state.show_shortcuts = true;
            state.phase = UiPhase::Idle;
            return Ok(());
        }
        "/clear" => {
            state.transcript.clear();
            state.scroll = ScrollState::default();
            state.phase = UiPhase::Idle;
            return Ok(());
        }
        "/new" => {
            state.transcript.clear();
            state.session_id = None;
            state.scroll = ScrollState::default();
            state.phase = UiPhase::Idle;
            return Ok(());
        }
        _ => {}
    }
    if let Some(title) = rename_command(&input)? {
        let session_id = state
            .session_id
            .clone()
            .ok_or_else(|| "Start the Session before renaming it".to_owned())?;
        let lease = app.lease_tui_turn().await?;
        let current = lease.read_session(session_id.clone(), 0, 1).await?;
        let renamed = lease
            .rename_session(
                session_id,
                title.to_owned(),
                current.title_revision.unwrap_or_else(|| "0".to_owned()),
            )
            .await
            .map_err(|error| format!("Session rename failed: {error:?}"))?;
        state.transcript.push(TranscriptEntry::System {
            text: format!("Session renamed to ‘{}’", renamed.title),
        });
        state.phase = UiPhase::Idle;
        return Ok(());
    }
    let model_input = compose_tui_context(app, &input).await?;
    state.transcript.push(TranscriptEntry::User {
        text: input.clone(),
        created_at: current_timestamp(),
    });
    state.start_provisional_thinking();
    if state.input_history.last() != Some(&input) {
        state.input_history.push(input.clone());
    }
    state.phase = UiPhase::Active;
    state.scroll.begin_page_flip();

    let lease = app.lease_tui_turn().await?;
    let mut context = lease.invocation_context()?;
    if let Some(allowed_tools) = options.allowed_tools.clone() {
        context = RunScope::new(allowed_tools)?.attach(context)?;
    }
    let stream = lease
        .handle()
        .open_with_context(
            RUN_TURN_OPERATION,
            context,
            RunTurnRequest {
                input: model_input,
                session_id: state.session_id.clone(),
            },
        )
        .await
        .map_err(|error| format!("Agent stream failed to open: {error:?}"))?
        .map_err(|error| format!("Agent rejected the turn: {error:?}"))?;
    stream
        .close_send()
        .await
        .map_err(|error| format!("failed to half-close Agent input: {error:?}"))?;
    state.active = Some(ActiveTurn {
        stream,
        lease,
        started_at,
    });
    Ok(())
}

pub(in crate::tui::shell) fn rename_command(input: &str) -> Result<Option<&str>, String> {
    let input = input.trim();
    if input == "/rename" {
        return Err("Usage: /rename <title>".to_owned());
    }
    let Some(title) = input.strip_prefix("/rename ") else {
        return Ok(None);
    };
    let title = title.trim();
    if title.is_empty() {
        Err("Usage: /rename <title>".to_owned())
    } else {
        Ok(Some(title))
    }
}

async fn compose_tui_context(app: &AgentApp, input: &str) -> Result<String, String> {
    if let Some(selection) = input.strip_prefix("/mcp-prompt ") {
        let (identity, task) = selection.split_once(char::is_whitespace).ok_or_else(|| {
            "an MCP Prompt selection must be followed by the user task".to_owned()
        })?;
        let (source, name) = identity
            .split_once('/')
            .ok_or_else(|| "invalid MCP Prompt source/name".to_owned())?;
        let rendered = app
            .render_tui_context_prompt(RenderPromptRequest {
                source: source.to_owned(),
                name: name.to_owned(),
                arguments_json: "{}"
                    .to_owned()
                    .try_into()
                    .expect("empty JSON object is valid"),
            })
            .await?;
        let messages = rendered
            .messages
            .into_iter()
            .map(|message| {
                let role = match message.role {
                    ContextRole::User => "user",
                    ContextRole::Assistant => "assistant",
                };
                format!("[{role}]\n{}", message.text)
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        return Ok(format!(
            "Selected Context Prompt: {source}/{name}\n{messages}\n\n---\n\nUser task:\n{}",
            task.trim()
        ));
    }
    if let Some(selection) = input.strip_prefix("/mcp-resource ") {
        let (identity, task) = selection.split_once(char::is_whitespace).ok_or_else(|| {
            "an MCP Resource selection must be followed by the user task".to_owned()
        })?;
        let (source, uri) = identity
            .split_once('=')
            .ok_or_else(|| "invalid MCP Resource source=URI".to_owned())?;
        let response = app
            .read_tui_context_resource(ReadResourceRequest {
                source: source.to_owned(),
                uri: uri.to_owned(),
            })
            .await?;
        let contents = response
            .contents
            .into_iter()
            .map(|content| {
                format!(
                    "URI: {}\nMIME: {}\n{}",
                    content.uri, content.mime_type, content.text
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        return Ok(format!(
            "Selected Context Resource: {source}/{uri}\n{contents}\n\n---\n\nUser task:\n{}",
            task.trim()
        ));
    }
    Ok(input.to_owned())
}
