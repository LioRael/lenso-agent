//! Host-bound sealed grants and final execution revalidation.

use lenso_kernel::{InvocationContext, RuntimeFailure, SealedInvocationExtension};
use lenso_plugin_authoring::ManyPort;

use crate::{
    AuthorityDecision, DynamicAuthorityClient, DynamicAuthorityRevalidateInvocationError,
    ResourceIdentity, RevalidateError, RevalidateRequest, SelectResponse,
};

/// The only accepted authority extension key for a Tool execution.
pub const TOOL_AUTHORITY_EXTENSION: &str = "lenso.agent.dynamic-authority.tool-grant@1";
const TOOLS_EXECUTE_AUDIENCE: &str = "lenso.agent.tools@2:execute";
const TOOLS_EXECUTE_STREAM_AUDIENCE: &str = "lenso.agent.tools@2:execute_stream";
const REVALIDATE_AUDIENCE: &str = "lenso.agent.dynamic-authority@1:revalidate";

/// A selected policy snapshot encoded inside a sealed Invocation Context
/// extension. Ordinary user input cannot create or replace this Kernel-owned
/// sealed extension key.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolAuthorityGrant {
    pub snapshot: SelectResponse,
}

/// Result of final authorization immediately before a concrete resource call.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolAuthorityState {
    NotRequested,
    Authorized {
        policy_revision: String,
    },
    Denied {
        reason_code: String,
        message: String,
    },
}

/// Attaches a selected authority snapshot to a root Invocation Context. The
/// caller must be a trusted Host boundary that has already authenticated the
/// subject and verified the selected authority Provider's issuance proof.
pub fn attach_tool_authority(
    context: InvocationContext,
    snapshot: SelectResponse,
    proof: impl Into<String>,
) -> Result<InvocationContext, String> {
    validate_selection_snapshot(&snapshot)?;
    let value = serde_json::to_vec(&ToolAuthorityGrant {
        snapshot: snapshot.clone(),
    })
    .map_err(|error| format!("failed to encode Tool authority grant: {error}"))?;
    context
        .with_sealed_extension(SealedInvocationExtension::signed(
            TOOL_AUTHORITY_EXTENSION,
            snapshot.authority_instance,
            [
                TOOLS_EXECUTE_AUDIENCE,
                TOOLS_EXECUTE_STREAM_AUDIENCE,
                REVALIDATE_AUDIENCE,
            ],
            value,
            proof,
        ))
        .map_err(|error| format!("failed to attach Tool authority grant: {error}"))
}

/// Reads the narrow grant visible to the currently targeted Provider. An
/// ordinary extension with the same key is treated as a forgery attempt and
/// fails closed rather than being interpreted as authority.
pub fn tool_authority_from_context(
    context: &InvocationContext,
) -> Result<Option<ToolAuthorityGrant>, RuntimeFailure> {
    if context.extension(TOOL_AUTHORITY_EXTENSION).is_some() {
        return Err(authority_failure(
            "ordinary invocation input cannot impersonate a Host-issued authority grant",
        ));
    }
    let Some(extension) = context.sealed_extension(TOOL_AUTHORITY_EXTENSION) else {
        return Ok(None);
    };
    let grant = serde_json::from_slice::<ToolAuthorityGrant>(extension.value())
        .map_err(|_| authority_failure("Tool authority sealed extension has an invalid payload"))?;
    validate_selection_snapshot(&grant.snapshot).map_err(authority_failure)?;
    if extension.issuer() != grant.snapshot.authority_instance {
        return Err(authority_failure(
            "Tool authority issuer does not match its granted authority instance",
        ));
    }
    Ok(Some(grant))
}

/// Revalidates a sealed selected resource immediately before execution. A
/// missing grant leaves existing static Plan behavior untouched. When a grant
/// is present, exactly one selected authority Provider must decide it; zero or
/// multiple Providers fail closed.
pub async fn revalidate_tool_authority(
    authorities: &ManyPort<DynamicAuthorityClient>,
    context: &InvocationContext,
    resource: ResourceIdentity,
) -> Result<ToolAuthorityState, RuntimeFailure> {
    let Some(grant) = tool_authority_from_context(context)? else {
        return Ok(ToolAuthorityState::NotRequested);
    };
    if !grant.snapshot.granted_resources.contains(&resource) {
        return Ok(ToolAuthorityState::Denied {
            reason_code: "resource_not_in_snapshot".to_owned(),
            message: "The selected authority snapshot does not grant this resource.".to_owned(),
        });
    }
    if authorities.len() != 1 {
        return Err(authority_failure(
            "a sealed authority grant requires exactly one Plan-bound Dynamic Authority Provider",
        ));
    }
    let provider = &authorities[0];
    if provider.provider_instance() != grant.snapshot.authority_instance {
        return Err(authority_failure(
            "the selected Dynamic Authority Provider differs from the sealed grant issuer",
        ));
    }
    let result = provider
        .revalidate_with_context(
            context.clone(),
            RevalidateRequest {
                snapshot_id: grant.snapshot.snapshot_id,
                policy_revision: grant.snapshot.policy_revision,
                resource,
            },
        )
        .await;
    match result {
        Ok(response) => match response.decision {
            AuthorityDecision::Allow => Ok(ToolAuthorityState::Authorized {
                policy_revision: response.observed_policy_revision,
            }),
            AuthorityDecision::Deny => Ok(ToolAuthorityState::Denied {
                reason_code: response.reason_code,
                message: response.message,
            }),
        },
        Err(DynamicAuthorityRevalidateInvocationError::Domain(error)) => {
            Ok(ToolAuthorityState::Denied {
                reason_code: domain_error_code(&error).to_owned(),
                message: "The Dynamic Authority Provider rejected this snapshot.".to_owned(),
            })
        }
        Err(DynamicAuthorityRevalidateInvocationError::Runtime(error)) => Err(error),
    }
}

/// Checks public snapshot invariants before it becomes trusted Host baggage.
pub fn validate_selection_snapshot(snapshot: &SelectResponse) -> Result<(), String> {
    validate_identity("snapshot id", &snapshot.snapshot_id, 192)?;
    validate_identity("policy revision", &snapshot.policy_revision, 192)?;
    validate_identity("authority instance", &snapshot.authority_instance, 256)?;
    if snapshot.granted_resources.len() > 256 {
        return Err("authority snapshot has too many resources".to_owned());
    }
    let _ = snapshot
        .expires_at_unix_ms
        .parse::<u64>()
        .map_err(|_| "authority snapshot expiry is not a decimal u64".to_owned())?;
    for resource in &snapshot.granted_resources {
        validate_resource(resource)?;
    }
    Ok(())
}

fn validate_resource(resource: &ResourceIdentity) -> Result<(), String> {
    validate_identity("resource kind", &resource.kind, 64)?;
    validate_identity("resource name", &resource.name, 128)?;
    validate_identity(
        "resource provider instance",
        &resource.provider_instance,
        256,
    )?;
    validate_identity("resource revision", &resource.revision, 192)
}

fn validate_identity(label: &str, value: &str, max: usize) -> Result<(), String> {
    if value.is_empty() || value.len() > max || value.chars().any(char::is_control) {
        return Err(format!(
            "authority {label} is empty, too long, or contains control text"
        ));
    }
    Ok(())
}

fn domain_error_code(error: &RevalidateError) -> &str {
    match error {
        RevalidateError::InvalidSelection => "invalid_selection",
        RevalidateError::Unauthorized => "unauthorized",
        RevalidateError::SnapshotExpired => "snapshot_expired",
        RevalidateError::SnapshotUnknown => "snapshot_unknown",
        RevalidateError::Revoked => "revoked",
        RevalidateError::Unknown(error) => &error.code,
    }
}

fn authority_failure(detail: impl Into<String>) -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: format!("Dynamic Authority failure: {}", detail.into()),
    }
}

#[cfg(test)]
mod tests {
    use lenso_kernel::{CancellationToken, InvocationContext};

    use super::{
        TOOL_AUTHORITY_EXTENSION, attach_tool_authority, tool_authority_from_context,
        validate_selection_snapshot,
    };
    use crate::{ResourceIdentity, SelectResponse};

    fn snapshot() -> SelectResponse {
        SelectResponse {
            snapshot_id: "snapshot-1".to_owned(),
            policy_revision: "policy-7".to_owned(),
            authority_instance: "example.tenant-policy/default".to_owned(),
            granted_resources: vec![ResourceIdentity {
                kind: "tool".to_owned(),
                name: "orders_read".to_owned(),
                provider_instance: "example.orders/default".to_owned(),
                revision: "sha256:abc".to_owned(),
            }],
            expires_at_unix_ms: "4102444800000".to_owned(),
        }
    }

    #[test]
    fn only_a_sealed_host_grant_is_accepted_as_tool_authority() {
        let context = InvocationContext::new(1, None, CancellationToken::new());
        let sealed = attach_tool_authority(context, snapshot(), "trusted-proof").unwrap();
        let grant = tool_authority_from_context(&sealed).unwrap().unwrap();
        assert_eq!(grant.snapshot.snapshot_id, "snapshot-1");

        let forged = InvocationContext::new(2, None, CancellationToken::new())
            .with_extension(
                TOOL_AUTHORITY_EXTENSION,
                br#"{"snapshot":"forged"}"#.to_vec(),
            )
            .unwrap();
        assert!(tool_authority_from_context(&forged).is_err());
    }

    #[test]
    fn snapshots_have_exact_resource_identities() {
        let mut value = snapshot();
        validate_selection_snapshot(&value).unwrap();
        value.granted_resources[0].revision.clear();
        assert!(validate_selection_snapshot(&value).is_err());
    }
}
