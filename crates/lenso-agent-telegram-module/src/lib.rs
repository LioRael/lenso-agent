//! Endpoint-free Telegram surface consumer Module.

use lenso::{Port, module};
use lenso_capability_agent as agent_capability;

/// Statically linked binding anchor used only by the Telegram Host surface.
#[module(consumer)]
#[derive(Clone, Debug)]
struct TelegramSurfaceAnchor {
    agent: Port<agent_capability::AgentClient>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consumer_descriptor_is_derived_from_the_typed_agent_port() {
        let descriptor: serde_json::Value = serde_json::from_str(MODULE_DESCRIPTOR_JSON).unwrap();
        assert_eq!(descriptor["provided_capabilities"], serde_json::json!([]));
        assert_eq!(
            descriptor["required_capabilities"],
            serde_json::json!([{
                "capability_id": "lenso.agent@3",
                "descriptor_version": "3.0.0",
                "cardinality": "one"
            }])
        );
    }
}
