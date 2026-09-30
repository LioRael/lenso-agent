//! Host-selected task and Session identity; it never grants management permission.
use lenso::{CtxExt, TypedExtension};
use lenso_kernel::InvocationContext;

pub const AGENT_TASK_BINDING_EXTENSION: &str = "lenso.agent.task-binding@1";

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct AgentTaskBinding {
    pub task_id: String,
    pub agent_session_id: String,
    pub delegate_caller: String,
}

impl AgentTaskBinding {
    pub fn validate(&self) -> Result<(), String> {
        let label = |value: &str| {
            !value.is_empty()
                && value.len() <= 128
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b':'))
        };
        let caller = self.delegate_caller.split('/').collect::<Vec<_>>();
        if !label(&self.task_id)
            || !label(&self.agent_session_id)
            || caller.len() != 2
            || !caller.iter().all(|part| label(part))
        {
            return Err("invalid Agent task binding".into());
        }
        Ok(())
    }

    pub fn attach(&self, context: InvocationContext) -> Result<InvocationContext, String> {
        self.validate()?;
        context
            .with_typed_extension(self)
            .map_err(|_| "Agent task binding unavailable".into())
    }

    pub fn accepts_session(&self, session_id: Option<&str>) -> bool {
        self.validate().is_ok() && session_id == Some(self.agent_session_id.as_str())
    }
}

impl TypedExtension for AgentTaskBinding {
    const KEY: &'static str = AGENT_TASK_BINDING_EXTENSION;
}

#[cfg(test)]
mod tests {
    use super::AgentTaskBinding;

    #[test]
    fn a_task_cannot_open_or_resume_a_different_session() {
        let binding = AgentTaskBinding {
            task_id: "task-1".into(),
            agent_session_id: "session-1".into(),
            delegate_caller: "lenso.agent.management-connection/default".into(),
        };
        assert!(binding.accepts_session(Some("session-1")));
        assert!(!binding.accepts_session(None));
        assert!(!binding.accepts_session(Some("session-2")));
        let mut invalid = binding;
        invalid.delegate_caller = "other/instance/extra".into();
        assert!(invalid.validate().is_err());
    }
}
