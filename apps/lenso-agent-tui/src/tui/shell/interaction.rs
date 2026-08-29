//! Generation-pinned user-question draft state, input, and answer submission.
//!
//! Accepted answers resolve the blocked Tool call and never create a User
//! transcript entry.

use super::{
    ACTIVE_TICK, Duration, Focus, Instant, InteractionAnswer, InteractionQuestion, KeyCode,
    KeyEvent, KeyModifiers, PendingInteraction, PollStatus, TranscriptEntry, TuiState,
};
use std::collections::BTreeSet;

#[derive(Debug)]
pub(super) struct InteractionDraft {
    pub(super) question_index: usize,
    pub(super) option_cursors: Vec<usize>,
    pub(super) selected: Vec<BTreeSet<String>>,
    pub(super) other: Vec<Option<String>>,
    pub(super) editing_other: bool,
    pub(super) other_input: String,
}

impl InteractionDraft {
    pub(super) fn new(interaction: &PendingInteraction) -> Self {
        let question_count = interaction.questions.len();
        Self {
            question_index: 0,
            option_cursors: vec![0; question_count],
            selected: vec![BTreeSet::new(); question_count],
            other: vec![None; question_count],
            editing_other: false,
            other_input: String::new(),
        }
    }

    pub(super) fn option_cursor(&self) -> usize {
        self.option_cursors
            .get(self.question_index)
            .copied()
            .unwrap_or_default()
    }

    pub(super) fn set_option_cursor(&mut self, cursor: usize) {
        if let Some(slot) = self.option_cursors.get_mut(self.question_index) {
            *slot = cursor;
        }
    }
}

pub(super) fn handle_interaction_key(key: KeyEvent, state: &mut TuiState) {
    let Some(interaction) = state.pending_interaction.clone() else {
        return;
    };
    let Some(question_index) = state
        .interaction_draft
        .as_ref()
        .map(|draft| draft.question_index)
    else {
        return;
    };
    let Some(question) = interaction.questions.get(question_index) else {
        return;
    };
    if state
        .interaction_draft
        .as_ref()
        .is_some_and(|draft| draft.editing_other)
    {
        handle_interaction_other_key(key, state, &interaction, question.multi_select);
        return;
    }
    let Some(draft) = state.interaction_draft.as_mut() else {
        return;
    };

    let item_count = question.options.len() + 1;
    match key.code {
        KeyCode::Up | KeyCode::Char('k') | KeyCode::BackTab => {
            draft.set_option_cursor(
                draft
                    .option_cursor()
                    .checked_sub(1)
                    .unwrap_or(item_count - 1),
            );
        }
        KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
            draft.set_option_cursor((draft.option_cursor() + 1) % item_count);
        }
        KeyCode::Left | KeyCode::Char('h' | '[') => {
            draft.question_index = draft
                .question_index
                .checked_sub(1)
                .unwrap_or_else(|| interaction.questions.len().saturating_sub(1));
        }
        KeyCode::Right | KeyCode::Char('l' | ']') => {
            draft.question_index = (draft.question_index + 1) % interaction.questions.len();
        }
        KeyCode::Char(' ') => toggle_focused_interaction_option(draft, question),
        KeyCode::Char('z') => {
            draft.set_option_cursor(question.options.len());
            draft.other_input = draft.other[draft.question_index]
                .clone()
                .unwrap_or_default();
            draft.editing_other = true;
        }
        KeyCode::Char(character) => {
            if select_interaction_shortcut(draft, question, character) {
                advance_interaction_question(state, &interaction);
            }
        }
        KeyCode::Enter => {
            if let Some(option) = question.options.get(draft.option_cursor()) {
                if question.multi_select {
                    if !draft.selected[draft.question_index].is_empty()
                        || draft.other[draft.question_index].is_some()
                    {
                        advance_interaction_question(state, &interaction);
                    }
                } else {
                    draft.selected[draft.question_index].clear();
                    draft.selected[draft.question_index].insert(option.option_id.clone());
                    draft.other[draft.question_index] = None;
                    advance_interaction_question(state, &interaction);
                }
            } else {
                draft.other_input = draft.other[draft.question_index]
                    .clone()
                    .unwrap_or_default();
                draft.editing_other = true;
            }
        }
        KeyCode::Esc => state.focus = Focus::Scrollback,
        _ => {}
    }
}

fn toggle_focused_interaction_option(draft: &mut InteractionDraft, question: &InteractionQuestion) {
    if let Some(option) = question.options.get(draft.option_cursor()) {
        let selected = &mut draft.selected[draft.question_index];
        if question.multi_select {
            if !selected.insert(option.option_id.clone()) {
                selected.remove(&option.option_id);
            }
        } else if selected.contains(&option.option_id) {
            selected.clear();
        } else {
            selected.clear();
            selected.insert(option.option_id.clone());
        }
    } else {
        draft.other_input = draft.other[draft.question_index]
            .clone()
            .unwrap_or_default();
        draft.editing_other = true;
    }
}

fn select_interaction_shortcut(
    draft: &mut InteractionDraft,
    question: &InteractionQuestion,
    character: char,
) -> bool {
    let Some(index) =
        interaction_option_index(character).filter(|index| *index < question.options.len())
    else {
        return false;
    };
    draft.set_option_cursor(index);
    let option = &question.options[index];
    if question.multi_select {
        let selected = &mut draft.selected[draft.question_index];
        if !selected.insert(option.option_id.clone()) {
            selected.remove(&option.option_id);
        }
        false
    } else {
        draft.selected[draft.question_index].clear();
        draft.selected[draft.question_index].insert(option.option_id.clone());
        draft.other[draft.question_index] = None;
        true
    }
}

fn handle_interaction_other_key(
    key: KeyEvent,
    state: &mut TuiState,
    interaction: &PendingInteraction,
    multi_select: bool,
) {
    let Some(draft) = state.interaction_draft.as_mut() else {
        return;
    };
    match key.code {
        KeyCode::Esc => {
            draft.editing_other = false;
            draft.other_input.clear();
        }
        KeyCode::Backspace => {
            draft.other_input.pop();
        }
        KeyCode::Enter
            if key
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
        {
            append_interaction_other(state, "\n");
        }
        KeyCode::Enter if !draft.other_input.trim().is_empty() => {
            draft.other[draft.question_index] = Some(draft.other_input.trim().to_owned());
            draft.other_input.clear();
            draft.editing_other = false;
            if !multi_select {
                advance_interaction_question(state, interaction);
            }
        }
        KeyCode::Char(character) => append_interaction_other(state, &character.to_string()),
        _ => {}
    }
}

fn interaction_option_index(character: char) -> Option<usize> {
    match character {
        '1'..='9' => Some(usize::from(character as u8 - b'1')),
        'a'..='y' => Some(9 + usize::from(character as u8 - b'a')),
        _ => None,
    }
}

pub(super) fn append_interaction_other(state: &mut TuiState, text: &str) {
    let Some(draft) = state.interaction_draft.as_mut() else {
        return;
    };
    let remaining = 4_096usize.saturating_sub(draft.other_input.chars().count());
    draft.other_input.extend(text.chars().take(remaining));
}

fn advance_interaction_question(state: &mut TuiState, interaction: &PendingInteraction) {
    let Some(draft) = state.interaction_draft.as_mut() else {
        return;
    };
    if draft.question_index + 1 < interaction.questions.len() {
        draft.question_index += 1;
        return;
    }
    state.pending_answers = Some(
        interaction
            .questions
            .iter()
            .enumerate()
            .map(|(index, question)| InteractionAnswer {
                question_id: question.question_id.clone(),
                selected_option_ids: draft.selected[index].iter().cloned().collect(),
                other: Some(draft.other[index].clone()),
            })
            .collect(),
    );
}

pub(super) async fn sync_user_interaction(state: &mut TuiState) {
    if state.active.is_none() {
        state.pending_interaction = None;
        state.interaction_draft = None;
        state.pending_answers = None;
        return;
    }

    if let (Some(interaction), Some(answers)) = (
        state.pending_interaction.clone(),
        state.pending_answers.take(),
    ) {
        let result = {
            let active = state.active.as_ref().expect("active Turn checked");
            active
                .lease
                .answer_interaction(interaction.interaction_id.clone(), answers.clone())
                .await
        };
        finish_interaction_submission(state, result);
    }

    if state.pending_interaction.is_some() || Instant::now() < state.next_interaction_poll {
        return;
    }
    state.next_interaction_poll = Instant::now() + ACTIVE_TICK;
    let result = {
        let active = state.active.as_ref().expect("active Turn checked");
        active.lease.pending_interactions().await
    };
    match result {
        Ok(interactions) => {
            state.interaction_poll_status = PollStatus::Ready;
            if let Some(interaction) = interactions.into_iter().next() {
                state.interaction_draft = Some(InteractionDraft::new(&interaction));
                state.pending_interaction = Some(interaction);
                state.focus = Focus::Prompt;
            }
        }
        Err(error) => {
            state.next_interaction_poll = Instant::now() + Duration::from_secs(2);
            if state.interaction_poll_status == PollStatus::Ready {
                state.transcript.push(TranscriptEntry::Error {
                    text: format!("Could not read pending user questions: {error}"),
                });
                state.interaction_poll_status = PollStatus::ErrorReported;
            }
        }
    }
}

pub(super) fn finish_interaction_submission(state: &mut TuiState, result: Result<(), String>) {
    match result {
        Ok(()) => {
            // This resolves the blocked ask_user Tool call. It is not a new
            // conversational prompt, so it must not become a User entry.
            state.pending_interaction = None;
            state.interaction_draft = None;
            state.next_interaction_poll = Instant::now() + ACTIVE_TICK;
        }
        Err(error) => state.transcript.push(TranscriptEntry::Error {
            text: format!("Answer was not accepted: {error}"),
        }),
    }
}
