//! Bounded extractive Context Compaction Adapter.

use lenso::prelude::*;
use lenso_capability_agent_context_compaction::{
    self as context_contract, CompactError, CompactRequest, CompactResponse, ContextMessage,
    ContextMessageRole,
};
use lenso_kernel::RuntimeFailure;

#[derive(Clone, Debug, serde::Deserialize, lenso::PluginConfig)]
#[serde(deny_unknown_fields)]
struct CompactionConfig {
    max_input_characters: usize,
    max_summary_characters: usize,
    retain_recent_turns: usize,
}

#[lenso::plugin(validate = validate_config)]
#[derive(Clone, Debug)]
struct ContextCompactionPlugin {
    #[config]
    config: CompactionConfig,
}

fn validate_config(config: &CompactionConfig) -> Result<(), RuntimeFailure> {
    if !(1_024..=16_777_216).contains(&config.max_input_characters)
        || !(256..=262_144).contains(&config.max_summary_characters)
        || !(1..=64).contains(&config.retain_recent_turns)
    {
        return Err(RuntimeFailure::InvalidResolvedPlan {
            detail: "Context Compaction limits are invalid".to_owned(),
        });
    }
    Ok(())
}

#[lenso::provides(context_contract::ContextCompaction)]
impl ContextCompactionPlugin {
    #[allow(clippy::unused_async_trait_impl)]
    async fn compact(
        &self,
        _: Ctx,
        request: CompactRequest,
    ) -> PluginResult<CompactResponse, CompactError> {
        compact_context(&self.config, request).map_err(PluginError::domain)
    }
}

fn compact_context(
    config: &CompactionConfig,
    request: CompactRequest,
) -> Result<CompactResponse, CompactError> {
    if request.messages.is_empty()
        || request.session_id.trim().is_empty()
        || !valid_turn_messages(&request.messages)
    {
        return Err(CompactError::InvalidContext);
    }
    let previous_summary = request.previous_summary.flatten();
    let input_characters = previous_summary
        .as_deref()
        .map_or(0, |value| value.chars().count())
        .saturating_add(
            request
                .messages
                .iter()
                .map(|message| message.content.chars().count())
                .sum::<usize>(),
        );
    if input_characters > config.max_input_characters {
        return Err(CompactError::ContextTooLarge);
    }

    let configured_retained_count = config
        .retain_recent_turns
        .saturating_mul(2)
        .min(request.messages.len());
    let retained_count = if configured_retained_count == request.messages.len() {
        0
    } else {
        configured_retained_count
    };
    let compacted_count = request.messages.len().saturating_sub(retained_count);
    let (compacted, retained) = request.messages.split_at(compacted_count);
    let target = usize::try_from(request.target_summary_characters)
        .unwrap_or(usize::MAX)
        .min(config.max_summary_characters);
    let summary = extractive_summary(previous_summary.as_deref(), compacted, target);
    Ok(CompactResponse {
        summary,
        retained_messages: retained.to_vec(),
    })
}

fn valid_turn_messages(messages: &[ContextMessage]) -> bool {
    messages.len().is_multiple_of(2)
        && messages.iter().enumerate().all(|(index, message)| {
            !message.content.trim().is_empty()
                && matches!(
                    (index % 2, &message.role),
                    (0, ContextMessageRole::User) | (1, ContextMessageRole::Assistant)
                )
        })
}

fn extractive_summary(
    previous_summary: Option<&str>,
    compacted: &[ContextMessage],
    target: usize,
) -> String {
    let mut summary = String::from("Earlier completed conversation (extractive):\n");
    if let Some(previous) = previous_summary.filter(|value| !value.trim().is_empty()) {
        let previous_budget = target
            .saturating_sub(summary.chars().count())
            .min(target / 2);
        let previous = truncate_chars(previous.trim(), previous_budget);
        if !previous.is_empty() {
            summary.push_str("Previous summary:\n");
            summary.push_str(&previous);
        }
    }
    let (pairs, remainder) = compacted.as_chunks::<2>();
    debug_assert!(remainder.is_empty());
    let candidates = pairs.iter().map(|pair| {
        format!(
            "User: {}\nAssistant: {}",
            normalized_excerpt(&pair[0].content),
            normalized_excerpt(&pair[1].content)
        )
    });
    if compacted.is_empty() && previous_summary.is_none() {
        return "No earlier completed turns were omitted.".to_owned();
    }

    for candidate in candidates.rev() {
        let separator = if summary.ends_with('\n') { "" } else { "\n\n" };
        let candidate_size = separator.chars().count() + candidate.chars().count();
        if summary.chars().count().saturating_add(candidate_size) > target {
            continue;
        }
        summary.push_str(separator);
        summary.push_str(&candidate);
    }
    if summary.trim_end() == "Earlier completed conversation (extractive):" {
        return truncate_chars(
            "Earlier completed conversation was omitted to fit the context window.",
            target,
        );
    }
    truncate_chars(&summary, target)
}

fn normalized_excerpt(value: &str) -> String {
    const MAX_EXCERPT_CHARACTERS: usize = 1_024;
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_chars(&normalized, MAX_EXCERPT_CHARACTERS)
}

fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        value.to_owned()
    } else {
        value.chars().take(limit).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(role: ContextMessageRole, content: &str) -> ContextMessage {
        ContextMessage {
            role,
            content: content.to_owned(),
        }
    }

    #[test]
    fn compaction_preserves_complete_recent_turns_and_bounds_the_summary() {
        let response = compact_context(
            &CompactionConfig {
                max_input_characters: 16_384,
                max_summary_characters: 512,
                retain_recent_turns: 1,
            },
            CompactRequest {
                session_id: "session-1".to_owned(),
                previous_summary: Some(Some("The user chose SQLite.".to_owned())),
                messages: vec![
                    message(ContextMessageRole::User, "Design storage."),
                    message(ContextMessageRole::Assistant, "Use an append-only log."),
                    message(ContextMessageRole::User, "What is next?"),
                    message(ContextMessageRole::Assistant, "Add compaction."),
                ],
                target_summary_characters: 512,
            },
        )
        .unwrap();

        assert!(response.summary.chars().count() <= 512);
        assert!(response.summary.contains("SQLite"));
        assert_eq!(response.retained_messages.len(), 2);
        assert_eq!(response.retained_messages[0].content, "What is next?");
    }

    #[test]
    fn compaction_rejects_incomplete_turn_pairs() {
        let result = compact_context(
            &CompactionConfig {
                max_input_characters: 16_384,
                max_summary_characters: 512,
                retain_recent_turns: 1,
            },
            CompactRequest {
                session_id: "session-1".to_owned(),
                previous_summary: Some(None),
                messages: vec![message(ContextMessageRole::Assistant, "orphan")],
                target_summary_characters: 512,
            },
        );
        assert!(matches!(result, Err(CompactError::InvalidContext)));
    }
}
