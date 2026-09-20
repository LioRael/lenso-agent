//! An external consumer with no privileged access to the persisted store.

use lenso::Port;
use lenso_capability_agent_durable_task as durable_task;
use lenso_capability_agent_extension_state as extension_state;

#[lenso::plugin(consumer)]
#[derive(Clone, Debug)]
struct StateTaskCaller {
    _durable_tasks: Port<durable_task::DurableTaskClient>,
    _extension_state: Port<extension_state::ExtensionStateClient>,
}

pub fn link() {
    link_plugin();
}
