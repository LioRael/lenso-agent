//! Pure validation and safe fallback helpers for extension-state Providers and readers.

use sha2::{Digest as _, Sha256};

use crate::{AppendRequest, ExtensionAssociation, ExtensionRecord, ReadRequest, SafePresentation};

const MAX_PAYLOAD_BYTES: usize = 65_536;
const MAX_PRESENTATION_BYTES: usize = 16_384;
const MAX_NAMESPACE_BYTES: usize = 192;
const MAX_SCHEMA_VERSION_BYTES: usize = 64;
const MAX_OWNER_BYTES: usize = 256;

/// Stable, text-only fallback kind used when no selected trusted presenter
/// understands a record's semantic presentation kind.
pub const GENERIC_SAFE_PRESENTATION_KIND: &str = "lenso.agent.extension-state.generic@1";

/// A safe generic inspection view. It deliberately excludes the original
/// payload so an unknown extension cannot become a credential or executable
/// content disclosure channel.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct GenericInspection {
    pub kind: &'static str,
    pub namespace: String,
    pub schema_version: String,
    pub event_id: String,
    pub owner_instance: String,
    pub recovery_required: bool,
    pub payload_bytes: usize,
    pub payload_sha256: String,
    pub presentation: SafePresentationView,
}

/// Text-only, bounded fields safely derived from the record's declared
/// presentation. A Console can render these as plain text and JSON metadata.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct SafePresentationView {
    pub kind: String,
    pub title: String,
    pub summary: String,
    pub attributes_json: String,
}

/// The required action when a reader does not have an explicitly selected
/// semantic interpreter for a durable record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryDisposition {
    SafeToInspectOnly,
    BlockedByUnknownRequiredRecord,
}

/// Validates an append request before a Provider gives it a durable identity.
/// `owner_instance` must come from the resolved invocation caller, never from
/// the payload supplied by an ordinary protocol message.
pub fn validate_append_request(
    request: &AppendRequest,
    owner_instance: &str,
) -> Result<(), String> {
    validate_identity("namespace", &request.namespace, MAX_NAMESPACE_BYTES)?;
    validate_identity(
        "schema version",
        &request.schema_version,
        MAX_SCHEMA_VERSION_BYTES,
    )?;
    validate_identity("event id", &request.event_id, 192)?;
    validate_identity("idempotency key", &request.idempotency_key, 192)?;
    validate_owner(owner_instance)?;
    validate_sequence(&request.sequence)?;
    validate_association(&request.association)?;
    validate_json(
        "extension payload",
        request.payload_json.as_str(),
        MAX_PAYLOAD_BYTES,
    )?;
    validate_presentation(&request.presentation)?;
    Ok(())
}

/// Validates a read cursor and association without granting a caller access to
/// an unselected state Provider or another Provider's internal store.
pub fn validate_read_request(request: &ReadRequest) -> Result<(), String> {
    validate_identity("namespace", &request.namespace, MAX_NAMESPACE_BYTES)?;
    validate_association(&request.association)?;
    if let Some(Some(sequence)) = &request.after_sequence {
        validate_sequence(sequence)?;
    }
    if !(1..=128).contains(&request.limit) {
        return Err("extension-state read limit must be between 1 and 128".to_owned());
    }
    Ok(())
}

/// Creates the only record shape that a Provider may persist after deriving
/// its trusted owner from the actual caller context.
#[must_use]
pub fn record_from_append(
    request: AppendRequest,
    owner_instance: impl Into<String>,
) -> ExtensionRecord {
    ExtensionRecord {
        namespace: request.namespace,
        schema_version: request.schema_version,
        event_id: request.event_id,
        sequence: request.sequence,
        idempotency_key: request.idempotency_key,
        owner_instance: owner_instance.into(),
        association: request.association,
        payload_json: request.payload_json,
        presentation: request.presentation,
        recovery_required: request.recovery_required,
    }
}

/// Projects an unrecognized record to safe inspection data. The returned
/// information can be rendered without executing HTML, JavaScript, a schema
/// migration, a Plugin loader, a Model, or an external Tool.
#[must_use]
pub fn generic_inspection(record: &ExtensionRecord) -> GenericInspection {
    let digest = Sha256::digest(record.payload_json.as_bytes());
    GenericInspection {
        kind: GENERIC_SAFE_PRESENTATION_KIND,
        namespace: record.namespace.clone(),
        schema_version: record.schema_version.clone(),
        event_id: record.event_id.clone(),
        owner_instance: record.owner_instance.clone(),
        recovery_required: record.recovery_required,
        payload_bytes: record.payload_json.as_str().len(),
        payload_sha256: format!("sha256:{digest:x}"),
        presentation: SafePresentationView {
            kind: record.presentation.kind.clone(),
            title: record.presentation.title.clone(),
            summary: record.presentation.summary.clone(),
            attributes_json: record.presentation.attributes_json.as_str().to_owned(),
        },
    }
}

/// Determines the recovery outcome for an unknown record. Known records are
/// interpreted by a selected trusted Plugin outside this pure helper.
#[must_use]
pub const fn assess_recovery(
    record: &ExtensionRecord,
    known_semantics: bool,
) -> RecoveryDisposition {
    if known_semantics || !record.recovery_required {
        RecoveryDisposition::SafeToInspectOnly
    } else {
        RecoveryDisposition::BlockedByUnknownRequiredRecord
    }
}

fn validate_identity(label: &str, value: &str, limit: usize) -> Result<(), String> {
    if value.is_empty() || value.len() > limit || value.chars().any(char::is_control) {
        return Err(format!(
            "extension-state {label} is empty, too long, or contains control text"
        ));
    }
    Ok(())
}

fn validate_owner(value: &str) -> Result<(), String> {
    validate_identity("owner instance", value, MAX_OWNER_BYTES)
}

fn validate_sequence(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 20
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || value.parse::<u64>().is_err()
    {
        return Err("extension-state sequence must be an unsigned decimal u64".to_owned());
    }
    Ok(())
}

fn validate_association(association: &ExtensionAssociation) -> Result<(), String> {
    let values = [
        association
            .session_id
            .as_ref()
            .and_then(|value| value.as_deref()),
        association
            .run_id
            .as_ref()
            .and_then(|value| value.as_deref()),
        association
            .task_id
            .as_ref()
            .and_then(|value| value.as_deref()),
    ];
    if values.iter().all(Option::is_none) {
        return Err("extension-state record must associate a Session, Run, or Task".to_owned());
    }
    for value in values.into_iter().flatten() {
        validate_identity("association identity", value, 128)?;
    }
    Ok(())
}

fn validate_json(label: &str, value: &str, limit: usize) -> Result<(), String> {
    if value.len() > limit {
        return Err(format!("{label} exceeds its bounded size"));
    }
    serde_json::from_str::<serde_json::Value>(value)
        .map_err(|_| format!("{label} is not valid JSON"))?;
    Ok(())
}

fn validate_presentation(presentation: &SafePresentation) -> Result<(), String> {
    validate_identity("presentation kind", &presentation.kind, 192)?;
    if presentation.title.len() > 512 || presentation.summary.len() > 4_096 {
        return Err("extension-state presentation text exceeds its bounded size".to_owned());
    }
    if presentation.title.contains('<')
        || presentation.title.contains('>')
        || presentation.summary.contains('<')
        || presentation.summary.contains('>')
    {
        return Err("extension-state presentation is text, not markup".to_owned());
    }
    validate_json(
        "extension-state presentation attributes",
        presentation.attributes_json.as_str(),
        MAX_PRESENTATION_BYTES,
    )
}

#[cfg(test)]
mod tests {
    use super::{
        RecoveryDisposition, assess_recovery, generic_inspection, record_from_append,
        validate_append_request,
    };
    use crate::{AppendRequest, ExtensionAssociation, RawJson, SafePresentation};

    fn append(recovery_required: bool) -> AppendRequest {
        AppendRequest {
            namespace: "example.task-board-event@1".to_owned(),
            schema_version: "1.0.0".to_owned(),
            event_id: "approval-requested".to_owned(),
            sequence: "7".to_owned(),
            idempotency_key: "event-7".to_owned(),
            association: ExtensionAssociation {
                session_id: Some(Some("session-1".to_owned())),
                run_id: None,
                task_id: Some(Some("task-1".to_owned())),
            },
            payload_json: RawJson::new(r#"{"approval":"pending"}"#).unwrap(),
            presentation: SafePresentation {
                kind: "example.task-board.presentation@1".to_owned(),
                title: "Approval requested".to_owned(),
                summary: "A task is waiting for approval.".to_owned(),
                attributes_json: RawJson::new(r#"{"priority":"high"}"#).unwrap(),
            },
            recovery_required,
        }
    }

    #[test]
    fn caller_owns_the_persisted_record_and_unknown_required_state_blocks_recovery() {
        let request = append(true);
        validate_append_request(&request, "example.task-board/default").unwrap();
        let record = record_from_append(request, "example.task-board/default");
        assert_eq!(record.owner_instance, "example.task-board/default");
        assert_eq!(
            assess_recovery(&record, false),
            RecoveryDisposition::BlockedByUnknownRequiredRecord
        );
        assert_eq!(
            assess_recovery(&record, true),
            RecoveryDisposition::SafeToInspectOnly
        );
    }

    #[test]
    fn generic_inspection_excludes_the_payload_and_rejects_markup() {
        let record = record_from_append(append(false), "example.task-board/default");
        let inspection = generic_inspection(&record);
        let serialized = serde_json::to_string(&inspection).unwrap();
        assert!(!serialized.contains("pending"));
        assert!(inspection.payload_sha256.starts_with("sha256:"));

        let mut unsafe_request = append(false);
        unsafe_request.presentation.summary = "<script>run()</script>".to_owned();
        assert!(validate_append_request(&unsafe_request, "example.task-board/default").is_err());
    }
}
