use std::{collections::BTreeSet, fs, path::Path};

use lenso_app_plan::authoring::{PluginInstanceId, PluginRootInstance, PluginRootSnapshot};

const MAX_PROFILE_BYTES: u64 = 256 * 1024;
const DEFAULT_AGENT: &str = "lenso.agent.loop/agent";

#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct ProfileDocument {
    #[serde(default)]
    #[serde(rename = "description")]
    pub description: String,
    #[serde(default = "default_agent")]
    pub agent: String,
    #[serde(default)]
    pub include_enabled: bool,
    pub instances: Vec<String>,
    #[serde(default)]
    pub excluded_instances: Vec<String>,
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(flatten)]
    pub extra: std::collections::BTreeMap<String, toml::Value>,
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

    let excluded = document
        .excluded_instances
        .iter()
        .map(|value| parse_instance_id(value, "excluded_instances"))
        .collect::<Result<BTreeSet<_>, _>>()?;
    if excluded.contains(&agent) {
        return Err("The selected Agent cannot be disabled".into());
    }
    if document.instructions.len() > 65536 {
        return Err("Profile instructions exceed 64 KiB".into());
    }
    if let Some(tools) = &document.allowed_tools {
        lenso_agent_loop_plugin::RunScope::new(tools.clone())?;
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

    let mut instances = root
        .instances()
        .iter()
        .filter(|instance| {
            !excluded.contains(instance.id())
                && (document.include_enabled || selected.contains(instance.id()))
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut disabled = root
        .disabled()
        .iter()
        .filter(|instance| {
            !selected.contains(*instance)
                && (document.include_enabled || !root_instances.contains(*instance))
        })
        .cloned()
        .collect::<Vec<_>>();
    disabled.extend(excluded);
    disabled.sort();
    disabled.dedup();
    if document.allowed_tools.is_some() || document.model.is_some() {
        if agent.plugin_id() != "lenso.agent.loop" {
            return Err("Tool and model defaults require a Lenso Agent Loop".into());
        }
        let existing = instances
            .iter()
            .position(|instance| instance.id() == &agent);
        let mut configuration = existing.map_or_else(
            || serde_json::json!({}),
            |index| instances[index].configuration().clone(),
        );
        if let Some(tools) = &document.allowed_tools {
            configuration["tool_allowlist"] = serde_json::json!(tools);
        }
        if let Some(model) = &document.model {
            configuration["model"] = serde_json::json!(model);
        }
        let item = PluginRootInstance::new(agent.plugin_id(), agent.instance_key())
            .with_configuration(configuration);
        if let Some(index) = existing {
            instances[index] = item;
        } else {
            instances.push(item);
        }
    }
    if let Some(value) = document.extra.get("approval_mode") {
        let mode = value
            .as_str()
            .filter(|mode| matches!(*mode, "request" | "assisted" | "full"))
            .ok_or_else(|| "Profile approval_mode must be request, assisted or full".to_owned())?;
        let hook = PluginInstanceId::new("lenso.agent.interactive-approval-hook", "default");
        if disabled.contains(&hook) {
            return Err("Approval mode requires the approval Hook".to_owned());
        }
        let existing = instances.iter().position(|instance| instance.id() == &hook);
        let mut configuration = existing.map_or_else(
            || serde_json::json!({}),
            |index| instances[index].configuration().clone(),
        );
        configuration["approval_mode"] = serde_json::json!(mode);
        let item = PluginRootInstance::new("lenso.agent.interactive-approval-hook", "default")
            .with_configuration(configuration);
        if let Some(index) = existing {
            instances[index] = item;
        } else {
            instances.push(item);
        }
    }
    if !document.instructions.trim().is_empty() {
        if instances.iter().any(|instance| {
            instance.id().to_string() == "lenso.agent.prompt.static/profile-instructions"
        }) {
            return Err(
                "Profile instructions conflict with the reserved profile-instructions instance"
                    .into(),
            );
        }
        instances.push(PluginRootInstance::new("lenso.agent.prompt.static", "profile-instructions").with_configuration(serde_json::json!({
            "contributions": [{"id":"profile.instructions", "version":"1", "kind":"instruction", "content":document.instructions}]
        })));
    }
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

pub fn validate_profile_name(name: &str) -> Result<(), String> {
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
    fn approval_modes_materialize_and_reject_invalid_values() {
        let root = PluginRootSnapshot::new(
            [],
            [
                instance("lenso.agent.interactive-approval-hook", "default").with_configuration(
                    serde_json::json!({"default_decision":"ask","max_preview_bytes":1024}),
                ),
            ],
            [],
        );
        for mode in ["request", "assisted", "full"] {
            let document: ProfileDocument = serde_json::from_value(
                serde_json::json!({"approval_mode":mode,"instances":[],"include_enabled":true}),
            )
            .unwrap();
            let selected = apply("test", &document, &root).unwrap();
            let hook = selected
                .root()
                .instances()
                .iter()
                .find(|item| {
                    item.id().to_string() == "lenso.agent.interactive-approval-hook/default"
                })
                .unwrap();
            assert_eq!(hook.configuration()["approval_mode"], mode);
            assert_eq!(hook.configuration()["default_decision"], "ask");
        }
        let document: ProfileDocument = serde_json::from_value(
            serde_json::json!({"approval_mode":"unknown","instances":[],"include_enabled":true}),
        )
        .unwrap();
        assert!(apply("test", &document, &root).is_err());
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
                excluded_instances: vec![],
                allowed_tools: None,
                instructions: String::new(),
                model: None,
                extra: Default::default(),
                description: "Game agent".to_owned(),
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
                excluded_instances: vec![],
                allowed_tools: None,
                instructions: String::new(),
                model: None,
                extra: Default::default(),
                description: String::new(),
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
                excluded_instances: vec![],
                allowed_tools: None,
                instructions: String::new(),
                model: None,
                extra: Default::default(),
                description: String::new(),
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
                excluded_instances: vec![],
                allowed_tools: None,
                instructions: String::new(),
                model: None,
                extra: Default::default(),
                description: String::new(),
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
                excluded_instances: vec![],
                allowed_tools: None,
                instructions: String::new(),
                model: None,
                extra: Default::default(),
                description: String::new(),
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
                excluded_instances: vec![],
                allowed_tools: None,
                instructions: String::new(),
                model: None,
                extra: Default::default(),
                description: String::new(),
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
