//! Portable Agent model-completion Capability.

#![allow(
    clippy::struct_excessive_bools,
    reason = "generated portable model feature flags are independent Provider facts"
)]

#[allow(dead_code)]
mod contract;

include!("generated.rs");

#[cfg(test)]
mod tests {
    #[test]
    fn pre_4_1_complete_payload_remains_valid_without_affinity_hint() {
        let original = serde_json::json!({
            "model": "fixture", "messages": [], "tools": [],
            "temperature": 0.0, "max_output_tokens": 100
        });
        let request: super::CompleteOpen = serde_json::from_value(original.clone()).unwrap();
        assert!(request.continuation_scope.is_none());
        assert_eq!(serde_json::to_value(request).unwrap(), original);
    }
}
