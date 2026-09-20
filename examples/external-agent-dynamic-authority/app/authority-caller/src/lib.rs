//! A surface that can ask its selected policy Provider and selected Tool runtime.

use lenso::Port;
use lenso_capability_agent_dynamic_authority as authority;
use lenso_capability_agent_tools as tools;

#[lenso::plugin(consumer)]
#[derive(Clone, Debug)]
struct AuthorityCaller {
    _authority: Port<authority::DynamicAuthorityClient>,
    _tools: Port<tools::ToolsClient>,
}

pub fn link() {
    link_plugin();
}
