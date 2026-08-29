//! Bounded composer editing, history, queueing, and suggestion selection.

use super::{
    Focus, MAX_INPUT_CHARACTERS, MAX_VISIBLE_SUGGESTIONS, SuggestionKind, SuggestionMatch,
    SuggestionVisibility, TuiState, char_to_byte,
};

impl TuiState {
    pub(super) fn queue_input(&mut self) {
        let input = self.take_input();
        if input.trim().is_empty() {
            return;
        }
        self.queued_inputs.push_back(input);
        self.queue_hovered = Some(self.queued_inputs.len().saturating_sub(1));
    }

    pub(super) fn edit_queued_input(&mut self, index: usize) {
        let Some(input) = self.queued_inputs.remove(index) else {
            return;
        };
        self.set_input(input);
        self.focus = Focus::Prompt;
        self.queue_hovered = None;
    }

    pub(super) fn cancel_queued_input(&mut self, index: usize) {
        self.queued_inputs.remove(index);
        self.queue_hovered = None;
    }

    pub(super) fn append_input(&mut self, text: &str) {
        let remaining = MAX_INPUT_CHARACTERS.saturating_sub(self.input_characters);
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let accepted: String = normalized.chars().take(remaining).collect();
        let accepted_characters = accepted.chars().count();
        let byte = char_to_byte(&self.input, self.input_cursor);
        self.input.insert_str(byte, &accepted);
        self.input_characters += accepted_characters;
        self.input_cursor += accepted_characters;
        self.leave_history();
        self.reset_suggestion_selection();
    }

    pub(super) fn pop_input(&mut self) {
        if self.input_cursor == 0 {
            return;
        }
        let end = char_to_byte(&self.input, self.input_cursor);
        let start = char_to_byte(&self.input, self.input_cursor - 1);
        self.input.replace_range(start..end, "");
        self.input_cursor -= 1;
        self.input_characters -= 1;
        self.leave_history();
        self.reset_suggestion_selection();
    }

    pub(super) fn delete_input(&mut self) {
        if self.input_cursor >= self.input_characters {
            return;
        }
        let start = char_to_byte(&self.input, self.input_cursor);
        let end = char_to_byte(&self.input, self.input_cursor + 1);
        self.input.replace_range(start..end, "");
        self.input_characters -= 1;
        self.leave_history();
        self.reset_suggestion_selection();
    }

    pub(super) fn move_cursor(&mut self, delta: isize) {
        self.input_cursor = self
            .input_cursor
            .saturating_add_signed(delta)
            .min(self.input_characters);
        self.reset_suggestion_selection();
    }

    pub(super) fn move_line_edge(&mut self, end: bool) {
        let chars: Vec<char> = self.input.chars().collect();
        if end {
            self.input_cursor += chars[self.input_cursor..]
                .iter()
                .position(|character| *character == '\n')
                .unwrap_or(chars.len() - self.input_cursor);
        } else {
            self.input_cursor = chars[..self.input_cursor]
                .iter()
                .rposition(|character| *character == '\n')
                .map_or(0, |position| position + 1);
        }
    }

    pub(super) fn move_vertical(&mut self, up: bool) {
        let chars: Vec<char> = self.input.chars().collect();
        let line_start = chars[..self.input_cursor]
            .iter()
            .rposition(|character| *character == '\n')
            .map_or(0, |position| position + 1);
        let column = self.input_cursor - line_start;
        if up {
            if line_start == 0 {
                return;
            }
            let previous_end = line_start - 1;
            let previous_start = chars[..previous_end]
                .iter()
                .rposition(|character| *character == '\n')
                .map_or(0, |position| position + 1);
            self.input_cursor = previous_start + column.min(previous_end - previous_start);
        } else {
            let Some(next_offset) = chars[self.input_cursor..]
                .iter()
                .position(|character| *character == '\n')
            else {
                return;
            };
            let next_start = self.input_cursor + next_offset + 1;
            let next_end = chars[next_start..]
                .iter()
                .position(|character| *character == '\n')
                .map_or(chars.len(), |offset| next_start + offset);
            self.input_cursor = next_start + column.min(next_end - next_start);
        }
    }

    pub(super) fn delete_previous_word(&mut self) {
        let chars: Vec<char> = self.input.chars().collect();
        let mut start = self.input_cursor;
        while start > 0 && chars[start - 1].is_whitespace() {
            start -= 1;
        }
        while start > 0 && !chars[start - 1].is_whitespace() {
            start -= 1;
        }
        if start == self.input_cursor {
            return;
        }
        let start_byte = char_to_byte(&self.input, start);
        let end_byte = char_to_byte(&self.input, self.input_cursor);
        self.input.replace_range(start_byte..end_byte, "");
        self.input_characters -= self.input_cursor - start;
        self.input_cursor = start;
        self.leave_history();
    }

    pub(super) fn previous_history(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        let next = match self.history_cursor {
            Some(index) => index.saturating_sub(1),
            None => {
                self.history_draft.clone_from(&self.input);
                self.input_history.len() - 1
            }
        };
        self.history_cursor = Some(next);
        self.set_input(self.input_history[next].clone());
    }

    pub(super) fn next_history(&mut self) {
        let Some(index) = self.history_cursor else {
            return;
        };
        if index + 1 < self.input_history.len() {
            self.history_cursor = Some(index + 1);
            self.set_input(self.input_history[index + 1].clone());
        } else {
            self.history_cursor = None;
            let draft = std::mem::take(&mut self.history_draft);
            self.set_input(draft);
        }
    }

    pub(super) fn set_input(&mut self, input: String) {
        self.input_characters = input.chars().count();
        self.input_cursor = self.input_characters;
        self.input = input;
        self.reset_suggestion_selection();
    }

    pub(super) fn leave_history(&mut self) {
        self.history_cursor = None;
        self.history_draft.clear();
    }

    pub(super) fn take_input(&mut self) -> String {
        self.input_characters = 0;
        self.input_cursor = 0;
        self.history_cursor = None;
        self.history_draft.clear();
        std::mem::take(&mut self.input)
    }

    pub(super) fn reset_suggestion_selection(&mut self) {
        self.suggestion_selected = 0;
        self.suggestion_scroll = 0;
        self.suggestion_visibility = SuggestionVisibility::Auto;
    }

    pub(super) fn suggestion_match(&self) -> Option<SuggestionMatch> {
        if self.turn_is_running()
            || self.focus != Focus::Prompt
            || self.suggestion_visibility == SuggestionVisibility::Dismissed
        {
            return None;
        }
        let before = self
            .input
            .chars()
            .take(self.input_cursor)
            .collect::<String>();
        let line_start = before.rfind('\n').map_or(0, |index| index + 1);
        let line = &before[line_start..];
        let (start, query, matches_kind) =
            if line.starts_with('/') && !line.contains(char::is_whitespace) {
                (
                    self.input_cursor - line.chars().count(),
                    line.to_ascii_lowercase(),
                    true,
                )
            } else {
                let token = line
                    .rsplit_once(char::is_whitespace)
                    .map_or(line, |(_, token)| token);
                if !token.starts_with('@') || token[1..].contains('@') {
                    return None;
                }
                (
                    self.input_cursor - token.chars().count(),
                    token[1..].to_ascii_lowercase(),
                    false,
                )
            };
        let mut indices = self
            .suggestions
            .iter()
            .enumerate()
            .filter(|(_, suggestion)| {
                if matches_kind {
                    matches!(
                        suggestion.kind,
                        SuggestionKind::Command
                            | SuggestionKind::Prompt
                            | SuggestionKind::Resource
                            | SuggestionKind::Skill
                    )
                } else {
                    suggestion.kind == SuggestionKind::File
                }
            })
            .filter(|(_, suggestion)| suggestion.label.to_ascii_lowercase().contains(&query))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        indices.sort_by_key(|index| {
            let label = self.suggestions[*index].label.to_ascii_lowercase();
            (!label.starts_with(&query), label)
        });
        (!indices.is_empty()).then_some(SuggestionMatch { start, indices })
    }

    pub(super) fn select_suggestion(&mut self, previous: bool) -> bool {
        let Some(matches) = self.suggestion_match() else {
            return false;
        };
        let len = matches.indices.len();
        self.suggestion_selected = if previous {
            self.suggestion_selected.checked_sub(1).unwrap_or(len - 1)
        } else {
            (self.suggestion_selected + 1) % len
        };
        if self.suggestion_selected < self.suggestion_scroll {
            self.suggestion_scroll = self.suggestion_selected;
        } else if self.suggestion_selected >= self.suggestion_scroll + MAX_VISIBLE_SUGGESTIONS {
            self.suggestion_scroll = self.suggestion_selected + 1 - MAX_VISIBLE_SUGGESTIONS;
        }
        true
    }

    pub(super) fn accept_suggestion(&mut self) -> Option<SuggestionKind> {
        let matches = self.suggestion_match()?;
        let selected = self.suggestion_selected.min(matches.indices.len() - 1);
        let suggestion = &self.suggestions[matches.indices[selected]];
        let kind = suggestion.kind.clone();
        let mut replacement = suggestion.insert_text.clone();
        if matches!(
            suggestion.kind,
            SuggestionKind::File
                | SuggestionKind::Prompt
                | SuggestionKind::Resource
                | SuggestionKind::Skill
        ) {
            replacement.push(' ');
        }
        let start_byte = char_to_byte(&self.input, matches.start);
        let end_byte = char_to_byte(&self.input, self.input_cursor);
        self.input.replace_range(start_byte..end_byte, &replacement);
        self.input_characters = self.input.chars().count();
        self.input_cursor = matches.start + replacement.chars().count();
        self.leave_history();
        self.reset_suggestion_selection();
        Some(kind)
    }
}
