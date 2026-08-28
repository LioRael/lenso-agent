//! Endpoint-free Web surface consumer Plugin.

use lenso::{Port, plugin};
use lenso_capability_agent as agent_capability;
use lenso_capability_agent_session as session_capability;

/// Statically linked binding anchor used only by the Web Host surface.
#[plugin(consumer)]
#[derive(Clone, Debug)]
struct AgentWebAnchor {
    agent: Port<agent_capability::AgentClient>,
    session: Port<session_capability::SessionClient>,
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
                    "capability_id": "lenso.agent@3",
                    "descriptor_version": "3.0.0",
                    "cardinality": "one"
                },
                {
                    "capability_id": "lenso.agent.session@1",
                    "descriptor_version": "1.3.0",
                    "cardinality": "one"
                }
            ])
        );
    }
}
