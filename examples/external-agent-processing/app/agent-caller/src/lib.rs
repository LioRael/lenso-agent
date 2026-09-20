//! A minimal external Surface dependency used to obtain the public Agent handle.

use lenso::prelude::*;
use lenso_capability_agent as agent;

// This Plugin only consumes the public Agent capability. Declaring it as a
// consumer makes its generated native factory linkable without inventing a
// private capability just for the fixture Host.
#[lenso::plugin(consumer)]
#[derive(Clone, Debug)]
struct AgentCaller {
    #[allow(dead_code)]
    agent: Port<agent::AgentClient>,
}

pub fn link() {
    link_plugin();
}
