//! Endpoint-free CLI consumer Plugin.

use lenso::{ManyPort, Port, plugin};
use lenso_capability_agent as agent_capability;
use lenso_capability_agent_context_source as context_source_capability;

/// Statically linked binding anchor used only by the CLI Host.
#[plugin(consumer)]
#[derive(Clone, Debug)]
struct AgentCliAnchor {
    agent: Port<agent_capability::AgentClient>,
    context_sources: ManyPort<context_source_capability::ContextSourceClient>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consumer_descriptor_is_derived_from_the_typed_agent_port() {
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
                    "descriptor_version": "1.1.0",
                    "cardinality": "many"
                }
            ])
        );
    }
}
