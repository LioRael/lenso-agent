use lenso_agent_cli_plugin as _;
use lenso_agent_session_terminal_plugin as _;
use lenso_terminal_cli_plugin as _;
use lenso_terminal_command_plugin as _;

#[test]
fn headless_distribution_links_only_its_surface() {
    lenso_agent_default_plugins::link();
    let catalog =
        serde_json::to_value(lenso_agent_host::generation::linked_host_catalog()).unwrap();
    let catalog = catalog.to_string();
    assert!(catalog.contains(r#""plugin_id":"lenso.agent.cli""#));
    assert!(catalog.contains(r#""plugin_id":"lenso.terminal.cli""#));
    assert!(catalog.contains(r#""plugin_id":"lenso.terminal.command""#));
    assert!(catalog.contains(r#""plugin_id":"lenso.agent.session-terminal""#));
    for plugin_id in [
        "lenso.secrets.env",
        "lenso.secrets.keychain",
        "lenso.secrets.encrypted-file",
        "lenso.secrets.command",
    ] {
        assert!(
            catalog.contains(&format!(r#""plugin_id":"{plugin_id}""#)),
            "headless Host should link {plugin_id}"
        );
    }
    assert!(!catalog.contains(r#""plugin_id":"lenso.agent.tui""#));
    assert!(!catalog.contains(r#""plugin_id":"lenso.terminal.tui""#));
    assert!(!catalog.contains(r#""plugin_id":"lenso.agent.telegram""#));
    assert!(!catalog.contains(r#""plugin_id":"lenso.agent.discord""#));
}
