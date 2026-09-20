//! An external surface that receives only its selected interaction port.

use lenso::Port;
use lenso_capability_agent_interaction as interaction;

#[lenso::plugin(consumer)]
#[derive(Clone, Debug)]
struct RealtimeCaller {
    _interaction: Port<interaction::InteractionClient>,
}

pub fn link() {
    link_plugin();
}
