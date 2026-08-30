//! Turn submission, local commands, and explicit Context Source composition.

use super::{
    ActiveTurn, AgentApp, ContextRole, Instant, MAX_INPUT_CHARACTERS, RUN_TURN_OPERATION,
    ReadResourceRequest, RenderPromptRequest, RunScope, RunTurnRequest, ScrollState, SessionMode,
    TranscriptEntry, TuiOptions, TuiState, UiPhase, current_timestamp,
};
use std::rc::Rc;

pub(super) async fn apply_pending_mode(app: &AgentApp, state: &mut TuiState) {
    let Some(mode) = state.pending_mode.take() else {
        return;
    };
    match app.select_profile(mode.profile()).await {
        Ok(()) => {
            state.mode = mode;
            state.advance_task_generation_epoch();
            state.push_system(format!(
                "Mode switched to `{}` through the Generation Ready Gate",
                state.mode.label()
            ));
        }
        Err(error) => state.push_system(format!("Mode switch failed: {error}")),
    }
}

pub(super) async fn submit(
    app: &AgentApp,
    _options: &TuiOptions,
    state: &mut TuiState,
) -> Result<(), String> {
    let started_at = Instant::now();
    let input = state.take_input();
    if input.chars().count() > MAX_INPUT_CHARACTERS {
        return Err(format!(
            "Agent input exceeds the {MAX_INPUT_CHARACTERS}-character limit"
        ));
    }
    if handle_builtin_command(state, &input) || handle_control_command(app, state, &input).await? {
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
    let mut context = lease.invocation_context_for_model_options(
        state.selected_model.as_deref(),
        state.selected_reasoning_effort.as_deref(),
        state.selected_service_tier.as_deref(),
    )?;
    if let Some(allowed_tools) = state.allowed_tools.clone() {
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
    let task_scope_id = state.next_task_turn_scope_id();
    state.active = Some(ActiveTurn {
        stream,
        lease: Rc::new(lease),
        task_scope_id,
        started_at,
    });
    Ok(())
}

fn handle_builtin_command(state: &mut TuiState, input: &str) -> bool {
    match input.trim() {
        "/help" => state.show_shortcuts = true,
        "/clear" => {
            state.transcript.clear();
            state.scroll = ScrollState::default();
        }
        "/new" => {
            state.transcript.clear();
            state.session_id = None;
            state.scroll = ScrollState::default();
        }
        _ => return false,
    }
    state.phase = UiPhase::Idle;
    true
}

#[allow(
    clippy::too_many_lines,
    reason = "one command dispatcher keeps mutually exclusive TUI controls ordered"
)]
async fn handle_control_command(
    app: &AgentApp,
    state: &mut TuiState,
    input: &str,
) -> Result<bool, String> {
    let message = match input.trim() {
        "/compact" => {
            let session_id = state
                .session_id
                .clone()
                .ok_or_else(|| "Start the Session before compacting it".to_owned())?;
            let compacted = app
                .lease_tui_turn()
                .await?
                .compact_session(session_id)
                .await?;
            format!(
                "Context compacted through revision {} ({} messages)",
                compacted.compacted_through_revision, compacted.source_message_count
            )
        }
        "/model" => {
            let lease = app.lease_tui_turn().await?;
            let models = lease.available_models();
            if models.is_empty() {
                "No models are exposed by the selected Provider Instance".to_owned()
            } else if lease.supports_dynamic_model_selection() {
                format!(
                    "Available concrete models: {}. Configured dynamic policy aliases may also be selected.",
                    models.join(", ")
                )
            } else {
                format!("Available models: {}", models.join(", "))
            }
        }
        "/permissions" => match &state.allowed_tools {
            None => "Permissions: composed Tool authority".to_owned(),
            Some(tools) if tools.is_empty() => "Permissions: no Tools".to_owned(),
            Some(tools) => format!("Permissions: {}", tools.join(", ")),
        },
        "/mode" => format!("Mode: {}", state.mode.label()),
        "/thinking" => state.selected_reasoning_effort.as_ref().map_or_else(
            || "Thinking: model default".to_owned(),
            |effort| format!("Thinking: {effort}"),
        ),
        "/fast" => format!(
            "Fast mode: {}",
            if state.selected_service_tier.as_deref() == Some("fast") {
                "on"
            } else {
                "off"
            }
        ),
        _ => {
            if let Some(model) = model_command(input)? {
                let lease = app.lease_tui_turn().await?;
                let retained_options = lease.invocation_context_for_model_options(
                    Some(model),
                    state.selected_reasoning_effort.as_deref(),
                    state.selected_service_tier.as_deref(),
                );
                let reset_options = retained_options.is_err();
                if reset_options {
                    lease.invocation_context_for_model(model)?;
                    state.selected_reasoning_effort = None;
                    state.selected_service_tier = None;
                }
                state.selected_model = Some(model.to_owned());
                if reset_options {
                    format!(
                        "Model set to `{model}`; unsupported thinking/fast selections were reset"
                    )
                } else {
                    format!("Model set to `{model}` for subsequent Turns")
                }
            } else if let Some(mode) = mode_command(input)? {
                state.pending_mode = Some(mode);
                "Switching mode through the Generation Ready Gate…".to_owned()
            } else if let Some(effort) = thinking_command(input)? {
                let lease = app.lease_tui_turn().await?;
                let effort = match effort {
                    ThinkingSelection::Default => None,
                    ThinkingSelection::Effort(effort) => Some(effort),
                };
                lease.invocation_context_for_model_options(
                    state.selected_model.as_deref(),
                    effort,
                    state.selected_service_tier.as_deref(),
                )?;
                state.selected_reasoning_effort = effort.map(str::to_owned);
                state.selected_reasoning_effort.as_ref().map_or_else(
                    || "Thinking reset to the model default".to_owned(),
                    |effort| format!("Thinking set to `{effort}`"),
                )
            } else if let Some(fast) = fast_command(input)? {
                let service_tier = match fast {
                    FastSelection::On => Some("fast"),
                    FastSelection::Off => None,
                };
                app.lease_tui_turn()
                    .await?
                    .invocation_context_for_model_options(
                        state.selected_model.as_deref(),
                        state.selected_reasoning_effort.as_deref(),
                        service_tier,
                    )?;
                state.selected_service_tier = service_tier.map(str::to_owned);
                format!(
                    "Fast mode {}",
                    if service_tier.is_some() {
                        "enabled"
                    } else {
                        "disabled"
                    }
                )
            } else if let Some(selection) = permissions_command(input)? {
                state.allowed_tools = match selection {
                    PermissionSelection::Composed => None,
                    PermissionSelection::Restricted(tools) => Some(tools),
                };
                state.tool_scope = permission_scope(state.allowed_tools.as_deref());
                format!("Permissions set to {}", state.tool_scope)
            } else if let Some(title) = rename_command(input)? {
                rename_session(app, state, title).await?
            } else {
                return Ok(false);
            }
        }
    };
    state
        .transcript
        .push(TranscriptEntry::System { text: message });
    state.phase = UiPhase::Idle;
    Ok(true)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::tui::shell) enum ThinkingSelection<'a> {
    Default,
    Effort(&'a str),
}

pub(in crate::tui::shell) fn thinking_command(
    input: &str,
) -> Result<Option<ThinkingSelection<'_>>, String> {
    let input = input.trim();
    let Some(effort) = input.strip_prefix("/thinking ") else {
        return Ok(None);
    };
    let effort = effort.trim();
    if effort == "default" {
        Ok(Some(ThinkingSelection::Default))
    } else if matches!(
        effort,
        "low" | "medium" | "high" | "xhigh" | "max" | "ultra"
    ) {
        Ok(Some(ThinkingSelection::Effort(effort)))
    } else {
        Err("Usage: /thinking <default|low|medium|high|xhigh|max|ultra>".to_owned())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::tui::shell) enum FastSelection {
    On,
    Off,
}

pub(in crate::tui::shell) fn fast_command(input: &str) -> Result<Option<FastSelection>, String> {
    let input = input.trim();
    let Some(value) = input.strip_prefix("/fast ") else {
        return Ok(None);
    };
    match value.trim() {
        "on" => Ok(Some(FastSelection::On)),
        "off" => Ok(Some(FastSelection::Off)),
        _ => Err("Usage: /fast <on|off>".to_owned()),
    }
}

pub(in crate::tui::shell) fn mode_command(input: &str) -> Result<Option<SessionMode>, String> {
    let input = input.trim();
    let Some(mode) = input.strip_prefix("/mode ") else {
        return Ok(None);
    };
    match mode.trim() {
        "normal" => Ok(Some(SessionMode::Normal)),
        "plan" => Ok(Some(SessionMode::Plan)),
        "auto" => Ok(Some(SessionMode::Auto)),
        _ => Err("Usage: /mode <normal|plan|auto>".to_owned()),
    }
}

async fn rename_session(app: &AgentApp, state: &TuiState, title: &str) -> Result<String, String> {
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
    Ok(format!("Session renamed to ‘{}’", renamed.title))
}

fn permission_scope(allowed_tools: Option<&[String]>) -> String {
    match allowed_tools {
        None => "composed tools".to_owned(),
        Some([]) => "no tools".to_owned(),
        Some(tools) => format!("{} scoped tools", tools.len()),
    }
}

pub(in crate::tui::shell) fn model_command(input: &str) -> Result<Option<&str>, String> {
    let input = input.trim();
    let Some(model) = input.strip_prefix("/model ") else {
        return Ok(None);
    };
    let model = model.trim();
    if model.is_empty() || model.chars().any(char::is_whitespace) {
        Err("Usage: /model <model-id>".to_owned())
    } else {
        Ok(Some(model))
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(in crate::tui::shell) enum PermissionSelection {
    Composed,
    Restricted(Vec<String>),
}

pub(in crate::tui::shell) fn permissions_command(
    input: &str,
) -> Result<Option<PermissionSelection>, String> {
    let input = input.trim();
    let Some(selection) = input.strip_prefix("/permissions ") else {
        return Ok(None);
    };
    let selection = selection.trim();
    match selection {
        "composed" => Ok(Some(PermissionSelection::Composed)),
        "none" => Ok(Some(PermissionSelection::Restricted(Vec::new()))),
        _ => {
            let Some(tools) = selection.strip_prefix("allow ") else {
                return Err("Usage: /permissions <composed|none|allow tool[,tool...]>".to_owned());
            };
            let tools = tools
                .split(',')
                .map(str::trim)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            RunScope::new(tools.clone())?;
            Ok(Some(PermissionSelection::Restricted(tools)))
        }
    }
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
                let body = content.text.flatten().unwrap_or_else(|| {
                    format!(
                        "[binary resource available: {} bytes base64]",
                        content.data_base64.flatten().map_or(0, |data| data.len())
                    )
                });
                format!(
                    "URI: {}\nMIME: {}\n{}",
                    content.uri, content.mime_type, body
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
