//! Explicit, deterministic dialogue-only composition for Lenso Agent.
//!
//! This optional composition is separate from the blank Foundation and from
//! the legacy Coding Host. It selects one fixture Model and no Tool Provider,
//! workspace capability, network capability, coding prompt, or channel.

use std::path::Path;

use lenso_app_plan::{
    ResolvedAppPlan,
    authoring::{
        HostCatalog, HostDefaultPlugin, HostSlot, PluginRootSnapshot, resolve_plugin_root,
    },
};
use lenso_native_adapter::NativePluginRegistry;

use lenso_agent_artifact_file_plugin as _;
use lenso_agent_cli_plugin as _;
use lenso_agent_context_compaction_plugin as _;
use lenso_agent_loop_plugin as _;
use lenso_agent_memory_sqlite_plugin as _;
use lenso_agent_model_fixture_plugin as _;
use lenso_agent_prompt_plugin as _;
use lenso_agent_session_sqlite_plugin as _;
use lenso_agent_tools_plugin as _;

/// Stable identity for this explicit composition.
pub const COMPOSITION_ID: &str = "lenso.agent.dialogue-starter@1";

/// Forces the selected optional Plugin inventory into the consuming Host binary.
///
/// The inventory is intentionally small and contains no Coding or workspace
/// automation Plugin. Calling this function does not enable or run a Plugin;
/// [`starter_plan`] selects the exact instances in an immutable Plan.
pub fn link() {}

/// Resolves the exact, non-Coding fixture dialogue Plan for one Agent Home.
pub fn starter_plan(home: &Path) -> Result<ResolvedAppPlan, String> {
    let catalog = starter_host_catalog(home)?;
    resolve_plugin_root(&catalog, &PluginRootSnapshot::default())
        .map(|app| app.plan().clone())
        .map_err(|error| format!("Dialogue starter composition cannot resolve: {error}"))
}

/// Returns the explicit Host Catalog used by [`starter_plan`].
pub fn starter_host_catalog(home: &Path) -> Result<HostCatalog, String> {
    NativePluginRegistry::host_catalog(starter_host_slots(), starter_defaults(home))
        .map_err(|error| format!("Dialogue starter Host Catalog is invalid: {error:?}"))
}

fn starter_host_slots() -> Vec<HostSlot> {
    [
        "agents",
        "artifact",
        "context-compactor",
        "memory",
        "model",
        "prompt-runtime",
        "session",
        "surfaces",
        "tool-providers",
        "tool-hooks",
        "tools-runtimes",
    ]
    .into_iter()
    .map(HostSlot::many)
    .collect()
}

fn starter_defaults(home: &Path) -> Vec<HostDefaultPlugin> {
    vec![
        configured_plugin(
            "lenso.agent.loop",
            "agent",
            serde_json::json!({
                "model": "fixture/readme-summary-v1",
                "tool_allowlist": [],
                "max_steps": 8,
                "max_tool_calls": 0,
                "max_parallel_tool_calls": 1,
                "max_output_tokens": 512,
                "max_history_events": 64,
                "max_compaction_summary_characters": 2048,
                "max_memory_items": 4,
                "max_memory_characters": 4096
            }),
        ),
        configured_plugin(
            "lenso.agent.artifact.file",
            "artifacts",
            serde_json::json!({
                "directory": home.join("artifacts"),
                "max_artifact_bytes": 1_048_576,
                "max_total_bytes": 16_777_216,
                "max_items": 128
            }),
        ),
        configured_plugin(
            "lenso.agent.context-compaction",
            "context-compactor",
            serde_json::json!({
                "max_input_characters": 262_144,
                "max_summary_characters": 2048,
                "retain_recent_turns": 4
            }),
        ),
        configured_plugin(
            "lenso.agent.memory.sqlite",
            "memory",
            serde_json::json!({
                "database": home.join("memory.sqlite3"),
                "scope": "dialogue-starter",
                "max_records": 1024,
                "max_item_characters": 4096,
                "max_recall_items": 4,
                "max_recall_characters": 4096
            }),
        ),
        configured_plugin(
            "lenso.agent.model.fixture",
            "model",
            serde_json::json!({"model": "fixture/readme-summary-v1"}),
        ),
        HostDefaultPlugin::new("lenso.agent.prompt", "prompt"),
        configured_plugin(
            "lenso.agent.session.sqlite",
            "sessions",
            serde_json::json!({"database": home.join("sessions.sqlite3")}),
        ),
        HostDefaultPlugin::new("lenso.agent.tools", "tools"),
        HostDefaultPlugin::new("lenso.agent.cli", "cli"),
    ]
}

fn configured_plugin(
    plugin_id: &str,
    instance_key: &str,
    configuration: serde_json::Value,
) -> HostDefaultPlugin {
    HostDefaultPlugin::new(plugin_id, instance_key).with_configuration(configuration)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{COMPOSITION_ID, starter_plan};

    #[test]
    fn selected_plan_has_only_the_explicit_non_coding_inventory() {
        let temporary = tempfile::tempdir().expect("temporary dialogue home");
        let plan = starter_plan(temporary.path()).expect("resolve dialogue starter");
        let selected = plan
            .plugin_instances()
            .iter()
            .map(|instance| instance.instance_key().to_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            selected,
            BTreeSet::from([
                "lenso.agent.artifact.file/artifacts".to_owned(),
                "lenso.agent.cli/cli".to_owned(),
                "lenso.agent.context-compaction/context-compactor".to_owned(),
                "lenso.agent.loop/agent".to_owned(),
                "lenso.agent.memory.sqlite/memory".to_owned(),
                "lenso.agent.model.fixture/model".to_owned(),
                "lenso.agent.prompt/prompt".to_owned(),
                "lenso.agent.session.sqlite/sessions".to_owned(),
                "lenso.agent.tools/tools".to_owned(),
            ])
        );
        assert_eq!(COMPOSITION_ID, "lenso.agent.dialogue-starter@1");
        assert!(
            plan.plugin_instances()
                .iter()
                .all(|instance| !instance.instance_key().contains("workspace"))
        );
    }
}
