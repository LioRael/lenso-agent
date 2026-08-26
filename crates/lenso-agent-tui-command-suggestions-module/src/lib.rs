//! Static command suggestion Module for the Agent TUI.

use futures::future::ready;
use lenso_capability_agent_tui_suggestion::{
    self as suggestion_contract, SnapshotRequest, SnapshotResponse, Suggestion, SuggestionKind,
    TuiSuggestionProvider, validate_snapshot_suggestions,
};
use lenso_kernel::{InvocationContext, RuntimeFailure};

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandSuggestionsConfig {
    commands: Vec<CommandConfig>,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandConfig {
    id: String,
    label: String,
    insert_text: String,
    description: String,
}

fn suggestions(config: &CommandSuggestionsConfig) -> Result<Vec<Suggestion>, RuntimeFailure> {
    if config.commands.len() > 64 {
        return Err(RuntimeFailure::InvalidResolvedPlan {
            detail: "command suggestions exceed the 64-command limit".to_owned(),
        });
    }
    let suggestions = config
        .commands
        .iter()
        .map(|command| Suggestion {
            id: command.id.clone(),
            kind: SuggestionKind::Command,
            label: command.label.clone(),
            insert_text: command.insert_text.clone(),
            description: command.description.clone(),
        })
        .collect::<Vec<_>>();
    validate_snapshot_suggestions(&suggestions)
        .map_err(|detail| RuntimeFailure::InvalidResolvedPlan { detail })?;
    Ok(suggestions)
}

fn validate_config(config: &CommandSuggestionsConfig) -> Result<(), RuntimeFailure> {
    suggestions(config).map(|_| ())
}

#[lenso::module(configuration_schema = "config.schema.json", validate = validate_config)]
#[derive(Clone, Debug)]
struct CommandSuggestions {
    #[config]
    config: CommandSuggestionsConfig,
}

#[lenso::provides(suggestion_contract::TuiSuggestion)]
impl TuiSuggestionProvider for CommandSuggestions {
    fn snapshot(
        &self,
        _context: InvocationContext,
        _request: SnapshotRequest,
    ) -> lenso_kernel::NativeRequestFuture<suggestion_contract::TuiSuggestion> {
        Box::pin(ready(
            suggestions(&self.config).map(|suggestions| Ok(SnapshotResponse { suggestions })),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_duplicate_command_ids() {
        let command = CommandConfig {
            id: "agent.command.help".to_owned(),
            label: "/help".to_owned(),
            insert_text: "/help".to_owned(),
            description: "Show help".to_owned(),
        };
        let error = suggestions(&CommandSuggestionsConfig {
            commands: vec![command.clone(), command],
        })
        .unwrap_err();
        assert!(matches!(error, RuntimeFailure::InvalidResolvedPlan { .. }));
    }
}
