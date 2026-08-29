//! Endpoint-free TUI Shell consumer Plugin.

use lenso::{ManyPort, Port, plugin};
use lenso_capability_agent as agent_capability;
use lenso_capability_agent_context_source as context_source_capability;
use lenso_capability_agent_session as session_capability;
use lenso_capability_agent_task_supervisor as task_supervisor_capability;
use lenso_capability_agent_tui_contribution as tui_capability;
use lenso_capability_agent_tui_suggestion as suggestion_capability;
use lenso_capability_agent_user_interaction as interaction_capability;

/// Statically linked binding anchor used only by the TUI Host.
#[plugin(consumer)]
#[derive(Clone, Debug)]
struct AgentTuiShell {
    agent: Port<agent_capability::AgentClient>,
    context_sources: ManyPort<context_source_capability::ContextSourceClient>,
    contributions: ManyPort<tui_capability::TuiContributionClient>,
    suggestions: ManyPort<suggestion_capability::TuiSuggestionClient>,
    interaction: Port<interaction_capability::UserInteractionClient>,
    session: Port<session_capability::SessionClient>,
    task_supervisors: ManyPort<task_supervisor_capability::TaskSupervisorClient>,
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
                    "capability_id": "lenso.agent.context-source@1",
                    "descriptor_version": "1.0.0",
                    "cardinality": "many"
                },
                {
                    "capability_id": "lenso.agent.tui-contribution@1",
                    "descriptor_version": "1.0.0",
                    "cardinality": "many"
                },
                {
                    "capability_id": "lenso.agent.tui-suggestion@1",
                    "descriptor_version": "1.2.0",
                    "cardinality": "many"
                },
                {
                    "capability_id": "lenso.agent.user-interaction@2",
                    "descriptor_version": "2.0.0",
                    "cardinality": "one"
                },
                {
                    "capability_id": "lenso.agent.session@1",
                    "descriptor_version": "1.6.0",
                    "cardinality": "one"
                },
                {
                    "capability_id": "lenso.agent.task-supervisor@2",
                    "descriptor_version": "2.0.0",
                    "cardinality": "many"
                }
            ])
        );
    }
}
