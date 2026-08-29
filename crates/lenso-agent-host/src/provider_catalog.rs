use std::collections::{BTreeMap, BTreeSet};

use lenso_app_plan::{ResolvedAppPlan, authoring::HostCatalog};
use serde::{Deserialize, Serialize};

const MODEL_CAPABILITY: &str = "lenso.agent.model@2";
const FIXTURE_PLUGIN: &str = "lenso.agent.model.fixture";
const OPENAI_COMPATIBLE_PLUGIN: &str = "lenso.agent.model.openai-compatible";
const CODEX_DIRECT_PLUGIN: &str = "lenso.agent.model.openai-codex-direct";

/// Read-only Provider and Model metadata derived from one Host build and Plan.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderModelCatalog {
    pub schema: String,
    pub providers: Vec<ModelProviderCatalogEntry>,
}

/// One Model Provider available in the immutable Host Catalog.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelProviderCatalogEntry {
    pub provider_id: String,
    pub name: String,
    pub plugin_id: String,
    pub authentication: ModelAuthentication,
    pub capabilities: ModelCapabilities,
    pub available_instances: Vec<String>,
    pub selected_instance: Option<String>,
    pub models: Vec<ModelCatalogEntry>,
}

/// Non-secret authentication metadata for a Model Provider.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case", tag = "kind")]
pub enum ModelAuthentication {
    None,
    SecretReference {
        capability_id: String,
    },
    #[serde(rename = "oauth")]
    OAuth {
        method_id: String,
        interactive: bool,
    },
}

/// Model protocol features implemented by the selected Provider adapter.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent provider feature flags are serialized compatibility metadata"
)]
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelCapabilities {
    pub text_input: bool,
    pub text_output: bool,
    pub tool_calls: bool,
    pub parallel_tool_calls: bool,
    pub reasoning_summary: bool,
    pub image_input: bool,
    pub audio_input: bool,
}

/// One configured primary or auxiliary model identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelCatalogEntry {
    pub id: String,
    pub selected: bool,
}

#[derive(Clone, Copy)]
struct ProviderDefinition {
    provider_id: &'static str,
    name: &'static str,
    plugin_id: &'static str,
    fallback_model: Option<&'static str>,
}

const PROVIDERS: [ProviderDefinition; 3] = [
    ProviderDefinition {
        provider_id: "chatgpt",
        name: "ChatGPT",
        plugin_id: CODEX_DIRECT_PLUGIN,
        fallback_model: None,
    },
    ProviderDefinition {
        provider_id: "openai-compatible",
        name: "OpenAI-compatible",
        plugin_id: OPENAI_COMPATIBLE_PLUGIN,
        fallback_model: None,
    },
    ProviderDefinition {
        provider_id: "fixture",
        name: "Fixture",
        plugin_id: FIXTURE_PLUGIN,
        fallback_model: Some("fixture/readme-summary-v1"),
    },
];

pub(crate) fn project(
    host: &HostCatalog,
    plan: &ResolvedAppPlan,
) -> Result<ProviderModelCatalog, String> {
    let selected_instance = selected_model_instance(plan)?;
    let selected_model = selected_instance
        .as_deref()
        .and_then(|instance| {
            plan.plugin_instances()
                .iter()
                .find(|item| item.instance_key() == instance)
        })
        .map(|instance| configuration_models(instance.configuration()))
        .transpose()?
        .and_then(|models| models.into_iter().next());
    let available_plugins = host
        .plugins()
        .iter()
        .map(|release| release.descriptor().plugin_id())
        .collect::<BTreeSet<_>>();
    let mut providers = Vec::new();
    for definition in PROVIDERS {
        if !available_plugins.contains(definition.plugin_id) {
            continue;
        }
        let mut instances = BTreeMap::<String, Vec<String>>::new();
        for item in host
            .defaults()
            .iter()
            .filter(|item| item.id().plugin_id() == definition.plugin_id)
        {
            instances.insert(
                item.id().to_string(),
                configuration_models_value(item.configuration())?,
            );
        }
        for item in host
            .configurations()
            .iter()
            .filter(|item| item.id().plugin_id() == definition.plugin_id)
        {
            instances.insert(
                item.id().to_string(),
                configuration_models_value(item.configuration())?,
            );
        }
        if let Some(instance) = selected_instance.as_deref().filter(|instance| {
            plan.plugin_instances().iter().any(|item| {
                item.instance_key() == *instance && item.package_id() == definition.plugin_id
            })
        }) {
            let selected = plan
                .plugin_instances()
                .iter()
                .find(|item| item.instance_key() == instance)
                .expect("selected Model Instance was checked");
            instances.insert(
                instance.to_owned(),
                configuration_models(selected.configuration())?,
            );
        }
        let mut model_ids = instances
            .values()
            .flatten()
            .cloned()
            .collect::<BTreeSet<_>>();
        if let Some(model) = definition.fallback_model {
            model_ids.insert(model.to_owned());
        }
        providers.push(ModelProviderCatalogEntry {
            provider_id: definition.provider_id.to_owned(),
            name: definition.name.to_owned(),
            plugin_id: definition.plugin_id.to_owned(),
            authentication: authentication(definition.plugin_id),
            capabilities: model_capabilities(),
            available_instances: instances.into_keys().collect(),
            selected_instance: selected_instance.clone().filter(|instance| {
                plan.plugin_instances().iter().any(|item| {
                    item.instance_key() == instance && item.package_id() == definition.plugin_id
                })
            }),
            models: model_ids
                .into_iter()
                .map(|id| ModelCatalogEntry {
                    selected: selected_model.as_deref() == Some(id.as_str()),
                    id,
                })
                .collect(),
        });
    }
    Ok(ProviderModelCatalog {
        schema: "lenso.agent.provider-model-catalog.v1".to_owned(),
        providers,
    })
}

fn selected_model_instance(plan: &ResolvedAppPlan) -> Result<Option<String>, String> {
    let providers = plan
        .capability_bindings()
        .iter()
        .filter(|binding| binding.capability_id() == MODEL_CAPABILITY)
        .map(lenso_app_plan::CapabilityBinding::provider_instance)
        .collect::<BTreeSet<_>>();
    match providers.len() {
        0 => Ok(None),
        1 => Ok(providers.into_iter().next().map(ToOwned::to_owned)),
        _ => Err("resolved App selects more than one Model Provider Instance".to_owned()),
    }
}

fn configuration_models(configuration: &str) -> Result<Vec<String>, String> {
    let value = serde_json::from_str(configuration)
        .map_err(|error| format!("selected Model configuration is invalid JSON: {error}"))?;
    configuration_models_value(&value)
}

fn configuration_models_value(configuration: &serde_json::Value) -> Result<Vec<String>, String> {
    let object = configuration
        .as_object()
        .ok_or_else(|| "Model configuration must be an object".to_owned())?;
    let mut models = Vec::new();
    if let Some(model) = object.get("model") {
        models.push(
            model
                .as_str()
                .filter(|model| !model.is_empty())
                .ok_or_else(|| "Model configuration has an invalid model identity".to_owned())?
                .to_owned(),
        );
    }
    if let Some(allowed) = object.get("allowed_models") {
        let allowed = allowed
            .as_array()
            .ok_or_else(|| "Model configuration allowed_models must be an array".to_owned())?;
        for model in allowed {
            models.push(
                model
                    .as_str()
                    .filter(|model| !model.is_empty())
                    .ok_or_else(|| {
                        "Model configuration has an invalid auxiliary model identity".to_owned()
                    })?
                    .to_owned(),
            );
        }
    }
    Ok(models)
}

fn authentication(plugin_id: &str) -> ModelAuthentication {
    match plugin_id {
        CODEX_DIRECT_PLUGIN => ModelAuthentication::OAuth {
            method_id: "chatgpt".to_owned(),
            interactive: true,
        },
        OPENAI_COMPATIBLE_PLUGIN => ModelAuthentication::SecretReference {
            capability_id: "lenso.secrets@1".to_owned(),
        },
        _ => ModelAuthentication::None,
    }
}

const fn model_capabilities() -> ModelCapabilities {
    ModelCapabilities {
        text_input: true,
        text_output: true,
        tool_calls: true,
        parallel_tool_calls: true,
        reasoning_summary: true,
        image_input: false,
        audio_input: false,
    }
}
