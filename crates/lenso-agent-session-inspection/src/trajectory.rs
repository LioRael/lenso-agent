use std::collections::BTreeMap;

use serde_json::{Map, Value};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{InspectedSession, InspectedSessionEvent, validate_session};

/// Versioned semantic projection of one durable Agent Session.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Trajectory {
    pub schema: String,
    pub session_id: String,
    pub revision: u64,
    pub summary: TrajectorySummary,
    pub records: Vec<TrajectoryRecord>,
}

impl Trajectory {
    pub const SCHEMA: &'static str = "lenso.agent.trajectory@1";
}

/// Aggregate facts used by the Trajectory toolbar and Turn groups.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TrajectorySummary {
    pub status: TrajectoryStatus,
    pub turns: u32,
    pub model_calls: u32,
    pub tool_calls: u32,
    pub failed_operations: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// Lifecycle state derived from durable events, never inferred by the Console.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrajectoryStatus {
    Idle,
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// Semantic record category rendered by a Trajectory client.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrajectoryKind {
    System,
    User,
    Model,
    Tool,
    Memory,
    Compaction,
}

/// One lifecycle record. Request/result event pairs are deliberately merged.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TrajectoryRecord {
    pub id: String,
    pub turn: u32,
    pub kind: TrajectoryKind,
    pub status: TrajectoryStatus,
    pub label: String,
    pub preview: String,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_to_first_token_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    pub detail: TrajectoryDetail,
    pub source_event_ids: Vec<String>,
}

/// Inspector content for one semantic Trajectory record.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TrajectoryDetail {
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_json: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_instruction_digest: Option<String>,
}

/// Project durable Session facts into a stable record ledger for Web clients.
pub fn project_trajectory(session: &InspectedSession) -> Result<Trajectory, String> {
    validate_session(session)?;
    let mut projection = Projection::new(session);
    for event in &session.events {
        projection.apply(event)?;
    }
    Ok(projection.finish(session))
}

struct Projection {
    records: Vec<TrajectoryRecord>,
    turns: BTreeMap<String, u32>,
    model_records: BTreeMap<(String, u32), usize>,
    latest_model: BTreeMap<String, usize>,
    tool_records: BTreeMap<String, usize>,
    compaction_records: BTreeMap<String, usize>,
    status: TrajectoryStatus,
    model_calls: u32,
    tool_calls: u32,
    failed_operations: u32,
    input_tokens: u64,
    output_tokens: u64,
}

impl Projection {
    fn new(session: &InspectedSession) -> Self {
        Self {
            records: Vec::with_capacity(session.events.len()),
            turns: BTreeMap::new(),
            model_records: BTreeMap::new(),
            latest_model: BTreeMap::new(),
            tool_records: BTreeMap::new(),
            compaction_records: BTreeMap::new(),
            status: TrajectoryStatus::Idle,
            model_calls: 0,
            tool_calls: 0,
            failed_operations: 0,
            input_tokens: 0,
            output_tokens: 0,
        }
    }

    fn apply(&mut self, event: &InspectedSessionEvent) -> Result<(), String> {
        let payload = payload(event)?;
        let turn = self.turn_number(event.turn_id.as_deref());
        match event.kind.as_str() {
            "session_created" => {}
            "system_instruction_installed" => self.system(event, &payload),
            "turn_started" => self.turn_started(event, &payload, turn),
            "model_requested" => self.model_requested(event, &payload, turn),
            "model_output" => self.model_output(event, &payload, turn),
            "tool_requested" => self.tool_requested(event, &payload, turn),
            "tool_result" => self.tool_result(event, &payload, turn),
            "memory_recalled"
            | "memory_recall_failed"
            | "memory_committed"
            | "memory_commit_failed" => self.memory(event, &payload, turn),
            "context_compaction_started"
            | "context_compaction_committed"
            | "context_compaction_failed" => self.compaction(event, &payload),
            "turn_completed" => self.status = TrajectoryStatus::Completed,
            "turn_failed" => {
                self.status = TrajectoryStatus::Failed;
                self.failed_operations = self.failed_operations.saturating_add(1);
                self.close_open_records(event, &payload, turn, TrajectoryStatus::Failed);
                self.failure(event, &payload, turn);
            }
            "turn_cancelled" => {
                self.status = TrajectoryStatus::Cancelled;
                self.close_open_records(event, &payload, turn, TrajectoryStatus::Cancelled);
                self.failure(event, &payload, turn);
            }
            _ => return Err(format!("unsupported Session event `{}`", event.kind)),
        }
        Ok(())
    }

    fn turn_number(&mut self, turn_id: Option<&str>) -> u32 {
        let Some(turn_id) = turn_id else {
            return 0;
        };
        if let Some(turn) = self.turns.get(turn_id) {
            return *turn;
        }
        let turn = u32::try_from(self.turns.len())
            .unwrap_or(u32::MAX)
            .saturating_add(1);
        self.turns.insert(turn_id.to_owned(), turn);
        turn
    }

    fn system(&mut self, event: &InspectedSessionEvent, payload: &Map<String, Value>) {
        let input = string(payload, "content");
        self.records.push(TrajectoryRecord {
            id: event.event_id.clone(),
            turn: 0,
            kind: TrajectoryKind::System,
            status: TrajectoryStatus::Completed,
            label: "System instruction".to_owned(),
            preview: count_label(array_len(payload, "contributions"), "prompt contribution"),
            started_at: event.occurred_at.clone(),
            completed_at: Some(event.occurred_at.clone()),
            duration_ms: None,
            time_to_first_token_ms: None,
            step: None,
            input_tokens: None,
            output_tokens: None,
            detail: TrajectoryDetail {
                summary: "Resolved system instruction installed for this Session.".to_owned(),
                input,
                system_instruction_digest: string(payload, "digest"),
                ..TrajectoryDetail::default()
            },
            source_event_ids: vec![event.event_id.clone()],
        });
    }

    fn turn_started(
        &mut self,
        event: &InspectedSessionEvent,
        payload: &Map<String, Value>,
        turn: u32,
    ) {
        self.status = TrajectoryStatus::Running;
        let input = string(payload, "input").unwrap_or_default();
        self.records.push(TrajectoryRecord {
            id: event.event_id.clone(),
            turn,
            kind: TrajectoryKind::User,
            status: TrajectoryStatus::Completed,
            label: "User message".to_owned(),
            preview: preview(&input),
            started_at: event.occurred_at.clone(),
            completed_at: Some(event.occurred_at.clone()),
            duration_ms: None,
            time_to_first_token_ms: None,
            step: None,
            input_tokens: None,
            output_tokens: None,
            detail: TrajectoryDetail {
                summary: "Operator input that started this Turn.".to_owned(),
                input: Some(input),
                metadata_json: payload
                    .get("run_scope")
                    .map(Value::to_string)
                    .filter(|value| value != "null"),
                ..TrajectoryDetail::default()
            },
            source_event_ids: vec![event.event_id.clone()],
        });
    }

    fn model_requested(
        &mut self,
        event: &InspectedSessionEvent,
        payload: &Map<String, Value>,
        turn: u32,
    ) {
        let step = u32_value(payload, "step").unwrap_or(1);
        let model = string(payload, "model");
        let index = self.records.len();
        let turn_id = event.turn_id.clone().unwrap_or_default();
        self.model_records.insert((turn_id.clone(), step), index);
        self.latest_model.insert(turn_id, index);
        self.model_calls = self.model_calls.saturating_add(1);
        self.records.push(TrajectoryRecord {
            id: event.event_id.clone(),
            turn,
            kind: TrajectoryKind::Model,
            status: TrajectoryStatus::Running,
            label: "Model call".to_owned(),
            preview: model.as_deref().map_or_else(
                || format!("Step {step}"),
                |model| format!("{model} · Step {step}"),
            ),
            started_at: event.occurred_at.clone(),
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
            step: Some(step),
            input_tokens: None,
            output_tokens: None,
            detail: TrajectoryDetail {
                summary: "Model request and completion for one Agent step.".to_owned(),
                input: request_summary(payload),
                model,
                system_instruction_digest: string(payload, "system_instruction_digest"),
                ..TrajectoryDetail::default()
            },
            source_event_ids: vec![event.event_id.clone()],
        });
    }

    fn model_output(
        &mut self,
        event: &InspectedSessionEvent,
        payload: &Map<String, Value>,
        turn: u32,
    ) {
        let payload_step = u32_value(payload, "step");
        let turn_id = event.turn_id.clone().unwrap_or_default();
        let index = payload_step
            .and_then(|step| self.model_records.get(&(turn_id.clone(), step)).copied())
            .or_else(|| self.latest_model.get(&turn_id).copied());
        let input_tokens = u64_value(payload, "input_tokens");
        let output_tokens = u64_value(payload, "output_tokens");
        self.input_tokens = self
            .input_tokens
            .saturating_add(input_tokens.unwrap_or_default());
        self.output_tokens = self
            .output_tokens
            .saturating_add(output_tokens.unwrap_or_default());
        if let Some(index) = index {
            let record = &mut self.records[index];
            record.status = status(payload).unwrap_or(TrajectoryStatus::Completed);
            record.completed_at = Some(event.occurred_at.clone());
            record.duration_ms = u64_value(payload, "duration_ms")
                .or_else(|| duration_between(&record.started_at, &event.occurred_at));
            record.time_to_first_token_ms = u64_value(payload, "time_to_first_token_ms");
            record.input_tokens = input_tokens;
            record.output_tokens = output_tokens;
            record.detail.output = string(payload, "text");
            record.source_event_ids.push(event.event_id.clone());
            if let Some(model) = string(payload, "model") {
                record.detail.model = Some(model);
            }
            let tool_calls = u64_value(payload, "tool_call_count").unwrap_or_default();
            record.preview = model_preview(record.detail.output.as_deref(), tool_calls);
            if record.status == TrajectoryStatus::Failed {
                self.failed_operations = self.failed_operations.saturating_add(1);
            }
            return;
        }
        self.model_calls = self.model_calls.saturating_add(1);
        self.records.push(TrajectoryRecord {
            id: event.event_id.clone(),
            turn,
            kind: TrajectoryKind::Model,
            status: status(payload).unwrap_or(TrajectoryStatus::Completed),
            label: "Model call".to_owned(),
            preview: model_preview(string(payload, "text").as_deref(), 0),
            started_at: event.occurred_at.clone(),
            completed_at: Some(event.occurred_at.clone()),
            duration_ms: u64_value(payload, "duration_ms"),
            time_to_first_token_ms: u64_value(payload, "time_to_first_token_ms"),
            step: Some(payload_step.unwrap_or(1)),
            input_tokens,
            output_tokens,
            detail: TrajectoryDetail {
                summary: "Model completion from a legacy Session without its request event."
                    .to_owned(),
                output: string(payload, "text"),
                model: string(payload, "model"),
                ..TrajectoryDetail::default()
            },
            source_event_ids: vec![event.event_id.clone()],
        });
    }

    fn tool_requested(
        &mut self,
        event: &InspectedSessionEvent,
        payload: &Map<String, Value>,
        turn: u32,
    ) {
        if let Some(index) = event
            .turn_id
            .as_ref()
            .and_then(|turn_id| self.latest_model.get(turn_id))
            .copied()
        {
            let model = &mut self.records[index];
            if model.status == TrajectoryStatus::Running {
                model.status = TrajectoryStatus::Completed;
                model.completed_at = Some(event.occurred_at.clone());
                model.duration_ms = duration_between(&model.started_at, &event.occurred_at);
                "Tool call requested".clone_into(&mut model.preview);
                model.source_event_ids.push(event.event_id.clone());
            }
        }
        let call_id = string(payload, "call_id").unwrap_or_else(|| event.event_id.clone());
        let name = string(payload, "name").unwrap_or_else(|| "Tool".to_owned());
        let index = self.records.len();
        self.tool_records.insert(call_id.clone(), index);
        self.tool_calls = self.tool_calls.saturating_add(1);
        self.records.push(TrajectoryRecord {
            id: event.event_id.clone(),
            turn,
            kind: TrajectoryKind::Tool,
            status: TrajectoryStatus::Running,
            label: name.clone(),
            preview: "Running".to_owned(),
            started_at: event.occurred_at.clone(),
            completed_at: None,
            duration_ms: None,
            time_to_first_token_ms: None,
            step: None,
            input_tokens: None,
            output_tokens: None,
            detail: TrajectoryDetail {
                summary: "One Tool execution lifecycle.".to_owned(),
                input: string(payload, "arguments_json"),
                tool_name: Some(name),
                tool_call_id: Some(call_id),
                ..TrajectoryDetail::default()
            },
            source_event_ids: vec![event.event_id.clone()],
        });
    }

    fn tool_result(
        &mut self,
        event: &InspectedSessionEvent,
        payload: &Map<String, Value>,
        turn: u32,
    ) {
        let call_id = string(payload, "call_id").unwrap_or_else(|| event.event_id.clone());
        let result_status = status(payload).unwrap_or(TrajectoryStatus::Completed);
        if result_status == TrajectoryStatus::Failed {
            self.failed_operations = self.failed_operations.saturating_add(1);
        }
        if let Some(index) = self.tool_records.get(&call_id).copied() {
            let record = &mut self.records[index];
            record.status = result_status;
            record.completed_at = Some(event.occurred_at.clone());
            record.duration_ms = u64_value(payload, "duration_ms")
                .or_else(|| duration_between(&record.started_at, &event.occurred_at));
            record.detail.output = string(payload, "content").or_else(|| string(payload, "error"));
            record.preview = if result_status == TrajectoryStatus::Failed {
                "Failed".to_owned()
            } else {
                record
                    .detail
                    .output
                    .as_deref()
                    .filter(|output| !output.trim().is_empty())
                    .map_or_else(|| "Completed".to_owned(), preview)
            };
            record.detail.metadata_json = string(payload, "metadata_json");
            record.source_event_ids.push(event.event_id.clone());
            return;
        }
        self.tool_calls = self.tool_calls.saturating_add(1);
        self.records.push(TrajectoryRecord {
            id: event.event_id.clone(),
            turn,
            kind: TrajectoryKind::Tool,
            status: result_status,
            label: string(payload, "name").unwrap_or_else(|| "Tool".to_owned()),
            preview: if result_status == TrajectoryStatus::Failed {
                "Failed".to_owned()
            } else {
                "Completed".to_owned()
            },
            started_at: event.occurred_at.clone(),
            completed_at: Some(event.occurred_at.clone()),
            duration_ms: u64_value(payload, "duration_ms"),
            time_to_first_token_ms: None,
            step: None,
            input_tokens: None,
            output_tokens: None,
            detail: TrajectoryDetail {
                summary: "Tool result from a legacy Session without its request event.".to_owned(),
                output: string(payload, "content").or_else(|| string(payload, "error")),
                metadata_json: string(payload, "metadata_json"),
                tool_name: string(payload, "name"),
                tool_call_id: Some(call_id),
                ..TrajectoryDetail::default()
            },
            source_event_ids: vec![event.event_id.clone()],
        });
    }

    fn memory(&mut self, event: &InspectedSessionEvent, payload: &Map<String, Value>, turn: u32) {
        let failed = event.kind.ends_with("_failed");
        if failed {
            self.failed_operations = self.failed_operations.saturating_add(1);
        }
        let recalled = event.kind.starts_with("memory_recall");
        let count = array_len(payload, "memory_ids");
        self.records.push(TrajectoryRecord {
            id: event.event_id.clone(),
            turn,
            kind: TrajectoryKind::Memory,
            status: if failed {
                TrajectoryStatus::Failed
            } else {
                TrajectoryStatus::Completed
            },
            label: if recalled {
                "Memory recall"
            } else {
                "Memory commit"
            }
            .to_owned(),
            preview: if failed {
                "Failed".to_owned()
            } else {
                count_label(count, "memory item")
            },
            started_at: event.occurred_at.clone(),
            completed_at: Some(event.occurred_at.clone()),
            duration_ms: None,
            time_to_first_token_ms: None,
            step: None,
            input_tokens: None,
            output_tokens: None,
            detail: TrajectoryDetail {
                summary: if recalled {
                    "Long-term memory selected for this Turn."
                } else {
                    "Turn output observed by long-term memory."
                }
                .to_owned(),
                output: payload.get("memory_ids").map(Value::to_string),
                ..TrajectoryDetail::default()
            },
            source_event_ids: vec![event.event_id.clone()],
        });
    }

    fn compaction(&mut self, event: &InspectedSessionEvent, payload: &Map<String, Value>) {
        let compaction_id =
            string(payload, "compaction_id").unwrap_or_else(|| event.event_id.clone());
        if event.kind == "context_compaction_started" {
            let index = self.records.len();
            self.compaction_records.insert(compaction_id, index);
            self.records.push(TrajectoryRecord {
                id: event.event_id.clone(),
                turn: 0,
                kind: TrajectoryKind::Compaction,
                status: TrajectoryStatus::Running,
                label: "Context compaction".to_owned(),
                preview: count_label(
                    usize_value(payload, "source_message_count"),
                    "source message",
                ),
                started_at: event.occurred_at.clone(),
                completed_at: None,
                duration_ms: None,
                time_to_first_token_ms: None,
                step: None,
                input_tokens: None,
                output_tokens: None,
                detail: TrajectoryDetail {
                    summary: "Conversation context compaction lifecycle.".to_owned(),
                    input: payload
                        .get("compacted_through_revision")
                        .map(Value::to_string),
                    ..TrajectoryDetail::default()
                },
                source_event_ids: vec![event.event_id.clone()],
            });
            return;
        }
        let failed = event.kind.ends_with("_failed");
        if failed {
            self.failed_operations = self.failed_operations.saturating_add(1);
        }
        if let Some(index) = self.compaction_records.get(&compaction_id).copied() {
            let record = &mut self.records[index];
            record.status = if failed {
                TrajectoryStatus::Failed
            } else {
                TrajectoryStatus::Completed
            };
            record.completed_at = Some(event.occurred_at.clone());
            record.duration_ms = duration_between(&record.started_at, &event.occurred_at);
            record.preview = if failed {
                "Failed".to_owned()
            } else {
                count_label(
                    usize_value(payload, "source_message_count"),
                    "source message",
                )
            };
            record.detail.output = string(payload, "summary").or_else(|| string(payload, "error"));
            record.source_event_ids.push(event.event_id.clone());
        }
    }

    fn failure(&mut self, event: &InspectedSessionEvent, payload: &Map<String, Value>, turn: u32) {
        let status = if event.kind == "turn_cancelled" {
            TrajectoryStatus::Cancelled
        } else {
            TrajectoryStatus::Failed
        };
        self.records.push(TrajectoryRecord {
            id: event.event_id.clone(),
            turn,
            kind: TrajectoryKind::Model,
            status,
            label: if status == TrajectoryStatus::Cancelled {
                "Turn cancelled"
            } else {
                "Turn failed"
            }
            .to_owned(),
            preview: string(payload, "error").unwrap_or_else(|| "Stopped".to_owned()),
            started_at: event.occurred_at.clone(),
            completed_at: Some(event.occurred_at.clone()),
            duration_ms: None,
            time_to_first_token_ms: None,
            step: None,
            input_tokens: None,
            output_tokens: None,
            detail: TrajectoryDetail {
                summary: "Terminal Turn outcome.".to_owned(),
                output: string(payload, "error"),
                ..TrajectoryDetail::default()
            },
            source_event_ids: vec![event.event_id.clone()],
        });
    }

    fn close_open_records(
        &mut self,
        event: &InspectedSessionEvent,
        payload: &Map<String, Value>,
        turn: u32,
        status: TrajectoryStatus,
    ) {
        let outcome = if status == TrajectoryStatus::Cancelled {
            "Cancelled"
        } else {
            "Failed"
        };
        let error = string(payload, "error");
        for record in self
            .records
            .iter_mut()
            .filter(|record| record.turn == turn && record.status == TrajectoryStatus::Running)
        {
            record.status = status;
            record.completed_at = Some(event.occurred_at.clone());
            record.duration_ms = duration_between(&record.started_at, &event.occurred_at);
            outcome.clone_into(&mut record.preview);
            if record.detail.output.is_none() {
                record.detail.output.clone_from(&error);
            }
            record.source_event_ids.push(event.event_id.clone());
        }
    }

    fn finish(self, session: &InspectedSession) -> Trajectory {
        let started_at = session
            .events
            .first()
            .map(|event| event.occurred_at.clone());
        let updated_at = session.events.last().map(|event| event.occurred_at.clone());
        let duration_ms = started_at
            .as_deref()
            .zip(updated_at.as_deref())
            .and_then(|(start, end)| duration_between(start, end));
        Trajectory {
            schema: Trajectory::SCHEMA.to_owned(),
            session_id: session.session_id.clone(),
            revision: session.revision,
            summary: TrajectorySummary {
                status: self.status,
                turns: u32::try_from(self.turns.len()).unwrap_or(u32::MAX),
                model_calls: self.model_calls,
                tool_calls: self.tool_calls,
                failed_operations: self.failed_operations,
                input_tokens: self.input_tokens,
                output_tokens: self.output_tokens,
                started_at,
                updated_at,
                duration_ms,
            },
            records: self.records,
        }
    }
}

fn payload(event: &InspectedSessionEvent) -> Result<Map<String, Value>, String> {
    serde_json::from_str::<Value>(&event.payload_json)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .ok_or_else(|| {
            format!(
                "Session event `{}` payload must be an object",
                event.event_id
            )
        })
}

fn string(payload: &Map<String, Value>, key: &str) -> Option<String> {
    payload.get(key)?.as_str().map(ToOwned::to_owned)
}

fn u64_value(payload: &Map<String, Value>, key: &str) -> Option<u64> {
    payload
        .get(key)
        .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
}

fn u32_value(payload: &Map<String, Value>, key: &str) -> Option<u32> {
    u64_value(payload, key).and_then(|value| u32::try_from(value).ok())
}

fn usize_value(payload: &Map<String, Value>, key: &str) -> usize {
    u64_value(payload, key)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or_default()
}

fn array_len(payload: &Map<String, Value>, key: &str) -> usize {
    payload
        .get(key)
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

fn status(payload: &Map<String, Value>) -> Option<TrajectoryStatus> {
    match payload.get("status")?.as_str()? {
        "running" => Some(TrajectoryStatus::Running),
        "completed" => Some(TrajectoryStatus::Completed),
        "failed" => Some(TrajectoryStatus::Failed),
        "cancelled" => Some(TrajectoryStatus::Cancelled),
        _ => None,
    }
}

fn duration_between(start: &str, end: &str) -> Option<u64> {
    let start = OffsetDateTime::parse(start, &Rfc3339).ok()?;
    let end = OffsetDateTime::parse(end, &Rfc3339).ok()?;
    u64::try_from((end - start).whole_milliseconds()).ok()
}

fn preview(value: &str) -> String {
    const LIMIT: usize = 120;
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= LIMIT {
        normalized
    } else {
        format!("{}…", normalized.chars().take(LIMIT).collect::<String>())
    }
}

fn model_preview(output: Option<&str>, tool_calls: u64) -> String {
    output
        .filter(|output| !output.trim().is_empty())
        .map_or_else(
            || {
                count_label(
                    usize::try_from(tool_calls).unwrap_or(usize::MAX),
                    "Tool call",
                )
            },
            preview,
        )
}

fn count_label(count: usize, label: &str) -> String {
    let suffix = if count == 1 { "" } else { "s" };
    format!("{count} {label}{suffix}")
}

fn request_summary(payload: &Map<String, Value>) -> Option<String> {
    let messages = usize_value(payload, "message_count");
    let tools = usize_value(payload, "tool_count");
    let max_output_tokens = u64_value(payload, "max_output_tokens")?;
    Some(format!(
        "{messages} messages, {tools} Tools, max {max_output_tokens} output tokens"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(
        revision: u64,
        kind: &str,
        turn_id: Option<&str>,
        occurred_at: &str,
        payload_json: &str,
    ) -> InspectedSessionEvent {
        InspectedSessionEvent {
            revision,
            event_id: format!("event-{revision}"),
            kind: kind.to_owned(),
            turn_id: turn_id.map(ToOwned::to_owned),
            occurred_at: occurred_at.to_owned(),
            payload_json: payload_json.to_owned(),
        }
    }

    #[test]
    fn projection_merges_model_and_tool_lifecycles_with_real_metrics() {
        let session = InspectedSession {
            title: None,
            title_revision: 0,
            session_id: "session-1".to_owned(),
            revision: 6,
            events: vec![
                event(
                    1,
                    "turn_started",
                    Some("turn-1"),
                    "2026-08-29T00:00:00Z",
                    r#"{"input":"Inspect it"}"#,
                ),
                event(
                    2,
                    "model_requested",
                    Some("turn-1"),
                    "2026-08-29T00:00:01Z",
                    r#"{"step":1,"model":"gpt-test","message_count":2,"tool_count":1,"max_output_tokens":128}"#,
                ),
                event(
                    3,
                    "model_output",
                    Some("turn-1"),
                    "2026-08-29T00:00:03Z",
                    r#"{"step":1,"model":"gpt-test","text":"","tool_call_count":1,"input_tokens":20,"output_tokens":4,"duration_ms":2000,"status":"completed"}"#,
                ),
                event(
                    4,
                    "tool_requested",
                    Some("turn-1"),
                    "2026-08-29T00:00:03Z",
                    r#"{"call_id":"call-1","name":"read_file","arguments_json":"{\"path\":\"a\"}"}"#,
                ),
                event(
                    5,
                    "tool_result",
                    Some("turn-1"),
                    "2026-08-29T00:00:04Z",
                    r#"{"call_id":"call-1","name":"read_file","content":"hello","metadata_json":"{}","duration_ms":1000,"status":"completed"}"#,
                ),
                event(
                    6,
                    "turn_completed",
                    Some("turn-1"),
                    "2026-08-29T00:00:05Z",
                    r#"{"output":"Done"}"#,
                ),
            ],
        };

        let projected = project_trajectory(&session).unwrap();

        assert_eq!(projected.summary.turns, 1);
        assert_eq!(projected.summary.input_tokens, 20);
        assert_eq!(projected.summary.output_tokens, 4);
        assert_eq!(projected.records.len(), 3);
        assert_eq!(
            projected.records[1].source_event_ids,
            ["event-2", "event-3"]
        );
        assert_eq!(projected.records[2].duration_ms, Some(1000));
        assert_eq!(projected.records[2].detail.output.as_deref(), Some("hello"));
    }

    #[test]
    fn projection_includes_memory_compaction_and_terminal_failures() {
        let session = InspectedSession {
            title: None,
            title_revision: 0,
            session_id: "session-2".to_owned(),
            revision: 7,
            events: vec![
                event(
                    1,
                    "context_compaction_started",
                    None,
                    "2026-08-29T00:00:00Z",
                    r#"{"compaction_id":"compact-1","source_message_count":8}"#,
                ),
                event(
                    2,
                    "context_compaction_committed",
                    None,
                    "2026-08-29T00:00:01Z",
                    r#"{"compaction_id":"compact-1","source_message_count":8,"summary":"Earlier work"}"#,
                ),
                event(
                    3,
                    "turn_started",
                    Some("turn-1"),
                    "2026-08-29T00:00:02Z",
                    r#"{"input":"Continue"}"#,
                ),
                event(
                    4,
                    "memory_recalled",
                    Some("turn-1"),
                    "2026-08-29T00:00:03Z",
                    r#"{"memory_ids":["m1","m2"]}"#,
                ),
                event(
                    5,
                    "memory_commit_failed",
                    Some("turn-1"),
                    "2026-08-29T00:00:04Z",
                    r#"{"error":"memory_commit_failed"}"#,
                ),
                event(
                    6,
                    "model_requested",
                    Some("turn-1"),
                    "2026-08-29T00:00:04.500Z",
                    r#"{"step":1,"model":"gpt-test"}"#,
                ),
                event(
                    7,
                    "turn_failed",
                    Some("turn-1"),
                    "2026-08-29T00:00:05Z",
                    r#"{"error":"provider_failure"}"#,
                ),
            ],
        };

        let projected = project_trajectory(&session).unwrap();

        assert_eq!(projected.summary.status, TrajectoryStatus::Failed);
        assert_eq!(projected.summary.failed_operations, 2);
        assert_eq!(projected.records[0].kind, TrajectoryKind::Compaction);
        assert_eq!(projected.records[0].duration_ms, Some(1000));
        assert_eq!(projected.records[2].preview, "2 memory items");
        assert!(
            projected
                .records
                .iter()
                .all(|record| record.status != TrajectoryStatus::Running)
        );
        assert_eq!(projected.records.last().unwrap().label, "Turn failed");
    }

    #[test]
    fn projection_closes_legacy_tool_steps_and_pairs_unscoped_model_output() {
        let session = InspectedSession {
            title: None,
            title_revision: 0,
            session_id: "session-legacy".to_owned(),
            revision: 7,
            events: vec![
                event(
                    1,
                    "turn_started",
                    Some("turn-1"),
                    "2026-08-29T00:00:00Z",
                    r#"{"input":"Use a Tool"}"#,
                ),
                event(
                    2,
                    "model_requested",
                    Some("turn-1"),
                    "2026-08-29T00:00:01Z",
                    r#"{"step":1}"#,
                ),
                event(
                    3,
                    "tool_requested",
                    Some("turn-1"),
                    "2026-08-29T00:00:02Z",
                    r#"{"call_id":"call-1","name":"read","arguments_json":"{}"}"#,
                ),
                event(
                    4,
                    "tool_result",
                    Some("turn-1"),
                    "2026-08-29T00:00:03Z",
                    r#"{"call_id":"call-1","name":"read","metadata_json":"{}"}"#,
                ),
                event(
                    5,
                    "model_requested",
                    Some("turn-1"),
                    "2026-08-29T00:00:04Z",
                    r#"{"step":2}"#,
                ),
                event(
                    6,
                    "model_output",
                    Some("turn-1"),
                    "2026-08-29T00:00:05Z",
                    r#"{"text":"Done"}"#,
                ),
                event(
                    7,
                    "turn_completed",
                    Some("turn-1"),
                    "2026-08-29T00:00:06Z",
                    r#"{"output":"Done"}"#,
                ),
            ],
        };

        let projected = project_trajectory(&session).unwrap();
        let models = projected
            .records
            .iter()
            .filter(|record| record.kind == TrajectoryKind::Model)
            .collect::<Vec<_>>();

        assert_eq!(models.len(), 2);
        assert_eq!(models[0].status, TrajectoryStatus::Completed);
        assert_eq!(models[0].preview, "Tool call requested");
        assert_eq!(models[1].status, TrajectoryStatus::Completed);
        assert_eq!(models[1].detail.output.as_deref(), Some("Done"));
    }
}
