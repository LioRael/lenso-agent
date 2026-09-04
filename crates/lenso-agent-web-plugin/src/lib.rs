//! Endpoint-free Web surface consumer Plugin.

use lenso::{ManyPort, Port, plugin};
use lenso_capability_agent as agent_capability;
use lenso_capability_agent_context_source as context_source_capability;
use lenso_capability_agent_session as session_capability;
use lenso_capability_agent_task_supervisor as task_supervisor_capability;
use lenso_capability_agent_user_interaction as interaction_capability;

/// Statically linked binding anchor used only by the Web Host surface.
#[plugin(consumer)]
#[derive(Clone, Debug)]
struct AgentWebAnchor {
    auth_connections: ManyPort<lenso_capability_agent_auth_connection::AuthConnectionClient>,
    agent: Port<agent_capability::AgentClient>,
    context_sources: ManyPort<context_source_capability::ContextSourceClient>,
    interaction: Port<interaction_capability::UserInteractionClient>,
    session: Port<session_capability::SessionClient>,
    task_supervisors: ManyPort<task_supervisor_capability::TaskSupervisorClient>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consumer_descriptor_is_derived_from_the_typed_ports() {
        let descriptor: serde_json::Value = serde_json::from_str(PLUGIN_DESCRIPTOR_JSON).unwrap();
        assert_eq!(descriptor["provided_capabilities"], serde_json::json!([]));
        assert_eq!(
            descriptor["required_capabilities"],
            serde_json::json!([
                {
                    "capability_id": "lenso.agent.auth-connection@1",
                    "descriptor_version": "1.0.0",
                    "cardinality": "many"
                },
                {
                    "capability_id": "lenso.agent@3",
                    "descriptor_version": "3.0.0",
                    "cardinality": "one"
                },
                {
                    "capability_id": "lenso.agent.context-source@1",
                    "descriptor_version": "1.1.0",
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
