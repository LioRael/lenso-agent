use std::{collections::BTreeSet, fs, path::Path};

use lenso_app_plan::authoring::{PluginInstanceId, PluginRootInstance, PluginRootSnapshot};

const MAX_PROFILE_BYTES: u64 = 256 * 1024;
const DEFAULT_AGENT: &str = "lenso.agent.loop/agent";

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileDocument {
    #[serde(default)]
    #[serde(rename = "description")]
    _description: String,
    #[serde(default = "default_agent")]
    agent: String,
    #[serde(default)]
    include_enabled: bool,
    instances: Vec<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct SelectedProfile {
    agent: PluginInstanceId,
    root: PluginRootSnapshot,
}

impl SelectedProfile {
    pub(crate) const fn agent(&self) -> &PluginInstanceId {
        &self.agent
    }

    pub(crate) const fn root(&self) -> &PluginRootSnapshot {
        &self.root
    }
}

pub(crate) fn select(
    name: &str,
    root: &PluginRootSnapshot,
    profile_directory: &Path,
) -> Result<SelectedProfile, String> {
    validate_profile_name(name)?;
    let path = profile_directory.join(format!("{name}.toml"));
    let metadata = fs::metadata(&path).map_err(|error| {
        format!(
            "failed to inspect Agent Profile {}: {error}",
            path.display()
        )
    })?;
    if !metadata.is_file() || metadata.len() > MAX_PROFILE_BYTES {
        return Err(format!(
            "Agent Profile must be a regular TOML file no larger than 256 KiB: {}",
            path.display()
        ));
    }
    let source = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read Agent Profile {}: {error}", path.display()))?;
    let document: ProfileDocument = toml::from_str(&source)
        .map_err(|error| format!("invalid Agent Profile {}: {error}", path.display()))?;
    apply(name, &document, root)
}

fn apply(
    name: &str,
    document: &ProfileDocument,
    root: &PluginRootSnapshot,
) -> Result<SelectedProfile, String> {
    let agent = parse_instance_id(&document.agent, "agent")?;
    let mut selected = document
        .instances
        .iter()
        .map(|instance| parse_instance_id(instance, "instances"))
        .collect::<Result<BTreeSet<_>, _>>()?;
    if document.instances.len() != selected.len() {
        return Err(format!(
            "Agent Profile `{name}` contains a duplicate Plugin Instance"
        ));
    }

    let root_instances = root
        .instances()
        .iter()
        .map(PluginRootInstance::id)
        .cloned()
        .collect::<BTreeSet<_>>();
    if agent.to_string() != DEFAULT_AGENT || root_instances.contains(&agent) {
        selected.insert(agent.clone());
    }
    if let Some(missing) = selected
        .iter()
        .find(|instance| !root_instances.contains(*instance))
    {
        return Err(format!(
            "Agent Profile `{name}` selects Plugin Instance `{missing}`, but its configuration is missing from `plugins/`"
        ));
    }

    let instances = root
        .instances()
        .iter()
        .filter(|instance| document.include_enabled || selected.contains(instance.id()))
        .cloned()
        .collect::<Vec<_>>();
    let disabled = root
        .disabled()
        .iter()
        .filter(|instance| {
            !selected.contains(*instance)
                && (document.include_enabled || !root_instances.contains(*instance))
        })
        .cloned()
        .collect::<Vec<_>>();
    Ok(SelectedProfile {
        agent,
        root: PluginRootSnapshot::new(root.releases().iter().cloned(), instances, disabled),
    })
}

fn parse_instance_id(value: &str, field: &str) -> Result<PluginInstanceId, String> {
    let Some((plugin_id, instance_key)) = value.split_once('/') else {
        return Err(format!(
            "Agent Profile `{field}` entry `{value}` must be `plugin-id/instance`"
        ));
    };
    if plugin_id.is_empty()
        || instance_key.is_empty()
        || instance_key.contains('/')
        || !valid_identity(plugin_id)
        || !valid_identity(instance_key)
    {
        return Err(format!("invalid Agent Profile `{field}` entry `{value}`"));
    }
    Ok(PluginInstanceId::new(plugin_id, instance_key))
}

fn validate_profile_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name.len() > 64
        || !name.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || (index > 0 && matches!(byte, b'-' | b'_'))
        })
    {
        return Err(format!("invalid Agent Profile name `{name}`"));
    }
    Ok(())
}

fn valid_identity(value: &str) -> bool {
    value.trim() == value && value != "." && value != ".." && !value.contains(['/', '\\', '\0'])
}

fn default_agent() -> String {
    DEFAULT_AGENT.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instance(plugin_id: &str, instance_key: &str) -> PluginRootInstance {
        PluginRootInstance::new(plugin_id, instance_key)
    }

    #[test]
    fn profile_enables_declared_instances_alongside_global_instances() {
        let root = PluginRootSnapshot::new(
            [],
            [
                instance("example.code-tools", "code"),
                instance("example.game-loop", "game"),
                instance("example.global", "default"),
                instance("example.other-profile", "default"),
            ],
            [
                PluginInstanceId::new("example.code-tools", "code"),
                PluginInstanceId::new("example.game-loop", "game"),
                PluginInstanceId::new("example.other-profile", "default"),
            ],
        );
        let selected = apply(
            "game",
            &ProfileDocument {
                _description: "Game agent".to_owned(),
                agent: "example.game-loop/game".to_owned(),
                include_enabled: true,
                instances: vec!["example.code-tools/code".to_owned()],
            },
            &root,
        )
        .unwrap();

        assert_eq!(selected.agent().to_string(), "example.game-loop/game");
        assert_eq!(
            selected
                .root()
                .instances()
                .iter()
                .map(|instance| instance.id().to_string())
                .collect::<Vec<_>>(),
            [
                "example.code-tools/code",
                "example.game-loop/game",
                "example.global/default",
                "example.other-profile/default",
            ]
        );
        assert_eq!(
            selected
                .root()
                .disabled()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["example.other-profile/default"]
        );
    }

    #[test]
    fn profile_uses_the_host_default_agent_without_reconfiguring_it() {
        let root = PluginRootSnapshot::new(
            [],
            [instance("example.code-tools", "code")],
            [PluginInstanceId::new("example.code-tools", "code")],
        );
        let selected = apply(
            "code",
            &ProfileDocument {
                _description: String::new(),
                agent: default_agent(),
                include_enabled: false,
                instances: vec!["example.code-tools/code".to_owned()],
            },
            &root,
        )
        .unwrap();

        assert_eq!(selected.agent().to_string(), DEFAULT_AGENT);
        assert_eq!(selected.root().instances().len(), 1);
    }

    #[test]
    fn profiles_select_distinct_configurations_of_the_same_plugin() {
        let root = PluginRootSnapshot::new(
            [],
            [
                instance("lenso.secrets.keychain", "code").with_configuration(serde_json::json!({
                    "service": "com.lenso.agent.code",
                    "references": {"model/openai-api-key": "code-openai"}
                })),
                instance("lenso.secrets.keychain", "game").with_configuration(serde_json::json!({
                    "service": "com.lenso.agent.game",
                    "references": {"model/openai-api-key": "game-openai"}
                })),
            ],
            [
                PluginInstanceId::new("lenso.secrets.keychain", "code"),
                PluginInstanceId::new("lenso.secrets.keychain", "game"),
            ],
        );
        let code = apply(
            "code",
            &ProfileDocument {
                _description: String::new(),
                agent: default_agent(),
                include_enabled: false,
                instances: vec!["lenso.secrets.keychain/code".to_owned()],
            },
            &root,
        )
        .unwrap();
        let game = apply(
            "game",
            &ProfileDocument {
                _description: String::new(),
                agent: default_agent(),
                include_enabled: false,
                instances: vec!["lenso.secrets.keychain/game".to_owned()],
            },
            &root,
        )
        .unwrap();

        assert_eq!(
            code.root().instances()[0].configuration()["service"],
            "com.lenso.agent.code"
        );
        assert_eq!(
            game.root().instances()[0].configuration()["service"],
            "com.lenso.agent.game"
        );
    }

    #[test]
    fn profile_rejects_missing_and_duplicate_instances() {
        let root = PluginRootSnapshot::default();
        let missing = apply(
            "code",
            &ProfileDocument {
                _description: String::new(),
                agent: default_agent(),
                include_enabled: false,
                instances: vec!["example.tools/code".to_owned()],
            },
            &root,
        )
        .unwrap_err();
        assert!(missing.contains("configuration is missing"));

        let duplicate = apply(
            "code",
            &ProfileDocument {
                _description: String::new(),
                agent: default_agent(),
                include_enabled: false,
                instances: vec![
                    "example.tools/code".to_owned(),
                    "example.tools/code".to_owned(),
                ],
            },
            &root,
        )
        .unwrap_err();
        assert!(duplicate.contains("duplicate"));
    }
}
