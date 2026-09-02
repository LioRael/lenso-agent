use std::collections::{BTreeMap, BTreeSet};

pub use lenso_agent_loop_plugin::{
    ModelCapabilities, ModelCatalogFreshness, ModelCatalogProvenance, ModelCatalogSource,
    ModelControlOption, ModelInputModality, ModelLimits, ModelReasoningControl,
    ModelServiceTierControl, ModelWireProtocol, ResolvedTurnProfile,
};
use lenso_app_plan::{ResolvedAppPlan, authoring::HostCatalog};
use lenso_capability_agent_model as model_contract;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

const MODEL_CAPABILITY: &str = "lenso.agent.model@4";
const MAX_CATALOG_STALE_SECONDS: u64 = 7 * 24 * 60 * 60;
const FIXTURE_PLUGIN: &str = "lenso.agent.model.fixture";
const OPENAI_COMPATIBLE_PLUGIN: &str = "lenso.agent.model.openai-compatible";
const CODEX_DIRECT_PLUGIN: &str = "lenso.agent.model.openai-codex-direct";

/// Read-only Provider and Model metadata derived from one Host build and Plan.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderModelCatalog {
    pub schema: String,
    pub catalog_revision: String,
    pub catalog_provenance: Option<ModelCatalogProvenance>,
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
        self.resolve_model_controls(model_id, reasoning_effort, None, None, service_tier)
    }

    /// Resolves one admitted model plus one typed reasoning selection.
    pub fn resolve_model_controls(
        &self,
        model_id: &str,
        reasoning_effort: Option<&str>,
        reasoning_enabled: Option<bool>,
        reasoning_budget_tokens: Option<u64>,
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
        let explicit_reasoning = usize::from(reasoning_effort.is_some())
            + usize::from(reasoning_enabled.is_some())
            + usize::from(reasoning_budget_tokens.is_some());
        if explicit_reasoning > 1 {
            return Err("a Turn may select only one reasoning control".to_owned());
        }
        let (reasoning_effort, reasoning_enabled, reasoning_budget_tokens) =
            if explicit_reasoning == 1 {
                (
                    reasoning_effort.map(str::to_owned),
                    reasoning_enabled,
                    reasoning_budget_tokens,
                )
            } else {
                reasoning_selection_or_default(model, current)
            };
        let service_tier = service_tier.map(str::to_owned).or_else(|| {
            model_supports_service_tier(model, current.service_tier.as_deref())
                .then(|| current.service_tier.clone())
                .flatten()
        });
        validate_selected_variant(
            model,
            reasoning_effort.as_deref(),
            reasoning_enabled,
            reasoning_budget_tokens,
            service_tier.as_deref(),
        )?;
        Ok(ResolvedTurnProfile {
            catalog_revision: self.catalog_revision.clone(),
            catalog_provenance: self.catalog_provenance.clone(),
            provider_id: current.provider_id.clone(),
            provider_instance: current.provider_instance.clone(),
            model: model.id.clone(),
            reasoning_effort,
            reasoning_enabled,
            reasoning_budget_tokens,
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

    /// Resolves every model admitted by the selected Provider that accepts the requested options.
    pub fn resolve_model_candidates(
        &self,
        reasoning_effort: Option<&str>,
        service_tier: Option<&str>,
    ) -> Vec<ResolvedTurnProfile> {
        self.resolve_model_control_candidates(reasoning_effort, None, None, service_tier)
    }

    /// Resolves selected-Provider candidates that accept one typed reasoning control.
    pub fn resolve_model_control_candidates(
        &self,
        reasoning_effort: Option<&str>,
        reasoning_enabled: Option<bool>,
        reasoning_budget_tokens: Option<u64>,
        service_tier: Option<&str>,
    ) -> Vec<ResolvedTurnProfile> {
        self.selected_provider_models()
            .into_iter()
            .filter_map(|model| {
                self.resolve_model_controls(
                    &model,
                    reasoning_effort,
                    reasoning_enabled,
                    reasoning_budget_tokens,
                    service_tier,
                )
                .ok()
            })
            .collect()
    }
}

fn reasoning_selection_or_default(
    model: &ModelCatalogEntry,
    current: &ResolvedTurnProfile,
) -> (Option<String>, Option<bool>, Option<u64>) {
    match &model.capabilities.reasoning {
        ModelReasoningControl::Selectable {
            efforts, default, ..
        } => (
            current
                .reasoning_effort
                .as_ref()
                .filter(|effort| efforts.contains(effort))
                .cloned()
                .or_else(|| default.clone()),
            None,
            None,
        ),
        ModelReasoningControl::Toggle {
            default_enabled, ..
        } => (
            None,
            Some(current.reasoning_enabled.unwrap_or(*default_enabled)),
            None,
        ),
        ModelReasoningControl::BudgetTokens {
            minimum,
            maximum,
            default,
        } => (
            None,
            None,
            Some(
                current
                    .reasoning_budget_tokens
                    .filter(|value| (*minimum..=*maximum).contains(value))
                    .unwrap_or(*default),
            ),
        ),
        ModelReasoningControl::Unknown | ModelReasoningControl::Unsupported => (None, None, None),
    }
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
    pub display_name: String,
    pub description: String,
    pub hidden: bool,
    pub selected: bool,
    pub default_reasoning_effort: Option<String>,
    pub default_service_tier: Option<String>,
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
    selected_catalog: Option<&model_contract::CatalogResponse>,
) -> Result<ProviderModelCatalog, String> {
    let catalog_revision = catalog_content_revision(selected_catalog)?;
    let catalog_provenance = selected_catalog
        .map(|catalog| project_catalog_provenance(&catalog.provenance))
        .transpose()?;
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
            selected_catalog,
        )?);
    }
    let resolved_turn_profile = resolved_turn_profile(
        &providers,
        selected_instance.as_deref(),
        selected_model.as_deref(),
        selected_reasoning_effort,
        selected_service_tier,
        &catalog_revision,
        catalog_provenance.as_ref(),
    )?;
    Ok(ProviderModelCatalog {
        schema: "lenso.agent.provider-model-catalog.v4".to_owned(),
        catalog_revision,
        catalog_provenance,
        resolved_turn_profile,
        providers,
    })
}

fn catalog_content_revision(
    selected_catalog: Option<&model_contract::CatalogResponse>,
) -> Result<String, String> {
    let models = selected_catalog.map(|catalog| &catalog.models);
    let bytes = serde_json::to_vec(&models)
        .map_err(|error| format!("failed to encode normalized Model catalog: {error}"))?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn project_provider(
    definition: ProviderDefinition,
    host: &HostCatalog,
    plan: &ResolvedAppPlan,
    selected_instance: Option<&str>,
    selected_model: Option<&str>,
    selected_catalog: Option<&model_contract::CatalogResponse>,
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
    let models = if selected_instance.is_some() {
        selected_catalog
            .ok_or_else(|| "selected Model Provider returned no catalog snapshot".to_owned())?
            .models
            .iter()
            .map(|model| project_model(model, selected_model))
            .collect::<Result<Vec<_>, _>>()?
    } else {
        let mut model_ids = instances
            .values()
            .flatten()
            .cloned()
            .collect::<BTreeSet<_>>();
        if let Some(model) = definition.fallback_model {
            model_ids.insert(model.to_owned());
        }
        model_ids
            .into_iter()
            .map(|id| configured_model(definition.plugin_id, id, selected_model))
            .collect()
    };
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
        models,
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
    catalog_provenance: Option<&ModelCatalogProvenance>,
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
    let (reasoning_effort, reasoning_enabled, reasoning_budget_tokens) =
        if reasoning_effort.is_some() {
            (reasoning_effort, None, None)
        } else {
            match &model.capabilities.reasoning {
                ModelReasoningControl::Selectable { default, .. } => (default.clone(), None, None),
                ModelReasoningControl::Toggle {
                    default_enabled, ..
                } => (None, Some(*default_enabled), None),
                ModelReasoningControl::BudgetTokens { default, .. } => (None, None, Some(*default)),
                ModelReasoningControl::Unknown | ModelReasoningControl::Unsupported => {
                    (None, None, None)
                }
            }
        };
    validate_selected_variant(
        model,
        reasoning_effort.as_deref(),
        reasoning_enabled,
        reasoning_budget_tokens,
        service_tier.as_deref(),
    )?;
    Ok(Some(ResolvedTurnProfile {
        catalog_revision: catalog_revision.to_owned(),
        catalog_provenance: catalog_provenance.cloned(),
        provider_id: provider.provider_id.clone(),
        provider_instance: selected_instance.to_owned(),
        model: selected_model.to_owned(),
        reasoning_effort,
        reasoning_enabled,
        reasoning_budget_tokens,
        service_tier,
        limits: model.limits.clone(),
        capabilities: model.capabilities.clone(),
        wire_protocol: model.wire_protocol,
        compaction_compatibility: model.compaction_compatibility.clone(),
    }))
}

fn project_catalog_provenance(
    provenance: &model_contract::CatalogProvenance,
) -> Result<ModelCatalogProvenance, String> {
    let source = match provenance.source {
        model_contract::CatalogSource::Live => ModelCatalogSource::Live,
        model_contract::CatalogSource::Cache => ModelCatalogSource::Cache,
        model_contract::CatalogSource::Configured => ModelCatalogSource::Configured,
    };
    let freshness = match provenance.freshness {
        model_contract::CatalogFreshness::Fresh => ModelCatalogFreshness::Fresh,
        model_contract::CatalogFreshness::Revalidated => ModelCatalogFreshness::Revalidated,
        model_contract::CatalogFreshness::Stale => ModelCatalogFreshness::Stale,
    };
    let fetched_at_unix_seconds = portable_u64(
        provenance.fetched_at_unix_seconds.as_ref(),
        "catalog fetched_at_unix_seconds",
    )?;
    let validated_at_unix_seconds = portable_u64(
        provenance.validated_at_unix_seconds.as_ref(),
        "catalog validated_at_unix_seconds",
    )?;
    let max_stale_seconds = portable_u64(
        provenance.max_stale_seconds.as_ref(),
        "catalog max_stale_seconds",
    )?;
    let revision = provenance
        .revision
        .as_ref()
        .and_then(Option::as_ref)
        .cloned();
    if revision
        .as_deref()
        .is_some_and(|value| value.trim() != value || value.is_empty() || value.len() > 256)
        || max_stale_seconds.is_some_and(|value| value > MAX_CATALOG_STALE_SECONDS)
    {
        return Err("selected Model Provider returned invalid catalog provenance".to_owned());
    }
    match source {
        ModelCatalogSource::Configured
            if freshness == ModelCatalogFreshness::Fresh
                && fetched_at_unix_seconds.is_none()
                && validated_at_unix_seconds.is_none()
                && revision.is_none()
                && max_stale_seconds.is_none() => {}
        ModelCatalogSource::Live
            if freshness == ModelCatalogFreshness::Fresh
                && fetched_at_unix_seconds.is_some()
                && validated_at_unix_seconds.is_some()
                && revision.is_some()
                && max_stale_seconds.is_some() => {}
        ModelCatalogSource::Cache
            if matches!(
                freshness,
                ModelCatalogFreshness::Revalidated | ModelCatalogFreshness::Stale
            ) && fetched_at_unix_seconds.is_some()
                && validated_at_unix_seconds.is_some()
                && revision.is_some()
                && max_stale_seconds.is_some() => {}
        _ => {
            return Err(
                "selected Model Provider returned inconsistent catalog provenance".to_owned(),
            );
        }
    }
    if freshness == ModelCatalogFreshness::Stale {
        let fetched = fetched_at_unix_seconds.expect("stale cache requires fetched time");
        let validated = validated_at_unix_seconds.expect("stale cache requires validation time");
        let maximum = max_stale_seconds.expect("stale cache requires a maximum age");
        if validated < fetched || validated - fetched > maximum {
            return Err("selected Model Provider returned over-age catalog provenance".to_owned());
        }
    }
    Ok(ModelCatalogProvenance {
        source,
        freshness,
        fetched_at_unix_seconds,
        validated_at_unix_seconds,
        revision,
        max_stale_seconds,
    })
}

fn portable_u64(value: Option<&Option<String>>, field: &str) -> Result<Option<u64>, String> {
    value
        .and_then(Option::as_ref)
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|_| format!("selected Model Provider returned invalid {field}"))
        })
        .transpose()
}

fn validate_selected_variant(
    model: &ModelCatalogEntry,
    reasoning_effort: Option<&str>,
    reasoning_enabled: Option<bool>,
    reasoning_budget_tokens: Option<u64>,
    service_tier: Option<&str>,
) -> Result<(), String> {
    let selected_count = usize::from(reasoning_effort.is_some())
        + usize::from(reasoning_enabled.is_some())
        + usize::from(reasoning_budget_tokens.is_some());
    if selected_count > 1 {
        return Err("a Turn may select only one reasoning control".to_owned());
    }
    match (
        &model.capabilities.reasoning,
        reasoning_effort,
        reasoning_enabled,
        reasoning_budget_tokens,
    ) {
        (ModelReasoningControl::Selectable { efforts, .. }, Some(selected), None, None)
            if efforts.iter().any(|effort| effort == selected) => {}
        (ModelReasoningControl::Toggle { .. }, None, Some(_), None) | (_, None, None, None) => {}
        (
            ModelReasoningControl::BudgetTokens {
                minimum, maximum, ..
            },
            None,
            None,
            Some(selected),
        ) if (*minimum..=*maximum).contains(&selected) => {}
        _ => {
            return Err(
                "selected reasoning control is not admitted by the model catalog".to_owned(),
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

pub(crate) fn selected_model_instance(plan: &ResolvedAppPlan) -> Result<Option<String>, String> {
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
    for key in ["allowed_models", "include_models"] {
        if let Some(configured) = object.get(key) {
            let configured = configured
                .as_array()
                .ok_or_else(|| format!("Model configuration {key} must be an array"))?;
            for model in configured {
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

fn project_model(
    model: &model_contract::CatalogModel,
    selected_model: Option<&str>,
) -> Result<ModelCatalogEntry, String> {
    let reasoning = project_reasoning_control(&model.reasoning)?;
    let service_tiers = project_control(&model.service_tiers, false)?;
    let default_reasoning_effort = match &reasoning {
        ModelReasoningControl::Selectable { default, .. } => default.clone(),
        _ => None,
    };
    Ok(ModelCatalogEntry {
        id: model.id.clone(),
        display_name: model.display_name.clone(),
        description: model.description.clone(),
        hidden: model.hidden,
        selected: selected_model == Some(model.id.as_str()),
        default_reasoning_effort,
        default_service_tier: model.service_tiers.default.clone().flatten(),
        limits: ModelLimits {
            context_window_tokens: optional_tokens(model.limits.context_window_tokens.as_ref())?,
            max_input_tokens: optional_tokens(model.limits.max_input_tokens.as_ref())?,
            max_output_tokens: optional_tokens(model.limits.max_output_tokens.as_ref())?,
        },
        capabilities: ModelCapabilities {
            input_modalities: model
                .input_modalities
                .iter()
                .map(|modality| match modality {
                    model_contract::CatalogInputModality::Text => ModelInputModality::Text,
                    model_contract::CatalogInputModality::Image => ModelInputModality::Image,
                    model_contract::CatalogInputModality::Audio => ModelInputModality::Audio,
                })
                .collect(),
            text_output: model.text_output,
            tool_calls: model.tool_calls,
            parallel_tool_calls: model.parallel_tool_calls,
            reasoning,
            service_tiers: match service_tiers {
                ProjectedControl::Unknown => ModelServiceTierControl::Unknown,
                ProjectedControl::Unsupported => ModelServiceTierControl::Unsupported,
                ProjectedControl::Selectable(values) => {
                    ModelServiceTierControl::Selectable { tiers: values }
                }
            },
        },
        wire_protocol: match model.wire_protocol {
            model_contract::CatalogWireProtocol::Fixture => ModelWireProtocol::Fixture,
            model_contract::CatalogWireProtocol::OpenaiResponses => {
                ModelWireProtocol::OpenaiResponses
            }
            model_contract::CatalogWireProtocol::OpenaiChatCompletions => {
                ModelWireProtocol::OpenaiChatCompletions
            }
        },
        compaction_compatibility: model.compaction_compatibility.clone(),
    })
}

enum ProjectedControl {
    Unknown,
    Unsupported,
    Selectable(Vec<String>),
}

fn project_reasoning_control(
    control: &model_contract::CatalogControl,
) -> Result<ModelReasoningControl, String> {
    let values = control
        .options
        .iter()
        .map(|option| option.id.clone())
        .collect::<BTreeSet<_>>();
    let default = control.default.as_ref().and_then(Option::as_ref);
    let mode = control.mode.as_ref().and_then(Option::as_ref);
    let budget = control.budget_tokens.as_ref().and_then(Option::as_ref);
    if values.len() != control.options.len() || default.is_some_and(|value| !values.contains(value))
    {
        return Err("Model Provider returned inconsistent reasoning metadata".to_owned());
    }
    let empty =
        control.options.is_empty() && default.is_none() && mode.is_none() && budget.is_none();
    match (&control.status, mode, budget) {
        (model_contract::CatalogControlStatus::Unknown, None, None) if empty => {
            Ok(ModelReasoningControl::Unknown)
        }
        (model_contract::CatalogControlStatus::Unsupported, None, None) if empty => {
            Ok(ModelReasoningControl::Unsupported)
        }
        (
            model_contract::CatalogControlStatus::Selectable,
            None | Some(model_contract::CatalogControlMode::Effort),
            None,
        ) if !values.is_empty() && default.is_some() => Ok(ModelReasoningControl::Selectable {
            efforts: values.into_iter().collect(),
            options: control
                .options
                .iter()
                .map(|option| ModelControlOption {
                    id: option.id.clone(),
                    name: option.name.clone(),
                    description: option.description.clone(),
                })
                .collect(),
            default: default.cloned(),
        }),
        (
            model_contract::CatalogControlStatus::Selectable,
            Some(model_contract::CatalogControlMode::Toggle),
            None,
        ) if values == BTreeSet::from(["off".to_owned(), "on".to_owned()]) && default.is_some() => {
            Ok(ModelReasoningControl::Toggle {
                default_enabled: default.is_some_and(|value| value == "on"),
                options: control
                    .options
                    .iter()
                    .map(|option| ModelControlOption {
                        id: option.id.clone(),
                        name: option.name.clone(),
                        description: option.description.clone(),
                    })
                    .collect(),
            })
        }
        (
            model_contract::CatalogControlStatus::Selectable,
            Some(model_contract::CatalogControlMode::BudgetTokens),
            Some(budget),
        ) if control.options.is_empty() && default.is_none() => {
            let minimum = budget.minimum.parse::<u64>().map_err(|_| {
                "Model Provider returned an invalid reasoning token budget".to_owned()
            })?;
            let maximum = budget.maximum.parse::<u64>().map_err(|_| {
                "Model Provider returned an invalid reasoning token budget".to_owned()
            })?;
            let default = budget.default.parse::<u64>().map_err(|_| {
                "Model Provider returned an invalid reasoning token budget".to_owned()
            })?;
            if minimum == 0 || minimum > default || default > maximum {
                return Err("Model Provider returned an invalid reasoning token budget".to_owned());
            }
            Ok(ModelReasoningControl::BudgetTokens {
                minimum,
                maximum,
                default,
            })
        }
        _ => Err("Model Provider returned invalid reasoning control metadata".to_owned()),
    }
}

fn project_control(
    control: &model_contract::CatalogControl,
    reasoning: bool,
) -> Result<ProjectedControl, String> {
    let values = control
        .options
        .iter()
        .map(|option| option.id.clone())
        .collect::<BTreeSet<_>>();
    let default = control.default.as_ref().and_then(Option::as_ref);
    let mode = control.mode.as_ref().and_then(Option::as_ref);
    let budget = control.budget_tokens.as_ref().and_then(Option::as_ref);
    if values.len() != control.options.len() || default.is_some_and(|value| !values.contains(value))
    {
        return Err("Model Provider returned inconsistent control metadata".to_owned());
    }
    if mode.is_some() || budget.is_some() {
        return Err(
            "Model Provider returned reasoning metadata for a service-tier control".to_owned(),
        );
    }
    let invalid_empty = !control.options.is_empty() || default.is_some();
    match control.status {
        model_contract::CatalogControlStatus::Unknown if !invalid_empty => {
            Ok(ProjectedControl::Unknown)
        }
        model_contract::CatalogControlStatus::Unsupported if !invalid_empty => {
            Ok(ProjectedControl::Unsupported)
        }
        model_contract::CatalogControlStatus::Selectable if !values.is_empty() => {
            Ok(ProjectedControl::Selectable(values.into_iter().collect()))
        }
        _ => Err(format!(
            "Model Provider returned invalid {} control metadata",
            if reasoning {
                "reasoning"
            } else {
                "service-tier"
            }
        )),
    }
}

#[allow(
    clippy::option_option,
    reason = "generated portable optional fields distinguish omitted from explicit null"
)]
fn optional_tokens(value: Option<&Option<String>>) -> Result<Option<u64>, String> {
    value
        .and_then(Option::as_ref)
        .map(|value| {
            value
                .parse::<u64>()
                .ok()
                .filter(|value| *value > 0)
                .ok_or_else(|| "Model Provider returned an invalid token limit".to_owned())
        })
        .transpose()
}

fn configured_model(
    plugin_id: &str,
    id: String,
    selected_model: Option<&str>,
) -> ModelCatalogEntry {
    ModelCatalogEntry {
        display_name: id.clone(),
        description: "Configured model; Provider metadata is unavailable until selected".to_owned(),
        hidden: false,
        selected: selected_model == Some(id.as_str()),
        default_reasoning_effort: None,
        default_service_tier: None,
        limits: ModelLimits {
            context_window_tokens: None,
            max_input_tokens: None,
            max_output_tokens: None,
        },
        capabilities: ModelCapabilities {
            input_modalities: vec![ModelInputModality::Text],
            text_output: true,
            tool_calls: true,
            parallel_tool_calls: true,
            reasoning: ModelReasoningControl::Unknown,
            service_tiers: ModelServiceTierControl::Unknown,
        },
        wire_protocol: match plugin_id {
            CODEX_DIRECT_PLUGIN => ModelWireProtocol::OpenaiResponses,
            OPENAI_COMPATIBLE_PLUGIN => ModelWireProtocol::OpenaiChatCompletions,
            _ => ModelWireProtocol::Fixture,
        },
        compaction_compatibility: "generic-text-v1".to_owned(),
        id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache_provenance(
        freshness: model_contract::CatalogFreshness,
        fetched: u64,
        validated: u64,
        maximum: u64,
    ) -> model_contract::CatalogProvenance {
        model_contract::CatalogProvenance {
            source: model_contract::CatalogSource::Cache,
            freshness,
            fetched_at_unix_seconds: Some(Some(fetched.to_string())),
            validated_at_unix_seconds: Some(Some(validated.to_string())),
            revision: Some(Some("\"catalog-v1\"".to_owned())),
            max_stale_seconds: Some(Some(maximum.to_string())),
        }
    }

    #[test]
    fn stale_catalog_provenance_is_bounded_and_projected() {
        let projected = project_catalog_provenance(&cache_provenance(
            model_contract::CatalogFreshness::Stale,
            100,
            160,
            60,
        ))
        .unwrap();
        assert_eq!(projected.source, ModelCatalogSource::Cache);
        assert_eq!(projected.freshness, ModelCatalogFreshness::Stale);
        assert_eq!(projected.fetched_at_unix_seconds, Some(100));
        assert_eq!(projected.validated_at_unix_seconds, Some(160));
    }

    #[test]
    fn over_age_or_inconsistent_catalog_provenance_fails_closed() {
        assert!(
            project_catalog_provenance(&cache_provenance(
                model_contract::CatalogFreshness::Stale,
                100,
                161,
                60,
            ))
            .is_err()
        );
        let configured_with_fetch = model_contract::CatalogProvenance {
            source: model_contract::CatalogSource::Configured,
            freshness: model_contract::CatalogFreshness::Fresh,
            fetched_at_unix_seconds: Some(Some("100".to_owned())),
            validated_at_unix_seconds: None,
            revision: None,
            max_stale_seconds: None,
        };
        assert!(project_catalog_provenance(&configured_with_fetch).is_err());
    }

    fn option(id: &str, name: &str) -> model_contract::CatalogControlOption {
        model_contract::CatalogControlOption {
            id: id.to_owned(),
            name: name.to_owned(),
            description: format!("{name} reasoning"),
        }
    }

    fn selectable_reasoning(
        mode: model_contract::CatalogControlMode,
        options: Vec<model_contract::CatalogControlOption>,
        default: Option<&str>,
        budget_tokens: Option<model_contract::CatalogTokenBudget>,
    ) -> model_contract::CatalogControl {
        model_contract::CatalogControl {
            status: model_contract::CatalogControlStatus::Selectable,
            mode: Some(Some(mode)),
            options,
            default: default.map(|value| Some(value.to_owned())),
            budget_tokens: budget_tokens.map(Some),
        }
    }

    fn model_with_reasoning(reasoning: ModelReasoningControl) -> ModelCatalogEntry {
        ModelCatalogEntry {
            id: "reasoning-model".to_owned(),
            display_name: "Reasoning model".to_owned(),
            description: String::new(),
            hidden: false,
            selected: true,
            default_reasoning_effort: None,
            default_service_tier: None,
            limits: ModelLimits {
                context_window_tokens: None,
                max_input_tokens: None,
                max_output_tokens: None,
            },
            capabilities: ModelCapabilities {
                input_modalities: vec![ModelInputModality::Text],
                text_output: true,
                tool_calls: true,
                parallel_tool_calls: true,
                reasoning,
                service_tiers: ModelServiceTierControl::Unsupported,
            },
            wire_protocol: ModelWireProtocol::Fixture,
            compaction_compatibility: "generic-text-v1".to_owned(),
        }
    }

    fn catalog_with_reasoning(reasoning: ModelReasoningControl) -> ProviderModelCatalog {
        let model = model_with_reasoning(reasoning);
        let profile = ResolvedTurnProfile {
            catalog_revision: format!("sha256:{}", "a".repeat(64)),
            catalog_provenance: None,
            provider_id: "fixture".to_owned(),
            provider_instance: "lenso.agent.model.fixture/model".to_owned(),
            model: model.id.clone(),
            reasoning_effort: None,
            reasoning_enabled: None,
            reasoning_budget_tokens: None,
            service_tier: None,
            limits: model.limits.clone(),
            capabilities: model.capabilities.clone(),
            wire_protocol: model.wire_protocol,
            compaction_compatibility: model.compaction_compatibility.clone(),
        };
        ProviderModelCatalog {
            schema: "lenso.agent.provider-model-catalog.v4".to_owned(),
            catalog_revision: profile.catalog_revision.clone(),
            catalog_provenance: None,
            resolved_turn_profile: Some(profile),
            providers: vec![ModelProviderCatalogEntry {
                provider_id: "fixture".to_owned(),
                name: "Fixture".to_owned(),
                plugin_id: FIXTURE_PLUGIN.to_owned(),
                authentication: ModelAuthentication::None,
                readiness: ModelProviderReadiness {
                    status: ModelProviderReadinessStatus::Unchecked,
                    detail: String::new(),
                },
                available_instances: vec!["lenso.agent.model.fixture/model".to_owned()],
                selected_instance: Some("lenso.agent.model.fixture/model".to_owned()),
                models: vec![model],
            }],
        }
    }

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
    fn unselected_models_do_not_claim_provider_metadata() {
        let model = configured_model(CODEX_DIRECT_PLUGIN, "gpt-next".to_owned(), None);
        assert!(matches!(
            model.capabilities.reasoning,
            ModelReasoningControl::Unknown
        ));
        assert!(matches!(
            model.capabilities.service_tiers,
            ModelServiceTierControl::Unknown
        ));
    }

    #[test]
    fn configured_model_projection_reads_include_models_during_unselected_inspection() {
        let models = configuration_models_value(&serde_json::json!({
            "model": "gpt-primary",
            "include_models": ["gpt-visible"]
        }))
        .unwrap();

        assert_eq!(models, ["gpt-primary", "gpt-visible"]);
    }

    #[test]
    fn reasoning_projection_preserves_effort_labels_and_default() {
        let control = selectable_reasoning(
            model_contract::CatalogControlMode::Effort,
            vec![option("low", "Low"), option("high", "High")],
            Some("high"),
            None,
        );

        assert_eq!(
            project_reasoning_control(&control).unwrap(),
            ModelReasoningControl::Selectable {
                efforts: vec!["high".to_owned(), "low".to_owned()],
                options: vec![
                    ModelControlOption {
                        id: "low".to_owned(),
                        name: "Low".to_owned(),
                        description: "Low reasoning".to_owned(),
                    },
                    ModelControlOption {
                        id: "high".to_owned(),
                        name: "High".to_owned(),
                        description: "High reasoning".to_owned(),
                    },
                ],
                default: Some("high".to_owned()),
            }
        );
    }

    #[test]
    fn reasoning_projection_supports_toggle_and_token_budget() {
        let toggle = selectable_reasoning(
            model_contract::CatalogControlMode::Toggle,
            vec![option("off", "Off"), option("on", "On")],
            Some("on"),
            None,
        );
        let budget = selectable_reasoning(
            model_contract::CatalogControlMode::BudgetTokens,
            Vec::new(),
            None,
            Some(model_contract::CatalogTokenBudget {
                minimum: "256".to_owned(),
                maximum: "8192".to_owned(),
                default: "2048".to_owned(),
            }),
        );

        assert_eq!(
            project_reasoning_control(&toggle).unwrap(),
            ModelReasoningControl::Toggle {
                default_enabled: true,
                options: vec![
                    ModelControlOption {
                        id: "off".to_owned(),
                        name: "Off".to_owned(),
                        description: "Off reasoning".to_owned(),
                    },
                    ModelControlOption {
                        id: "on".to_owned(),
                        name: "On".to_owned(),
                        description: "On reasoning".to_owned(),
                    },
                ],
            }
        );
        assert_eq!(
            project_reasoning_control(&budget).unwrap(),
            ModelReasoningControl::BudgetTokens {
                minimum: 256,
                maximum: 8192,
                default: 2048,
            }
        );
    }

    #[test]
    fn typed_reasoning_selection_is_validated_and_written_to_turn_provenance() {
        let toggle = model_with_reasoning(ModelReasoningControl::Toggle {
            default_enabled: true,
            options: Vec::new(),
        });
        assert!(validate_selected_variant(&toggle, None, Some(false), None, None).is_ok());
        assert!(validate_selected_variant(&toggle, Some("high"), None, None, None).is_err());

        let budget = model_with_reasoning(ModelReasoningControl::BudgetTokens {
            minimum: 256,
            maximum: 8192,
            default: 2048,
        });
        assert!(validate_selected_variant(&budget, None, None, Some(4096), None).is_ok());
        assert!(validate_selected_variant(&budget, None, None, Some(128), None).is_err());
        assert!(validate_selected_variant(&budget, None, Some(true), Some(4096), None).is_err());

        let toggle_profile = catalog_with_reasoning(ModelReasoningControl::Toggle {
            default_enabled: true,
            options: Vec::new(),
        })
        .resolve_model_controls("reasoning-model", None, Some(false), None, None)
        .unwrap();
        assert_eq!(toggle_profile.reasoning_enabled, Some(false));
        assert_eq!(toggle_profile.reasoning_budget_tokens, None);

        let budget_profile = catalog_with_reasoning(ModelReasoningControl::BudgetTokens {
            minimum: 256,
            maximum: 8192,
            default: 2048,
        })
        .resolve_model_controls("reasoning-model", None, None, Some(4096), None)
        .unwrap();
        assert_eq!(budget_profile.reasoning_enabled, None);
        assert_eq!(budget_profile.reasoning_budget_tokens, Some(4096));
    }
}
