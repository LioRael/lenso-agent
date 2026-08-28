//! Backend-neutral inspection of durable Session facts.

use std::collections::BTreeSet;

/// One normalized event read from a durable Session Adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InspectedSessionEvent {
    pub revision: u64,
    pub event_id: String,
    pub kind: String,
    pub turn_id: Option<String>,
    pub occurred_at: String,
    pub payload_json: String,
}

/// One normalized durable Session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InspectedSession {
    pub session_id: String,
    pub revision: u64,
    pub events: Vec<InspectedSessionEvent>,
}

/// Storage-neutral offline inspection seam implemented by each Session Adapter.
pub trait SessionInspector {
    fn inspect_one(&self, session_id: &str) -> Result<InspectedSession, String>;
    fn inspect_all(&self) -> Result<Vec<InspectedSession>, String>;
}

/// One validated `turn_started` fact projected for Generation provenance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InspectedTurnStarted {
    pub session_id: String,
    pub revision: u64,
    pub turn_id: Option<String>,
    pub payload_json: String,
}

/// Validate complete Session snapshots and project their Turn provenance facts.
pub fn inspect_turn_started(
    inspector: &dyn SessionInspector,
    session_id: Option<&str>,
) -> Result<Vec<InspectedTurnStarted>, String> {
    let sessions = match session_id {
        Some(session_id) => vec![inspector.inspect_one(session_id)?],
        None => inspector.inspect_all()?,
    };
    let mut projected = Vec::new();
    for session in sessions {
        validate_session(&session)?;
        projected.extend(
            session
                .events
                .into_iter()
                .filter(|event| event.kind == "turn_started")
                .map(|event| InspectedTurnStarted {
                    session_id: session.session_id.clone(),
                    revision: event.revision,
                    turn_id: event.turn_id,
                    payload_json: event.payload_json,
                }),
        );
    }
    projected.sort_by(|left, right| {
        left.session_id
            .cmp(&right.session_id)
            .then(left.revision.cmp(&right.revision))
    });
    Ok(projected)
}

pub fn validate_session(session: &InspectedSession) -> Result<(), String> {
    if !valid_session_id(&session.session_id)
        || session.revision != u64::try_from(session.events.len()).unwrap_or(u64::MAX)
    {
        return Err("Session identity or revision is invalid".to_owned());
    }
    let mut event_ids = BTreeSet::new();
    for (offset, event) in session.events.iter().enumerate() {
        let expected = u64::try_from(offset)
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| "Session revision space overflowed".to_owned())?;
        if event.revision != expected
            || event.event_id.is_empty()
            || event.event_id.len() > 128
            || !event_ids.insert(event.event_id.as_str())
            || !known_event_kind(&event.kind)
            || event.occurred_at.is_empty()
            || serde_json::from_str::<serde_json::Value>(&event.payload_json).is_err()
        {
            return Err("Session event sequence or payload is invalid".to_owned());
        }
    }
    Ok(())
}

pub fn valid_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id.len() <= 128
        && session_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn known_event_kind(kind: &str) -> bool {
    matches!(
        kind,
        "session_created"
            | "system_instruction_installed"
            | "context_compaction_started"
            | "context_compaction_committed"
            | "context_compaction_failed"
            | "memory_recalled"
            | "memory_recall_failed"
            | "memory_committed"
            | "memory_commit_failed"
            | "turn_started"
            | "model_requested"
            | "model_output"
            | "tool_requested"
            | "tool_result"
            | "turn_completed"
            | "turn_failed"
            | "turn_cancelled"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeInspector(InspectedSession);

    impl SessionInspector for FakeInspector {
        fn inspect_one(&self, _: &str) -> Result<InspectedSession, String> {
            Ok(self.0.clone())
        }

        fn inspect_all(&self) -> Result<Vec<InspectedSession>, String> {
            Ok(vec![self.0.clone()])
        }
    }

    fn session() -> InspectedSession {
        InspectedSession {
            session_id: "session-1".to_owned(),
            revision: 1,
            events: vec![InspectedSessionEvent {
                revision: 1,
                event_id: "event-1".to_owned(),
                kind: "turn_started".to_owned(),
                turn_id: Some("turn-1".to_owned()),
                occurred_at: "2026-08-28T00:00:00Z".to_owned(),
                payload_json: "{}".to_owned(),
            }],
        }
    }

    #[test]
    fn projection_uses_only_the_backend_neutral_interface() {
        let projected = inspect_turn_started(&FakeInspector(session()), None).unwrap();
        assert_eq!(projected.len(), 1);
        assert_eq!(projected[0].session_id, "session-1");
        assert_eq!(projected[0].revision, 1);
    }

    #[test]
    fn projection_rejects_incomplete_or_corrupt_snapshots() {
        let mut invalid = session();
        invalid.revision = 2;
        assert!(inspect_turn_started(&FakeInspector(invalid), None).is_err());

        let mut invalid = session();
        invalid.events[0].payload_json = "not-json".to_owned();
        assert!(inspect_turn_started(&FakeInspector(invalid), None).is_err());
    }
}
