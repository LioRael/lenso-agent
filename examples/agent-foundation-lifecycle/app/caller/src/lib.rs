//! Explicit Host binding anchor; it grants no authority beyond its typed ports.

use lenso::Port;
use lenso_capability_agent as agent;
use lenso_capability_agent_model as model;
use lenso_capability_agent_session as session;
use lenso_capability_agent_tool_provider as tools;

#[lenso::plugin(consumer)]
#[derive(Clone, Debug)]
struct LifecycleCaller {
    _agent: Port<agent::AgentClient>,
    _model: Port<model::ModelClient>,
    _session: Port<session::SessionClient>,
    _task_tool: Port<tools::ToolProviderClient>,
}

pub fn link() {
    link_plugin();
}
