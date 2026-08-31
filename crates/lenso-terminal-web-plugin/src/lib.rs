//! Generic Web terminal surface consumer Plugin identity.

use lenso::{Port, plugin};
use lenso_capability_terminal_command as command_capability;

#[plugin(consumer)]
#[derive(Clone, Debug)]
struct TerminalWebSurface {
    commands: Port<command_capability::CommandClient>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_requires_only_the_terminal_command_role() {
        let descriptor: serde_json::Value = serde_json::from_str(PLUGIN_DESCRIPTOR_JSON).unwrap();
        assert_eq!(descriptor["provided_capabilities"], serde_json::json!([]));
        assert_eq!(
            descriptor["required_capabilities"],
            serde_json::json!([{
                "capability_id": "lenso.terminal.command@1",
                "descriptor_version": "1.0.0",
                "cardinality": "one"
            }])
        );
    }
}
