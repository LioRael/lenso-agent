//! Final authorization proof: only post-transform arguments are admitted.

use lenso::prelude::*;
use lenso_capability_agent_tool_hook::{
    self as hook, AfterExecuteError, AfterExecuteRequest, AfterExecuteResponse, BeforeExecuteError,
    BeforeExecuteRequest, BeforeExecuteResponse, HookDecision,
};

#[derive(Clone, Debug, Default, serde::Deserialize, lenso::PluginConfig)]
#[serde(deny_unknown_fields)]
struct FinalApprovalConfig {}

#[lenso::plugin]
#[derive(Clone, Debug)]
struct FinalApproval {
    #[config]
    config: FinalApprovalConfig,
}

pub fn link() {
    link_plugin();
}

#[lenso::provides(hook::ToolHook)]
impl FinalApproval {
    async fn before_execute(
        &self,
        _context: Ctx,
        request: BeforeExecuteRequest,
    ) -> PluginResult<BeforeExecuteResponse, BeforeExecuteError> {
        let _ = &self.config;
        let allowed = request.tool_name == "read_secret"
            && request.arguments_json.as_str() == r#"{"id":"approved"}"#;
        Ok(BeforeExecuteResponse {
            decision: if allowed {
                HookDecision::Allow
            } else {
                HookDecision::Deny
            },
            reason_code: if allowed {
                "approved_final_arguments".to_owned()
            } else {
                "approval_does_not_match_final_arguments".to_owned()
            },
            message: if allowed {
                "Final transformed arguments are approved.".to_owned()
            } else {
                "An approval cannot be reused after Tool arguments change.".to_owned()
            },
            context_json: "{}".try_into().expect("fixture hook context is valid JSON"),
        })
    }

    async fn after_execute(
        &self,
        _context: Ctx,
        _request: AfterExecuteRequest,
    ) -> PluginResult<AfterExecuteResponse, AfterExecuteError> {
        let _ = &self.config;
        Ok(AfterExecuteResponse {})
    }
}
