//! Validated Agent Tool catalog and execution Capability.

include!("generated.rs");

/// Versioned Tool-result metadata fact indicating that execution completed
/// only after receiving an answer from the user.
pub const USER_INTERACTION_COMPLETED_METADATA_KEY: &str =
    "lenso.agent.user-interaction-completed@1";

/// Returns whether Tool-result metadata records a completed user interaction.
#[must_use]
pub fn metadata_completes_user_interaction(metadata_json: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(metadata_json)
        .ok()
        .and_then(|metadata| {
            metadata
                .get(USER_INTERACTION_COMPLETED_METADATA_KEY)
                .and_then(serde_json::Value::as_bool)
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::{USER_INTERACTION_COMPLETED_METADATA_KEY, metadata_completes_user_interaction};

    #[test]
    fn only_the_versioned_true_fact_completes_user_interaction() {
        let completed = serde_json::json!({
            (USER_INTERACTION_COMPLETED_METADATA_KEY): true,
            "interaction_id": "ask-1"
        })
        .to_string();
        assert!(metadata_completes_user_interaction(&completed));
        assert!(!metadata_completes_user_interaction(
            r#"{"lenso.agent.user-interaction-completed@1":false}"#
        ));
        assert!(!metadata_completes_user_interaction("{}"));
        assert!(!metadata_completes_user_interaction("[]"));
        assert!(!metadata_completes_user_interaction("invalid"));
    }
}
