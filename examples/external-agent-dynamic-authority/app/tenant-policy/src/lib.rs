//! External tenant policy used to prove final Tool authorization.
//!
//! The source only trusts the Kernel-resolved caller for selection. At final
//! execution it requires the Host-issued sealed grant, then reads an external
//! revocation fact before returning a decision.

use std::{fs, path::Path};

use lenso::prelude::*;
use lenso_capability_agent_dynamic_authority::{
    self as authority, AuthorityDecision, ResourceIdentity, RevalidateRequest, RevalidateResponse,
    SelectRequest, SelectResponse,
};
use lenso_capability_agent_tool_hook as hook;

const SELECTING_CALLER: &str = "example.authority-caller/surface";
const AUTHORITY_INSTANCE: &str = "example.tenant-policy/default";
const TOOL_PROVIDER: &str = "example.policy-tool/default";
const TOOL_NAME: &str = "read_policy_value";

#[derive(Clone, Debug, serde::Deserialize, PluginConfig)]
#[serde(deny_unknown_fields)]
struct TenantPolicyConfig {
    policy_path: String,
}

#[lenso::plugin(validate = validate_config)]
#[derive(Clone, Debug)]
struct TenantPolicy {
    #[config]
    config: TenantPolicyConfig,
}

fn validate_config(config: &TenantPolicyConfig) -> Result<(), RuntimeFailure> {
    if !Path::new(&config.policy_path).is_absolute() || config.policy_path.len() > 4_096 {
        return Err(RuntimeFailure::InvalidResolvedPlan {
            detail: "external tenant policy requires an absolute bounded policy path".to_owned(),
        });
    }
    Ok(())
}

impl TenantPolicy {
    fn revoked(&self) -> bool {
        fs::read_to_string(&self.config.policy_path)
            .map(|value| value.trim() == "revoked")
            .unwrap_or(false)
    }

    fn is_selected_tool(resource: &ResourceIdentity) -> bool {
        resource.kind == "tool"
            && resource.name == TOOL_NAME
            && resource.provider_instance == TOOL_PROVIDER
            && resource.revision.starts_with("sha256:")
            && resource.revision.len() == "sha256:".len() + 64
    }
}

#[lenso::provides(authority::DynamicAuthority, hook::ToolHook)]
impl TenantPolicy {
    async fn before_execute(
        &self,
        _context: Ctx,
        _request: hook::BeforeExecuteRequest,
    ) -> PluginResult<hook::BeforeExecuteResponse, hook::BeforeExecuteError> {
        // Deterministically model revocation while an approval is outstanding.
        if fs::read_to_string(&self.config.policy_path).unwrap() == "revoke-on-approval" {
            fs::write(&self.config.policy_path, "revoked").unwrap();
        }
        Ok(hook::BeforeExecuteResponse {
            decision: hook::HookDecision::Allow,
            reason_code: "approved".to_owned(),
            message: "Fixture approval completed.".to_owned(),
            context_json: "{}".try_into().unwrap(),
        })
    }

    async fn after_execute(
        &self,
        _context: Ctx,
        _request: hook::AfterExecuteRequest,
    ) -> PluginResult<hook::AfterExecuteResponse, hook::AfterExecuteError> {
        fs::write(
            Path::new(&self.config.policy_path).with_extension("settled"),
            "settled",
        )
        .unwrap();
        Ok(hook::AfterExecuteResponse {})
    }

    async fn select(
        &self,
        context: Ctx,
        request: SelectRequest,
    ) -> PluginResult<SelectResponse, authority::SelectError> {
        if context.caller_instance() != Some(SELECTING_CALLER) {
            return Err(PluginError::domain(authority::SelectError::Unauthorized));
        }
        if self.revoked() {
            return Err(PluginError::domain(authority::SelectError::Revoked));
        }
        if request.selection_id.is_empty()
            || request.requested_resources.len() != 1
            || !Self::is_selected_tool(&request.requested_resources[0])
        {
            return Err(PluginError::domain(
                authority::SelectError::InvalidSelection,
            ));
        }
        Ok(SelectResponse {
            snapshot_id: format!("tenant-snapshot-{}", request.selection_id),
            policy_revision: "tenant-policy-v1".to_owned(),
            authority_instance: AUTHORITY_INSTANCE.to_owned(),
            granted_resources: request.requested_resources,
            expires_at_unix_ms: "4102444800000".to_owned(),
        })
    }

    async fn revalidate(
        &self,
        context: Ctx,
        request: RevalidateRequest,
    ) -> PluginResult<RevalidateResponse, authority::RevalidateError> {
        if context
            .sealed_extension(authority::TOOL_AUTHORITY_EXTENSION)
            .is_none()
        {
            return Err(PluginError::domain(
                authority::RevalidateError::Unauthorized,
            ));
        }
        if !request.snapshot_id.starts_with("tenant-snapshot-")
            || request.policy_revision != "tenant-policy-v1"
            || !Self::is_selected_tool(&request.resource)
        {
            return Err(PluginError::domain(
                authority::RevalidateError::SnapshotUnknown,
            ));
        }
        let (decision, reason_code, message) = if self.revoked() {
            (
                AuthorityDecision::Deny,
                "tenant_revoked",
                "The tenant policy revoked this Tool after selection.",
            )
        } else {
            (
                AuthorityDecision::Allow,
                "tenant_active",
                "The selected Tool remains authorized.",
            )
        };
        Ok(RevalidateResponse {
            decision,
            reason_code: reason_code.to_owned(),
            message: message.to_owned(),
            observed_policy_revision: "tenant-policy-v1".to_owned(),
        })
    }
}

pub fn link() {
    link_plugin();
}
