use std::fmt::Write;

use sha2::{Digest, Sha256};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{Trajectory, TrajectoryRecord, TrajectoryStatus};

/// Projects one durable Trajectory into the OTLP/HTTP JSON trace envelope.
pub fn project_otlp_trace(
    trajectory: &Trajectory,
    service_name: &str,
) -> Result<serde_json::Value, String> {
    if service_name.trim().is_empty() || service_name.len() > 256 {
        return Err("OTLP service name is invalid".to_owned());
    }
    let trace_id = hex_prefix(
        Sha256::digest(format!("lenso/session/{}", trajectory.session_id)),
        16,
    );
    let root_span_id = hex_prefix(
        Sha256::digest(format!("lenso/session/{}/root", trajectory.session_id)),
        8,
    );
    let start = trajectory
        .summary
        .started_at
        .as_deref()
        .or_else(|| {
            trajectory
                .records
                .first()
                .map(|record| record.started_at.as_str())
        })
        .ok_or_else(|| "Trajectory has no OTLP start time".to_owned())?;
    let end = trajectory
        .summary
        .updated_at
        .as_deref()
        .or_else(|| trajectory.records.last().map(record_end))
        .ok_or_else(|| "Trajectory has no OTLP end time".to_owned())?;
    let mut spans = vec![serde_json::json!({
        "traceId": trace_id,
        "spanId": root_span_id,
        "name": "lenso.agent.session",
        "kind": 1,
        "startTimeUnixNano": unix_nanos(start)?,
        "endTimeUnixNano": unix_nanos(end)?,
        "attributes": [
            attribute("lenso.session.id", &trajectory.session_id),
            attribute("lenso.trajectory.schema", &trajectory.schema),
            attribute("lenso.trajectory.revision", &trajectory.revision.to_string()),
        ],
        "status": otlp_status(trajectory.summary.status),
    })];
    for record in &trajectory.records {
        let span_id = hex_prefix(
            Sha256::digest(format!(
                "lenso/session/{}/record/{}",
                trajectory.session_id, record.id
            )),
            8,
        );
        let mut attributes = vec![
            attribute("lenso.record.id", &record.id),
            attribute(
                "lenso.record.kind",
                &format!("{:?}", record.kind).to_lowercase(),
            ),
            attribute("lenso.turn.number", &record.turn.to_string()),
        ];
        if let Some(model) = &record.detail.model {
            attributes.push(attribute("gen_ai.request.model", model));
        }
        if let Some(tool) = &record.detail.tool_name {
            attributes.push(attribute("gen_ai.tool.name", tool));
        }
        if let Some(call_id) = &record.detail.tool_call_id {
            attributes.push(attribute("gen_ai.tool.call.id", call_id));
        }
        spans.push(serde_json::json!({
            "traceId": trace_id,
            "spanId": span_id,
            "parentSpanId": root_span_id,
            "name": format!("lenso.agent.{}", format!("{:?}", record.kind).to_lowercase()),
            "kind": 1,
            "startTimeUnixNano": unix_nanos(&record.started_at)?,
            "endTimeUnixNano": unix_nanos(record_end(record))?,
            "attributes": attributes,
            "status": otlp_status(record.status),
        }));
    }
    Ok(serde_json::json!({
        "resourceSpans": [{
            "resource": {
                "attributes": [
                    attribute("service.name", service_name),
                    attribute("lenso.session.id", &trajectory.session_id),
                ]
            },
            "scopeSpans": [{
                "scope": { "name": "lenso-agent-session-inspection", "version": "0.1.0" },
                "spans": spans,
            }]
        }]
    }))
}

fn record_end(record: &TrajectoryRecord) -> &str {
    record.completed_at.as_deref().unwrap_or(&record.started_at)
}

fn unix_nanos(value: &str) -> Result<String, String> {
    let timestamp = OffsetDateTime::parse(value, &Rfc3339)
        .map_err(|error| format!("Trajectory timestamp is invalid: {error}"))?;
    u128::try_from(timestamp.unix_timestamp_nanos())
        .map(|value| value.to_string())
        .map_err(|_| "Trajectory timestamp precedes the Unix epoch".to_owned())
}

fn hex_prefix(bytes: impl AsRef<[u8]>, length: usize) -> String {
    bytes
        .as_ref()
        .iter()
        .take(length)
        .fold(String::with_capacity(length * 2), |mut hex, byte| {
            write!(hex, "{byte:02x}").expect("writing to a String cannot fail");
            hex
        })
}

fn attribute(key: &str, value: &str) -> serde_json::Value {
    serde_json::json!({ "key": key, "value": { "stringValue": value } })
}

fn otlp_status(status: TrajectoryStatus) -> serde_json::Value {
    match status {
        TrajectoryStatus::Completed | TrajectoryStatus::Idle => serde_json::json!({ "code": 1 }),
        TrajectoryStatus::Running => serde_json::json!({ "code": 0 }),
        TrajectoryStatus::Failed | TrajectoryStatus::Cancelled => {
            serde_json::json!({ "code": 2, "message": format!("{status:?}").to_lowercase() })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TrajectoryDetail, TrajectoryKind, TrajectoryRecord, TrajectorySummary};

    #[test]
    fn otlp_projection_is_deterministic_and_uses_valid_identifier_lengths() {
        let trajectory = Trajectory {
            schema: Trajectory::SCHEMA.to_owned(),
            session_id: "session-1".to_owned(),
            revision: 1,
            summary: TrajectorySummary {
                status: TrajectoryStatus::Completed,
                turns: 1,
                model_calls: 0,
                tool_calls: 0,
                failed_operations: 0,
                input_tokens: 0,
                output_tokens: 0,
                started_at: Some("2026-08-30T00:00:00Z".to_owned()),
                updated_at: Some("2026-08-30T00:00:01Z".to_owned()),
                duration_ms: Some(1000),
            },
            records: vec![TrajectoryRecord {
                id: "user-1".to_owned(),
                turn: 1,
                kind: TrajectoryKind::User,
                status: TrajectoryStatus::Completed,
                label: "User".to_owned(),
                preview: String::new(),
                started_at: "2026-08-30T00:00:00Z".to_owned(),
                completed_at: Some("2026-08-30T00:00:00Z".to_owned()),
                duration_ms: Some(0),
                time_to_first_token_ms: None,
                step: None,
                input_tokens: None,
                output_tokens: None,
                detail: TrajectoryDetail::default(),
                source_event_ids: vec!["event-1".to_owned()],
            }],
        };
        let first = project_otlp_trace(&trajectory, "lenso-agent").unwrap();
        let second = project_otlp_trace(&trajectory, "lenso-agent").unwrap();
        assert_eq!(first, second);
        let spans = first["resourceSpans"][0]["scopeSpans"][0]["spans"]
            .as_array()
            .unwrap();
        assert_eq!(spans[0]["traceId"].as_str().unwrap().len(), 32);
        assert_eq!(spans[0]["spanId"].as_str().unwrap().len(), 16);
    }
}
