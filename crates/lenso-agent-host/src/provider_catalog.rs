use std::collections::{BTreeMap, BTreeSet};

pub use lenso_agent_loop_plugin::{
    ModelCapabilities, ModelInputModality, ModelLimits, ModelReasoningControl,
    ModelServiceTierControl, ModelWireProtocol, ResolvedTurnProfile,
};
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
    pub catalog_revision: String,
    pub resolved_turn_profile: Option<ResolvedTurnProfile>,
    pub providers: Vec<ModelProviderCatalogEntry>,
}

impl ProviderModelCatalog {
    /// Resolves a model admitted by the already-selected Provider Instance.
    pub fn resolve_model(&self, model_id: &str) -> Result<ResolvedTurnProfile, String> {
        self.resolve_model_options(model_id, None, None)
    }

    /// Resolves one admitted model plus optional per-Turn inference controls.
    pub fn resolve_model_options(
        &self,
        model_id: &str,
        reasoning_effort: Option<&str>,
        service_tier: Option<&str>,
    ) -> Result<ResolvedTurnProfile, String> {
        let current = self
            .resolved_turn_profile
            .as_ref()
            .ok_or_else(|| "active Generation has no selected model profile".to_owned())?;
        let provider = self
            .providers
            .iter()
            .find(|provider| {
                provider.selected_instance.as_deref() == Some(current.provider_instance.as_str())
            })
            .ok_or_else(|| "selected Model Provider is absent from the catalog".to_owned())?;
        let model = provider
            .models
            .iter()
            .find(|model| model.id == model_id)
            .ok_or_else(|| {
                format!(
                    "model `{model_id}` is not admitted by Provider Instance `{}`",
                    current.provider_instance
                )
            })?;
        let reasoning_effort = reasoning_effort.map(str::to_owned).or_else(|| {
            model_supports_reasoning(model, current.reasoning_effort.as_deref())
                .then(|| current.reasoning_effort.clone())
                .flatten()
        });
        let service_tier = service_tier.map(str::to_owned).or_else(|| {
            model_supports_service_tier(model, current.service_tier.as_deref())
                .then(|| current.service_tier.clone())
                .flatten()
        });
        validate_selected_variant(model, reasoning_effort.as_deref(), service_tier.as_deref())?;
        Ok(ResolvedTurnProfile {
            catalog_revision: self.catalog_revision.clone(),
            provider_id: current.provider_id.clone(),
            provider_instance: current.provider_instance.clone(),
            model: model.id.clone(),
            reasoning_effort,
            service_tier,
            limits: model.limits.clone(),
            capabilities: model.capabilities.clone(),
            wire_protocol: model.wire_protocol,
            compaction_compatibility: model.compaction_compatibility.clone(),
        })
    }

    pub fn selected_provider_models(&self) -> Vec<String> {
        let Some(current) = self.resolved_turn_profile.as_ref() else {
            return Vec::new();
        };
        self.providers
            .iter()
            .find(|provider| {
                provider.selected_instance.as_deref() == Some(current.provider_instance.as_str())
            })
            .map(|provider| {
                provider
                    .models
                    .iter()
                    .map(|model| model.id.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn model_supports_reasoning(model: &ModelCatalogEntry, effort: Option<&str>) -> bool {
    matches!(
        (&model.capabilities.reasoning, effort),
        (ModelReasoningControl::Selectable { efforts }, Some(effort))
            if efforts.iter().any(|candidate| candidate == effort)
    )
}

fn model_supports_service_tier(model: &ModelCatalogEntry, tier: Option<&str>) -> bool {
    matches!(
        (&model.capabilities.service_tiers, tier),
        (ModelServiceTierControl::Selectable { tiers }, Some(tier))
            if tiers.iter().any(|candidate| candidate == tier)
    )
}

/// One Model Provider available in the immutable Host Catalog.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelProviderCatalogEntry {
    pub provider_id: String,
    pub name: String,
    pub plugin_id: String,
    pub authentication: ModelAuthentication,
    pub readiness: ModelProviderReadiness,
    pub available_instances: Vec<String>,
    pub selected_instance: Option<String>,
    pub models: Vec<ModelCatalogEntry>,
}

/// Authentication/catalog readiness without activating an unselected Provider.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelProviderReadiness {
    pub status: ModelProviderReadinessStatus,
    pub detail: String,
}

/// A catalog projection cannot claim credential readiness without consulting the Provider.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelProviderReadinessStatus {
    Unchecked,
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

/// One configured primary or auxiliary model identity.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelCatalogEntry {
    pub id: String,
    pub selected: bool,
    pub limits: ModelLimits,
    pub capabilities: ModelCapabilities,
    pub wire_protocol: ModelWireProtocol,
    pub compaction_compatibility: String,
}

#[derive(Clone, Copy)]
struct ProviderDefinition {
    provider_id: &'static str,
    name: &'static str,
    plugin_id: &'static str,
    fallback_model: Option<&'static str>,
    flavor: ProviderFlavor,
}

#[derive(Clone, Copy)]
enum ProviderFlavor {
    Any,
    GenericOpenAiCompatible,
    OpenRouter,
}

const PROVIDERS: [ProviderDefinition; 4] = [
    ProviderDefinition {
        provider_id: "chatgpt",
        name: "ChatGPT",
        plugin_id: CODEX_DIRECT_PLUGIN,
        fallback_model: None,
        flavor: ProviderFlavor::Any,
    },
    ProviderDefinition {
        provider_id: "openrouter",
        name: "OpenRouter",
        plugin_id: OPENAI_COMPATIBLE_PLUGIN,
        fallback_model: None,
        flavor: ProviderFlavor::OpenRouter,
    },
    ProviderDefinition {
        provider_id: "openai-compatible",
        name: "OpenAI-compatible",
        plugin_id: OPENAI_COMPATIBLE_PLUGIN,
        fallback_model: None,
        flavor: ProviderFlavor::GenericOpenAiCompatible,
    },
    ProviderDefinition {
        provider_id: "fixture",
        name: "Fixture",
        plugin_id: FIXTURE_PLUGIN,
        fallback_model: Some("fixture/readme-summary-v1"),
        flavor: ProviderFlavor::Any,
    },
];

pub(crate) fn project(
    host: &HostCatalog,
    plan: &ResolvedAppPlan,
    catalog_revision: &str,
) -> Result<ProviderModelCatalog, String> {
    let selected_instance = selected_model_instance(plan)?;
    let selected_configuration = selected_instance.as_deref().and_then(|instance| {
        plan.plugin_instances()
            .iter()
            .find(|item| item.instance_key() == instance)
    });
    let selected_model = selected_configuration
        .map(|instance| configuration_models(instance.configuration()))
        .transpose()?
        .and_then(|models| models.into_iter().next());
    let selected_reasoning_effort = selected_configuration
        .map(|instance| configuration_string(instance.configuration(), "reasoning_effort"))
        .transpose()?
        .flatten();
    let selected_service_tier = selected_configuration
        .map(|instance| configuration_string(instance.configuration(), "service_tier"))
        .transpose()?
        .flatten();
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
        providers.push(project_provider(
            definition,
            host,
            plan,
            selected_instance.as_deref(),
            selected_model.as_deref(),
        )?);
    }
    let resolved_turn_profile = resolved_turn_profile(
        &providers,
        selected_instance.as_deref(),
        selected_model.as_deref(),
        selected_reasoning_effort,
        selected_service_tier,
        catalog_revision,
    )?;
    Ok(ProviderModelCatalog {
        schema: "lenso.agent.provider-model-catalog.v2".to_owned(),
        catalog_revision: catalog_revision.to_owned(),
        resolved_turn_profile,
        providers,
    })
}

fn project_provider(
    definition: ProviderDefinition,
    host: &HostCatalog,
    plan: &ResolvedAppPlan,
    selected_instance: Option<&str>,
    selected_model: Option<&str>,
) -> Result<ModelProviderCatalogEntry, String> {
    let mut instances = BTreeMap::<String, Vec<String>>::new();
    for item in host
        .defaults()
        .iter()
        .filter(|item| item.id().plugin_id() == definition.plugin_id)
        .filter(|item| definition_matches_configuration(definition, item.configuration()))
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
        .filter(|item| definition_matches_configuration(definition, item.configuration()))
    {
        instances.insert(
            item.id().to_string(),
            configuration_models_value(item.configuration())?,
        );
    }
    let selected_instance = selected_instance.filter(|instance| {
        plan.plugin_instances().iter().any(|item| {
            item.instance_key() == *instance
                && item.package_id() == definition.plugin_id
                && serde_json::from_str::<serde_json::Value>(item.configuration()).is_ok_and(
                    |configuration| definition_matches_configuration(definition, &configuration),
                )
        })
    });
    if let Some(instance) = selected_instance {
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
    Ok(ModelProviderCatalogEntry {
        provider_id: definition.provider_id.to_owned(),
        name: definition.name.to_owned(),
        plugin_id: definition.plugin_id.to_owned(),
        authentication: authentication(definition.plugin_id),
        readiness: ModelProviderReadiness {
            status: ModelProviderReadinessStatus::Unchecked,
            detail: "read-only projection does not activate the Provider or inspect authentication state"
                .to_owned(),
        },
        available_instances: instances.into_keys().collect(),
        selected_instance: selected_instance.map(str::to_owned),
        models: model_ids
            .into_iter()
            .map(|id| ModelCatalogEntry {
                selected: selected_model == Some(id.as_str()),
                limits: model_limits(definition.plugin_id),
                capabilities: model_capabilities(definition.plugin_id, &id),
                wire_protocol: wire_protocol(definition.plugin_id),
                compaction_compatibility: "generic-text-v1".to_owned(),
                id,
            })
            .collect(),
    })
}

fn definition_matches_configuration(
    definition: ProviderDefinition,
    configuration: &serde_json::Value,
) -> bool {
    let openrouter = configuration
        .get("base_url")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|base_url| {
            reqwest::Url::parse(base_url)
                .is_ok_and(|url| url.scheme() == "https" && url.host_str() == Some("openrouter.ai"))
        });
    match definition.flavor {
        ProviderFlavor::Any => true,
        ProviderFlavor::GenericOpenAiCompatible => !openrouter,
        ProviderFlavor::OpenRouter => openrouter,
    }
}

fn resolved_turn_profile(
    providers: &[ModelProviderCatalogEntry],
    selected_instance: Option<&str>,
    selected_model: Option<&str>,
    reasoning_effort: Option<String>,
    service_tier: Option<String>,
    catalog_revision: &str,
) -> Result<Option<ResolvedTurnProfile>, String> {
    let (Some(selected_instance), Some(selected_model)) = (selected_instance, selected_model)
    else {
        return Ok(None);
    };
    let provider = providers
        .iter()
        .find(|provider| provider.selected_instance.as_deref() == Some(selected_instance))
        .ok_or_else(|| "selected Model Provider is absent from the Host catalog".to_owned())?;
    let model = provider
        .models
        .iter()
        .find(|model| model.id == selected_model)
        .ok_or_else(|| "selected model is absent from its Provider catalog".to_owned())?;
    validate_selected_variant(model, reasoning_effort.as_deref(), service_tier.as_deref())?;
    Ok(Some(ResolvedTurnProfile {
        catalog_revision: catalog_revision.to_owned(),
        provider_id: provider.provider_id.clone(),
        provider_instance: selected_instance.to_owned(),
        model: selected_model.to_owned(),
        reasoning_effort,
        service_tier,
        limits: model.limits.clone(),
        capabilities: model.capabilities.clone(),
        wire_protocol: model.wire_protocol,
        compaction_compatibility: model.compaction_compatibility.clone(),
    }))
}

fn validate_selected_variant(
    model: &ModelCatalogEntry,
    reasoning_effort: Option<&str>,
    service_tier: Option<&str>,
) -> Result<(), String> {
    match (&model.capabilities.reasoning, reasoning_effort) {
        (ModelReasoningControl::Selectable { efforts }, Some(selected))
            if efforts.iter().any(|effort| effort == selected) => {}
        (_, None) => {}
        _ => {
            return Err(
                "selected reasoning effort is not admitted by the model catalog".to_owned(),
            );
        }
    }
    match (&model.capabilities.service_tiers, service_tier) {
        (ModelServiceTierControl::Selectable { tiers }, Some(selected))
            if tiers.iter().any(|tier| tier == selected) => {}
        (_, None) => {}
        _ => return Err("selected service tier is not admitted by the model catalog".to_owned()),
    }
    Ok(())
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

fn configuration_string(configuration: &str, key: &str) -> Result<Option<String>, String> {
    let value = serde_json::from_str::<serde_json::Value>(configuration)
        .map_err(|error| format!("selected Model configuration is invalid JSON: {error}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| "Model configuration must be an object".to_owned())?;
    object
        .get(key)
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .ok_or_else(|| format!("Model configuration has an invalid {key}"))
        })
        .transpose()
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

fn model_limits(plugin_id: &str) -> ModelLimits {
    if plugin_id == FIXTURE_PLUGIN {
        ModelLimits {
            context_window_tokens: Some(32_768),
            max_input_tokens: Some(28_672),
            max_output_tokens: Some(4_096),
        }
    } else {
        ModelLimits {
            context_window_tokens: None,
            max_input_tokens: None,
            max_output_tokens: None,
        }
    }
}

fn model_capabilities(plugin_id: &str, model_id: &str) -> ModelCapabilities {
    ModelCapabilities {
        input_modalities: vec![ModelInputModality::Text],
        text_output: true,
        tool_calls: true,
        parallel_tool_calls: true,
        reasoning: if plugin_id == CODEX_DIRECT_PLUGIN && model_id.starts_with("gpt-5") {
            ModelReasoningControl::Selectable {
                efforts: (match model_id {
                    "gpt-5.6-sol" => {
                        vec!["low", "medium", "high", "xhigh", "max", "ultra"]
                    }
                    "gpt-5.6-luna" => vec!["low", "medium", "high", "xhigh", "max"],
                    _ => vec!["low", "medium", "high", "xhigh"],
                })
                .into_iter()
                .map(str::to_owned)
                .collect(),
            }
        } else {
            ModelReasoningControl::Unsupported
        },
        service_tiers: if plugin_id == CODEX_DIRECT_PLUGIN && model_id == "gpt-5.6-sol" {
            ModelServiceTierControl::Selectable {
                tiers: vec!["fast".to_owned()],
            }
        } else {
            ModelServiceTierControl::Unsupported
        },
    }
}

fn wire_protocol(plugin_id: &str) -> ModelWireProtocol {
    match plugin_id {
        CODEX_DIRECT_PLUGIN => ModelWireProtocol::OpenaiResponses,
        OPENAI_COMPATIBLE_PLUGIN => ModelWireProtocol::OpenaiChatCompletions,
        _ => ModelWireProtocol::Fixture,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_catalog_exposes_openrouter_as_a_distinct_provider() {
        let openrouter = PROVIDERS
            .iter()
            .find(|provider| provider.provider_id == "openrouter")
            .unwrap();
        assert_eq!(openrouter.plugin_id, OPENAI_COMPATIBLE_PLUGIN);
        assert!(matches!(openrouter.flavor, ProviderFlavor::OpenRouter));
        assert!(PROVIDERS.iter().any(|provider| {
            provider.provider_id == "openai-compatible"
                && matches!(provider.flavor, ProviderFlavor::GenericOpenAiCompatible)
        }));
    }

    #[test]
    fn direct_model_controls_are_model_specific() {
        let sol = model_capabilities(CODEX_DIRECT_PLUGIN, "gpt-5.6-sol");
        assert!(matches!(
            sol.service_tiers,
            ModelServiceTierControl::Selectable { ref tiers } if tiers == &["fast"]
        ));
        assert!(matches!(
            sol.reasoning,
            ModelReasoningControl::Selectable { ref efforts }
                if efforts.last().map(String::as_str) == Some("ultra")
        ));

        let luna = model_capabilities(CODEX_DIRECT_PLUGIN, "gpt-5.6-luna");
        assert!(matches!(
            luna.service_tiers,
            ModelServiceTierControl::Unsupported
        ));
        assert!(matches!(
            luna.reasoning,
            ModelReasoningControl::Selectable { ref efforts }
                if efforts.last().map(String::as_str) == Some("max")
        ));
    }
}
