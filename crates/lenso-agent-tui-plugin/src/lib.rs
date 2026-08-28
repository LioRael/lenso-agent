//! Endpoint-free TUI Shell consumer Plugin.

use lenso::{ManyPort, Port, plugin};
use lenso_capability_agent as agent_capability;
use lenso_capability_agent_tui_contribution as tui_capability;
use lenso_capability_agent_tui_suggestion as suggestion_capability;

/// Statically linked binding anchor used only by the TUI Host.
#[plugin(consumer)]
#[derive(Clone, Debug)]
struct AgentTuiShell {
    agent: Port<agent_capability::AgentClient>,
    contributions: ManyPort<tui_capability::TuiContributionClient>,
    suggestions: ManyPort<suggestion_capability::TuiSuggestionClient>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consumer_descriptor_is_derived_from_typed_terminal_ports() {
        let descriptor: serde_json::Value = serde_json::from_str(PLUGIN_DESCRIPTOR_JSON).unwrap();
        assert_eq!(descriptor["provided_capabilities"], serde_json::json!([]));
        assert_eq!(
            descriptor["required_capabilities"],
            serde_json::json!([
                {
                    "capability_id": "lenso.agent@3",
                    "descriptor_version": "3.0.0",
                    "cardinality": "one"
                },
                {
                    "capability_id": "lenso.agent.tui-contribution@1",
                    "descriptor_version": "1.0.0",
                    "cardinality": "many"
                },
                {
                    "capability_id": "lenso.agent.tui-suggestion@1",
                    "descriptor_version": "1.0.0",
                    "cardinality": "many"
                }
            ])
        );
    }
}
