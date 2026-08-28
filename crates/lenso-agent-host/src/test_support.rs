use std::sync::OnceLock;

use lenso_agent_cli_plugin as _;
use lenso_app_plan::authoring::PluginRootSnapshot;

static HEADLESS_PLAN: OnceLock<Vec<u8>> = OnceLock::new();

pub(crate) fn headless_plan() -> &'static [u8] {
    HEADLESS_PLAN.get_or_init(resolve)
}

fn resolve() -> Vec<u8> {
    lenso_agent_default_plugins::link();
    let plan = crate::generation::resolve_host_plan(&PluginRootSnapshot::default())
        .expect("the linked Host defaults should resolve");
    serde_json::to_vec(&plan).expect("the derived Plan should serialize")
}
