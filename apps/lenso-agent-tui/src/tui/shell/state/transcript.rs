//! Volatile transcript mutations and semantic selection state.

use super::{
    Focus, RunTurnResponse, ThinkingCard, ToolCard, ToolSelection, ToolStatus, TranscriptEntry,
    TuiState, current_timestamp,
};

impl TuiState {
    pub(super) fn append_agent_text(&mut self, text: &str) {
        self.finish_provisional_thinking();
        if let Some(last) = self.transcript.last_mut()
            && let TranscriptEntry::Agent { text: existing, .. } = last
        {
            existing.push_str(text);
            return;
        }
        self.transcript.push(TranscriptEntry::Agent {
            text: text.to_owned(),
            created_at: current_timestamp(),
        });
    }

    pub(super) fn start_provisional_thinking(&mut self) {
        self.transcript
            .push(TranscriptEntry::Thinking(ThinkingCard::provisional()));
    }

    pub(super) fn append_reasoning(&mut self, message: RunTurnResponse) {
        let Some(reasoning_id) = message.reasoning_id else {
            self.push_system("Ignored reasoning without an ID");
            return;
        };
        if let Some(TranscriptEntry::Thinking(card)) = self.transcript.last_mut()
            && card.is_running()
            && card
                .reasoning_id
                .as_deref()
                .is_none_or(|current| current == reasoning_id)
        {
            card.append(reasoning_id, &message.text);
            return;
        }
        let mut card = ThinkingCard::provisional();
        card.append(reasoning_id, &message.text);
        self.transcript.push(TranscriptEntry::Thinking(card));
    }

    pub(super) fn complete_reasoning(&mut self, message: RunTurnResponse) {
        let Some(reasoning_id) = message.reasoning_id else {
            self.push_system("Ignored reasoning completion without an ID");
            return;
        };
        let duration_ms = message
            .duration_ms
            .and_then(|value| value.parse::<u64>().ok());
        if let Some(card) = self
            .transcript
            .iter_mut()
            .rev()
            .find_map(|entry| match entry {
                TranscriptEntry::Thinking(card)
                    if card.reasoning_id.as_deref() == Some(reasoning_id.as_str()) =>
                {
                    Some(card)
                }
                _ => None,
            })
        {
            card.finish(duration_ms);
        }
    }

    pub(super) fn finish_provisional_thinking(&mut self) {
        let remove = matches!(
            self.transcript.last(),
            Some(TranscriptEntry::Thinking(card)) if card.is_running() && card.text.is_empty()
        );
        if remove {
            self.transcript.pop();
        }
    }

    pub(super) fn finish_active_thinking(&mut self) {
        self.finish_provisional_thinking();
        if let Some(TranscriptEntry::Thinking(card)) = self.transcript.last_mut()
            && card.is_running()
        {
            card.finish(None);
        }
    }

    pub(super) fn toggle_thinking_at(&mut self, position: ratatui::layout::Position) -> bool {
        let Some(target) = self
            .thinking_hit_targets
            .iter()
            .copied()
            .find(|target| target.area.contains(position))
        else {
            return false;
        };
        self.selected_entry = Some(target.entry_index);
        self.focus = Focus::Scrollback;
        if let Some(TranscriptEntry::Thinking(card)) = self.transcript.get_mut(target.entry_index) {
            card.expanded = !card.expanded;
        }
        true
    }

    pub(super) fn toggle_user_at(&mut self, position: ratatui::layout::Position) -> bool {
        let Some(target) = self
            .user_hit_targets
            .iter()
            .copied()
            .find(|target| target.area.contains(position))
        else {
            return false;
        };
        self.selected_entry = Some(target.entry_index);
        self.focus = Focus::Scrollback;
        if !self.expanded_user_entries.remove(&target.entry_index) {
            self.expanded_user_entries.insert(target.entry_index);
        }
        true
    }

    pub(super) fn start_tool(&mut self, message: RunTurnResponse) {
        self.finish_provisional_thinking();
        let Some(call_id) = message.tool_call_id else {
            self.push_system("Ignored a Tool event without a call ID");
            return;
        };
        let Some(name) = message.tool_name else {
            self.push_system("Ignored a Tool event without a name");
            return;
        };
        self.transcript
            .push(TranscriptEntry::Tool(ToolCard::running(
                call_id,
                name,
                message.arguments_json.map(|value| value.to_string()),
            )));
        self.selected_block = Some(ToolSelection::Tool(self.transcript.len() - 1));
    }

    pub(super) fn finish_tool(&mut self, message: RunTurnResponse, status: ToolStatus) {
        let Some(call_id) = message.tool_call_id else {
            self.push_system("Ignored a Tool result without a call ID");
            return;
        };
        let index = self.transcript.iter().rposition(
            |entry| matches!(entry, TranscriptEntry::Tool(card) if card.call_id == call_id),
        );
        let Some(index) = index else {
            self.push_system(format!(
                "Ignored a Tool result for unknown call `{call_id}`"
            ));
            return;
        };
        let TranscriptEntry::Tool(card) = &mut self.transcript[index] else {
            unreachable!("Tool lookup returned a message entry")
        };
        card.content = message.content;
        card.metadata_json = message.metadata_json.map(|value| value.to_string());
        card.duration_ms = message
            .duration_ms
            .and_then(|value| value.parse::<u64>().ok());
        card.error = message.error;
        card.status = status;
        self.selected_block = Some(ToolSelection::Tool(index));
    }

    pub(super) fn append_tool_progress(&mut self, message: RunTurnResponse) {
        let Some(call_id) = message.tool_call_id else {
            self.push_system("Ignored Tool progress without a call ID");
            return;
        };
        let Some(content) = message.content else {
            return;
        };
        let index = self.transcript.iter().rposition(
            |entry| matches!(entry, TranscriptEntry::Tool(card) if card.call_id == call_id),
        );
        let Some(index) = index else {
            self.push_system(format!(
                "Ignored Tool progress for unknown call `{call_id}`"
            ));
            return;
        };
        let TranscriptEntry::Tool(card) = &mut self.transcript[index] else {
            unreachable!("Tool lookup returned a message entry")
        };
        card.append_progress(&content);
        self.selected_block = Some(ToolSelection::Tool(index));
    }

    pub(super) fn toggle_tool_details(&mut self) {
        let Some(selection) = self.selected_block else {
            return;
        };
        match selection {
            ToolSelection::Tool(index) => {
                if let Some(TranscriptEntry::Tool(card)) = self.transcript.get_mut(index) {
                    card.expanded = !card.expanded;
                }
            }
            ToolSelection::Group { start, .. } => {
                if !self.expanded_groups.remove(&start) {
                    self.expanded_groups.insert(start);
                }
            }
        }
    }

    pub(super) fn set_tool_details(&mut self, expanded: bool) {
        let Some(selection) = self.selected_block else {
            return;
        };
        match selection {
            ToolSelection::Tool(index) => {
                if let Some(TranscriptEntry::Tool(card)) = self.transcript.get_mut(index) {
                    card.expanded = expanded;
                }
            }
            ToolSelection::Group { start, .. } if expanded => {
                self.expanded_groups.insert(start);
            }
            ToolSelection::Group { start, .. } => {
                self.expanded_groups.remove(&start);
            }
        }
    }

    pub(super) fn select_adjacent_tool(&mut self, previous: bool) {
        if self.visible_tool_blocks.is_empty() {
            self.selected_block = None;
            return;
        }
        let current = self.selected_block.and_then(|selected| {
            self.visible_tool_blocks
                .iter()
                .position(|item| *item == selected)
        });
        let next = if previous {
            current
                .and_then(|position| position.checked_sub(1))
                .unwrap_or(self.visible_tool_blocks.len() - 1)
        } else {
            current.map_or(0, |position| {
                (position + 1) % self.visible_tool_blocks.len()
            })
        };
        self.selected_block = Some(self.visible_tool_blocks[next]);
    }

    pub(super) fn select_adjacent_entry(&mut self, previous: bool) {
        if self.rendered_entry_rows.is_empty() {
            self.selected_entry = None;
            return;
        }
        let current = self.selected_entry.and_then(|selected| {
            self.rendered_entry_rows
                .iter()
                .position(|row| row.entry_index == selected)
        });
        let next = if previous {
            current
                .and_then(|position| position.checked_sub(1))
                .unwrap_or(self.rendered_entry_rows.len() - 1)
        } else {
            current.map_or(0, |position| {
                (position + 1) % self.rendered_entry_rows.len()
            })
        };
        let row = self.rendered_entry_rows[next];
        self.selected_entry = Some(row.entry_index);
        if let Some(selection) = self.tool_selection_for_entry(row.entry_index) {
            self.selected_block = Some(selection);
        }
        if row.start_row < self.scroll.top {
            self.scroll.top = row.start_row;
            self.scroll.follow_tail = false;
        } else {
            let viewport_end = self.scroll.top.saturating_add(self.scroll.viewport_rows);
            if row.end_row >= viewport_end {
                self.scroll.top = row
                    .end_row
                    .saturating_add(1)
                    .saturating_sub(self.scroll.viewport_rows)
                    .min(self.scroll.max_top);
                self.scroll.follow_tail = self.scroll.top == self.scroll.max_top;
            }
        }
        self.scroll.cancel_page_flip();
    }

    pub(super) fn toggle_selected_entry(&mut self) {
        let Some(index) = self.selected_entry else {
            return;
        };
        match self.transcript.get_mut(index) {
            Some(TranscriptEntry::User { .. }) => {
                if !self.expanded_user_entries.remove(&index) {
                    self.expanded_user_entries.insert(index);
                }
            }
            Some(TranscriptEntry::Thinking(card)) => card.expanded = !card.expanded,
            Some(TranscriptEntry::Tool(_)) => {
                self.selected_block = self
                    .tool_selection_for_entry(index)
                    .or(Some(ToolSelection::Tool(index)));
                self.toggle_tool_details();
            }
            _ => {}
        }
    }

    pub(super) fn tool_selection_for_entry(&self, entry_index: usize) -> Option<ToolSelection> {
        self.visible_tool_blocks.iter().copied().find(|selection| {
            matches!(
                selection,
                ToolSelection::Tool(index) if *index == entry_index
            ) || matches!(
                selection,
                ToolSelection::Group { start, .. } if *start == entry_index
            )
        })
    }

    pub(super) fn toggle_tool_at(&mut self, column: u16, row: u16) {
        let Some(target) = self
            .tool_hit_targets
            .iter()
            .find(|target| {
                column >= target.column_start
                    && column <= target.column_end
                    && row >= target.row_start
                    && row <= target.row_end
            })
            .copied()
        else {
            return;
        };
        self.selected_block = Some(target.selection);
        self.selected_entry = Some(match target.selection {
            ToolSelection::Tool(index) => index,
            ToolSelection::Group { start, .. } => start,
        });
        self.toggle_tool_details();
    }

    pub(super) fn active_tool_activity(&self) -> Option<String> {
        self.transcript.iter().rev().find_map(|entry| match entry {
            TranscriptEntry::Tool(card) if card.status == ToolStatus::Running => {
                Some(card.activity())
            }
            _ => None,
        })
    }
}
