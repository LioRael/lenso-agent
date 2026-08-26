//! Portable Agent turn Capability.

include!("generated.rs");

impl RunTurnResponse {
    /// Returns true for both explicit text deltas and messages from pre-1.2 providers.
    pub fn is_text_delta(&self) -> bool {
        self.kind
            .as_ref()
            .is_none_or(|kind| *kind == RunTurnResponseKind::TextDelta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pre_1_2_text_messages_remain_decodable() {
        let message =
            decode_run_turn_response(r#"{"sequence":"1","session_id":"session-1","text":"hello"}"#)
                .unwrap();

        assert!(message.is_text_delta());
        assert_eq!(message.kind, None);
        assert_eq!(message.text, "hello");
    }

    #[test]
    fn semantic_tool_messages_round_trip() {
        let message = RunTurnResponse {
            arguments_json: Some(r#"{"path":"src/lib.rs"}"#.to_owned().try_into().unwrap()),
            content: None,
            duration_ms: None,
            error: None,
            kind: Some(RunTurnResponseKind::ToolStarted),
            metadata_json: None,
            sequence: "2".to_owned(),
            session_id: Some("session-1".to_owned()),
            text: String::new(),
            tool_call_id: Some("call-1".to_owned()),
            tool_name: Some("read_text".to_owned()),
        };

        let wire = encode_run_turn_response(&message).unwrap();
        assert_eq!(decode_run_turn_response(&wire).unwrap(), message);
        assert!(!message.is_text_delta());
    }
}
