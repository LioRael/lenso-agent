use std::collections::{BTreeMap, BTreeSet};

use lenso_agent_auth_openai_codex_module::{
    OpenAiCodexAuthFactory, PACKAGE_ID as CODEX_AUTH_PACKAGE_ID,
};
use lenso_agent_loop_module::PACKAGE_ID as AGENT_LOOP_PACKAGE_ID;
use lenso_agent_model_fixture_module::{
    FixtureModelFactory, MODEL_ID as FIXTURE_MODEL_ID, PACKAGE_ID as FIXTURE_MODEL_PACKAGE_ID,
};
use lenso_agent_model_openai_codex_direct_module::{
    OpenAiCodexDirectModelFactory, PACKAGE_ID as CODEX_MODEL_PACKAGE_ID,
};
use lenso_agent_text_tools_module::{
    FACTORY_IDENTITY as TEXT_TOOLS_FACTORY_IDENTITY, PACKAGE_ID as TEXT_TOOLS_PACKAGE_ID,
};
use lenso_app_plan::{
    CapabilityBinding, CapabilityCardinality, CapabilityOperationKind, ResolvedAppPlan,
};
use lenso_capability_agent_auth_openai_codex::{
    ACCESS_OPERATION as CODEX_AUTH_ACCESS_OPERATION, CAPABILITY_ID as CODEX_AUTH_CAPABILITY_ID,
    DESCRIPTOR_VERSION as CODEX_AUTH_DESCRIPTOR_VERSION,
};
use lenso_capability_agent_model::{
    CAPABILITY_ID as MODEL_CAPABILITY_ID, COMPLETE_OPERATION as MODEL_COMPLETE_OPERATION,
    DESCRIPTOR_VERSION as MODEL_DESCRIPTOR_VERSION,
};
use lenso_capability_agent_tool_provider::{
    CAPABILITY_ID as TOOL_PROVIDER_CAPABILITY_ID,
    CATALOG_OPERATION as TOOL_PROVIDER_CATALOG_OPERATION,
    DESCRIPTOR_VERSION as TOOL_PROVIDER_DESCRIPTOR_VERSION,
    EXECUTE_OPERATION as TOOL_PROVIDER_EXECUTE_OPERATION,
};
use lenso_native_adapter::NativeModuleFactory;
use lenso_plugin_control_plane::{
    BindingTemplate, CapabilityDeclaration, CapabilityRequirement, ControlPlaneError,
    ModuleContribution, PluginManifest, RequirementCardinality, SupportChannel, TrustLevel,
    sha256_digest,
};

pub(crate) const NATIVE_EXECUTION_CLASS: &str = "lenso.native-rust@1";
pub(crate) const NATIVE_TOOL_PROFILE: &str = "agent-tool-provider-v1";
pub(crate) const NATIVE_MODEL_PROFILE: &str = "agent-model-provider-v1";
pub(crate) const NATIVE_AUTH_PROFILE: &str = "agent-auth-provider-v1";

const EMPTY_CONFIGURATION_SCHEMA: &[u8] = br#"{"additionalProperties":false,"type":"object"}"#;
const TOOL_PROVIDER_DESCRIPTOR: &[u8] =
    include_bytes!("../../../crates/lenso-capability-agent-tool-provider/capability.json");
const MODEL_DESCRIPTOR: &[u8] =
    include_bytes!("../../../crates/lenso-capability-agent-model/capability.json");
const FIXTURE_MODEL_CONFIGURATION_SCHEMA: &[u8] =
    include_bytes!("../../../composition/headless-readonly/config/model.schema.json");
const CODEX_MODEL_CONFIGURATION_SCHEMA: &[u8] = include_bytes!(
    "../../../crates/lenso-agent-model-openai-codex-direct-module/config.schema.json"
);
const CODEX_AUTH_CONFIGURATION_SCHEMA: &[u8] =
    include_bytes!("../../../crates/lenso-agent-auth-openai-codex-module/config.schema.json");
const CODEX_AUTH_DESCRIPTOR: &[u8] =
    include_bytes!("../../../crates/lenso-capability-agent-auth-openai-codex/capability.json");
const FIXTURE_AGENT_CONFIGURATION: &str = r#"{"max_history_events":200,"max_output_tokens":1024,"max_steps":8,"max_tool_calls":4,"model":"fixture/readme-summary-v1"}"#;
const CODEX_AGENT_CONFIGURATION: &str = r#"{"max_history_events":200,"max_output_tokens":1024,"max_steps":8,"max_tool_calls":4,"model":"gpt-5.6-luna"}"#;
const CODEX_MODEL_CONFIGURATION: &str = r#"{"base_url":"https://chatgpt.com/backend-api","max_event_bytes":1048576,"model":"gpt-5.6-luna","reasoning_effort":"medium"}"#;
const CODEX_AUTH_CONFIGURATION: &str =
    r#"{"issuer":"https://auth.openai.com","profile":"default","refresh_margin_seconds":60}"#;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CapabilityProfile {
    capability_id: String,
    descriptor_version: String,
    descriptor_digest: String,
    request_operations: Vec<String>,
    operation_kinds: BTreeMap<String, CapabilityOperationKind>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AttachmentProfile {
    AppendMany {
        consumer_instance: String,
        capability_id: String,
        descriptor_version: String,
    },
    ReplaceOne {
        consumer_instance: String,
        capability_id: String,
        descriptor_version: String,
        displaced_provider_instance: String,
        allowed_displaced_packages: BTreeSet<String>,
        base_configuration_replacements: Vec<BaseConfigurationReplacement>,
    },
    IntraPluginOnly,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BaseConfigurationReplacement {
    pub(crate) instance_key: String,
    pub(crate) allowed_package: String,
    pub(crate) expected_configuration: String,
    pub(crate) replacement_configuration: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ResolvedAttachment {
    AppendMany(CapabilityBinding),
    ReplaceOne {
        binding: CapabilityBinding,
        displaced_provider_instance: String,
        base_configuration_replacements: Vec<BaseConfigurationReplacement>,
    },
    IntraPluginOnly,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExecutablePluginProfile {
    registration_id: String,
    adapter_profile: String,
    package_id: String,
    factory_identity: String,
    configuration_schema_digest: String,
    configuration: String,
    provides: Vec<CapabilityProfile>,
    requires: Vec<CapabilityRequirement>,
    entrypoint: String,
    execution_class: String,
    support_channel: SupportChannel,
    trust: TrustLevel,
    attachment: AttachmentProfile,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct PluginProfileCatalog {
    profiles: BTreeMap<String, ExecutablePluginProfile>,
}

impl PluginProfileCatalog {
    pub(crate) fn register(mut self, profile: ExecutablePluginProfile) -> Result<Self, String> {
        profile.validate()?;
        if self.profiles.contains_key(&profile.registration_id) {
            return Err(format!(
                "Plugin profile registration `{}` is registered twice",
                profile.registration_id
            ));
        }
        if self
            .profiles
            .values()
            .any(|registered| registered.factory_identity == profile.factory_identity)
        {
            return Err(format!(
                "Plugin factory `{}` has more than one attachment profile",
                profile.factory_identity
            ));
        }
        self.profiles
            .insert(profile.registration_id.clone(), profile);
        Ok(self)
    }

    pub(crate) fn profiles_for_execution_class(&self, execution_class: &str) -> Vec<String> {
        self.profiles
            .values()
            .filter(|profile| profile.execution_class == execution_class)
            .map(|profile| profile.adapter_profile.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub(crate) fn support_channels_for_execution_class(
        &self,
        execution_class: &str,
    ) -> Vec<SupportChannel> {
        self.profiles
            .values()
            .filter(|profile| profile.execution_class == execution_class)
            .map(|profile| profile.support_channel)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub(crate) fn trust_levels_for_execution_class(
        &self,
        execution_class: &str,
    ) -> Vec<TrustLevel> {
        self.profiles
            .values()
            .filter(|profile| profile.execution_class == execution_class)
            .map(|profile| profile.trust)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub(crate) fn validate_contribution(
        &self,
        contribution: &ModuleContribution,
        target: &str,
    ) -> Result<(), ControlPlaneError> {
        self.matching_profile(contribution, target).map(|_| ())
    }

    pub(crate) fn validate_manifest_topology(
        &self,
        manifest: &PluginManifest,
        target: &str,
    ) -> Result<(), ControlPlaneError> {
        for contribution in &manifest.module_contributions {
            self.validate_contribution(contribution, target)?;
        }
        let mut template_keys = BTreeSet::new();
        for template in &manifest.binding_templates {
            let key = (
                template.consumer_contribution_id.as_str(),
                template.capability_id.as_str(),
                template.provider_contribution_id.as_str(),
            );
            if !template_keys.insert(key) {
                return rejected("Plugin binding templates must be unique");
            }
            binding_template_parts(manifest, template)
                .map_err(|detail| ControlPlaneError::AdmissionRejected { detail })?;
        }
        let mut replaced_instances = BTreeSet::new();
        for contribution in &manifest.module_contributions {
            let profile = self.matching_profile(contribution, target)?;
            match &profile.attachment {
                AttachmentProfile::ReplaceOne {
                    displaced_provider_instance,
                    ..
                } => {
                    if !replaced_instances.insert(displaced_provider_instance) {
                        return rejected(format!(
                            "Plugin Manifest replaces Instance `{displaced_provider_instance}` more than once"
                        ));
                    }
                }
                AttachmentProfile::IntraPluginOnly => {
                    let consumers = manifest
                        .binding_templates
                        .iter()
                        .filter(|template| template.provider_contribution_id == contribution.id)
                        .count();
                    if consumers != 1 {
                        return rejected(format!(
                            "intra-Plugin contribution `{}` must be consumed exactly once",
                            contribution.id
                        ));
                    }
                }
                AttachmentProfile::AppendMany { .. } => {}
            }
            for requirement in &contribution.requires {
                if requirement.cardinality != RequirementCardinality::One {
                    return rejected("this Host accepts only exact one intra-Plugin requirements");
                }
                let count = manifest
                    .binding_templates
                    .iter()
                    .filter(|template| {
                        template.consumer_contribution_id == contribution.id
                            && template.capability_id == requirement.capability_id
                    })
                    .count();
                if count != 1 {
                    return rejected(format!(
                        "Plugin contribution `{}` does not close exactly one binding for `{}`",
                        contribution.id, requirement.capability_id
                    ));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn binding_for_template(
        manifest: &PluginManifest,
        template: &BindingTemplate,
        consumer_instance: &str,
        provider_instance: &str,
    ) -> Result<CapabilityBinding, String> {
        let (requirement, _) = binding_template_parts(manifest, template)?;
        Ok(CapabilityBinding::new(
            consumer_instance,
            &requirement.capability_id,
            &requirement.descriptor_version,
            provider_instance,
        ))
    }

    pub(crate) fn configuration_for(
        &self,
        contribution: &ModuleContribution,
        target: &str,
    ) -> Result<String, ControlPlaneError> {
        self.matching_profile(contribution, target)
            .map(|profile| profile.configuration.clone())
    }

    pub(crate) fn attachment_for(
        &self,
        contribution: &ModuleContribution,
        target: &str,
        instance_key: &str,
        base_plan: &ResolvedAppPlan,
    ) -> Result<ResolvedAttachment, String> {
        let profile = self
            .matching_profile(contribution, target)
            .map_err(|error| format!("Plugin profile selection failed: {error}"))?;
        let Some((consumer_instance, capability_id, descriptor_version)) =
            profile.attachment.capability_edge()
        else {
            return Ok(ResolvedAttachment::IntraPluginOnly);
        };
        let consumer = base_plan
            .module_instance(consumer_instance)
            .ok_or_else(|| {
                format!(
                    "Plugin profile `{}` requires consumer Instance `{}`",
                    profile.registration_id, consumer_instance
                )
            })?;
        let binding = CapabilityBinding::new(
            consumer_instance,
            capability_id,
            descriptor_version,
            instance_key,
        );
        match &profile.attachment {
            AttachmentProfile::AppendMany { .. } => {
                require_cardinality(
                    consumer,
                    capability_id,
                    descriptor_version,
                    CapabilityCardinality::Many,
                    &profile.registration_id,
                )?;
                Ok(ResolvedAttachment::AppendMany(binding))
            }
            AttachmentProfile::ReplaceOne {
                displaced_provider_instance,
                allowed_displaced_packages,
                base_configuration_replacements,
                ..
            } => {
                require_cardinality(
                    consumer,
                    capability_id,
                    descriptor_version,
                    CapabilityCardinality::One,
                    &profile.registration_id,
                )?;
                validate_displaced_provider(
                    base_plan,
                    consumer_instance,
                    capability_id,
                    descriptor_version,
                    displaced_provider_instance,
                    allowed_displaced_packages,
                    &profile.registration_id,
                )?;
                Ok(ResolvedAttachment::ReplaceOne {
                    binding,
                    displaced_provider_instance: displaced_provider_instance.clone(),
                    base_configuration_replacements: base_configuration_replacements.clone(),
                })
            }
            AttachmentProfile::IntraPluginOnly => unreachable!("handled above"),
        }
    }

    fn matching_profile<'a>(
        &'a self,
        contribution: &ModuleContribution,
        target: &str,
    ) -> Result<&'a ExecutablePluginProfile, ControlPlaneError> {
        let matches = self
            .profiles
            .values()
            .filter(|profile| profile.matches(contribution, target))
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [profile] => Ok(*profile),
            [] => rejected(format!(
                "executable Module contribution `{}` does not match a registered Plugin profile",
                contribution.id
            )),
            _ => rejected(format!(
                "executable Module contribution `{}` matches more than one Plugin profile",
                contribution.id
            )),
        }
    }
}

impl ExecutablePluginProfile {
    fn validate(&self) -> Result<(), String> {
        if self.registration_id.is_empty()
            || self.adapter_profile.is_empty()
            || self.package_id.is_empty()
            || self.factory_identity.is_empty()
            || self.configuration_schema_digest.is_empty()
            || self.configuration.is_empty()
            || self.provides.is_empty()
            || self.entrypoint.is_empty()
            || self.execution_class.is_empty()
        {
            return Err("Plugin profile fields must be non-empty".to_owned());
        }
        if let Some((consumer, attachment_capability_id, attachment_descriptor_version)) =
            self.attachment.capability_edge()
        {
            if consumer.is_empty()
                || attachment_capability_id.is_empty()
                || attachment_descriptor_version.is_empty()
            {
                return Err("Plugin attachment fields must be non-empty".to_owned());
            }
            let matching_provision = self.provides.iter().filter(|provided| {
                provided.capability_id == attachment_capability_id
                    && provided.descriptor_version == attachment_descriptor_version
            });
            if matching_provision.count() != 1 {
                return Err(format!(
                    "Plugin profile `{}` attachment must select exactly one provided Capability",
                    self.registration_id
                ));
            }
        }
        if let AttachmentProfile::ReplaceOne {
            displaced_provider_instance,
            allowed_displaced_packages,
            ..
        } = &self.attachment
            && (displaced_provider_instance.is_empty() || allowed_displaced_packages.is_empty())
        {
            return Err(format!(
                "Plugin profile `{}` replacement policy is incomplete",
                self.registration_id
            ));
        }
        if let AttachmentProfile::ReplaceOne {
            base_configuration_replacements,
            ..
        } = &self.attachment
        {
            for replacement in base_configuration_replacements {
                if replacement.instance_key.is_empty()
                    || replacement.allowed_package.is_empty()
                    || !is_canonical_json(&replacement.expected_configuration)?
                    || !is_canonical_json(&replacement.replacement_configuration)?
                {
                    return Err(format!(
                        "Plugin profile `{}` base configuration replacement is invalid",
                        self.registration_id
                    ));
                }
            }
        }
        let configuration: serde_json::Value = serde_json::from_str(&self.configuration)
            .map_err(|error| format!("Plugin profile configuration is invalid JSON: {error}"))?;
        if serde_json::to_string(&configuration).map_err(|error| error.to_string())?
            != self.configuration
        {
            return Err(format!(
                "Plugin profile `{}` configuration is not canonical JSON",
                self.registration_id
            ));
        }
        Ok(())
    }

    fn matches(&self, contribution: &ModuleContribution, target: &str) -> bool {
        contribution.package_id == self.package_id
            && contribution.configuration_schema_digest == self.configuration_schema_digest
            && contribution.requires == self.requires
            && contribution.permission_request_ids.is_empty()
            && contribution.state.is_none()
            && contribution.provides.len() == self.provides.len()
            && contribution
                .provides
                .iter()
                .zip(&self.provides)
                .all(|(provided, expected)| {
                    provided.capability_id == expected.capability_id
                        && provided.descriptor_version == expected.descriptor_version
                        && provided.descriptor_digest == expected.descriptor_digest
                        && provided.request_operations == expected.request_operations
                        && provided.operation_kinds == expected.operation_kinds
                })
            && !contribution.implementations.is_empty()
            && contribution
                .implementations
                .iter()
                .any(|implementation| implementation.targets.iter().any(|item| item == target))
            && contribution.implementations.iter().all(|implementation| {
                implementation.artifact.is_none()
                    && implementation.built_in_factory.as_deref()
                        == Some(self.factory_identity.as_str())
                    && implementation.entrypoint == self.entrypoint
                    && implementation.execution_class == self.execution_class
                    && implementation.profiles == [self.adapter_profile.as_str()]
                    && implementation.support_channel == self.support_channel
                    && implementation.trust == self.trust
            })
    }
}

pub(crate) fn harness_plugin_profiles() -> Result<PluginProfileCatalog, String> {
    PluginProfileCatalog::default()
        .register(text_tools_profile())?
        .register(fixture_model_profile())?
        .register(codex_model_profile())?
        .register(codex_auth_profile())
}

fn text_tools_profile() -> ExecutablePluginProfile {
    ExecutablePluginProfile {
        registration_id: "native-text-tools-v1".to_owned(),
        adapter_profile: NATIVE_TOOL_PROFILE.to_owned(),
        package_id: TEXT_TOOLS_PACKAGE_ID.to_owned(),
        factory_identity: TEXT_TOOLS_FACTORY_IDENTITY.to_owned(),
        configuration_schema_digest: sha256_digest(EMPTY_CONFIGURATION_SCHEMA),
        configuration: "{}".to_owned(),
        provides: vec![CapabilityProfile {
            capability_id: TOOL_PROVIDER_CAPABILITY_ID.to_owned(),
            descriptor_version: TOOL_PROVIDER_DESCRIPTOR_VERSION.to_owned(),
            descriptor_digest: sha256_digest(TOOL_PROVIDER_DESCRIPTOR),
            request_operations: vec![
                TOOL_PROVIDER_CATALOG_OPERATION.to_owned(),
                TOOL_PROVIDER_EXECUTE_OPERATION.to_owned(),
            ],
            operation_kinds: BTreeMap::new(),
        }],
        requires: Vec::new(),
        entrypoint: "default".to_owned(),
        execution_class: NATIVE_EXECUTION_CLASS.to_owned(),
        support_channel: SupportChannel::Stable,
        trust: TrustLevel::Trusted,
        attachment: AttachmentProfile::AppendMany {
            consumer_instance: "tools".to_owned(),
            capability_id: TOOL_PROVIDER_CAPABILITY_ID.to_owned(),
            descriptor_version: TOOL_PROVIDER_DESCRIPTOR_VERSION.to_owned(),
        },
    }
}

fn fixture_model_profile() -> ExecutablePluginProfile {
    ExecutablePluginProfile {
        registration_id: "native-fixture-model-v1".to_owned(),
        adapter_profile: NATIVE_MODEL_PROFILE.to_owned(),
        package_id: FIXTURE_MODEL_PACKAGE_ID.to_owned(),
        factory_identity: FixtureModelFactory.factory_identity(),
        configuration_schema_digest: sha256_digest(FIXTURE_MODEL_CONFIGURATION_SCHEMA),
        configuration: format!(r#"{{"model":"{FIXTURE_MODEL_ID}"}}"#),
        provides: vec![CapabilityProfile {
            capability_id: MODEL_CAPABILITY_ID.to_owned(),
            descriptor_version: MODEL_DESCRIPTOR_VERSION.to_owned(),
            descriptor_digest: sha256_digest(MODEL_DESCRIPTOR),
            request_operations: vec![MODEL_COMPLETE_OPERATION.to_owned()],
            operation_kinds: BTreeMap::from([(
                MODEL_COMPLETE_OPERATION.to_owned(),
                CapabilityOperationKind::Stream,
            )]),
        }],
        requires: Vec::new(),
        entrypoint: "default".to_owned(),
        execution_class: NATIVE_EXECUTION_CLASS.to_owned(),
        support_channel: SupportChannel::Stable,
        trust: TrustLevel::Trusted,
        attachment: AttachmentProfile::ReplaceOne {
            consumer_instance: "agent".to_owned(),
            capability_id: MODEL_CAPABILITY_ID.to_owned(),
            descriptor_version: MODEL_DESCRIPTOR_VERSION.to_owned(),
            displaced_provider_instance: "model".to_owned(),
            allowed_displaced_packages: BTreeSet::from([FIXTURE_MODEL_PACKAGE_ID.to_owned()]),
            base_configuration_replacements: Vec::new(),
        },
    }
}

fn codex_model_profile() -> ExecutablePluginProfile {
    ExecutablePluginProfile {
        registration_id: "native-codex-direct-model-v1".to_owned(),
        adapter_profile: NATIVE_MODEL_PROFILE.to_owned(),
        package_id: CODEX_MODEL_PACKAGE_ID.to_owned(),
        factory_identity: OpenAiCodexDirectModelFactory.factory_identity(),
        configuration_schema_digest: sha256_digest(CODEX_MODEL_CONFIGURATION_SCHEMA),
        configuration: CODEX_MODEL_CONFIGURATION.to_owned(),
        provides: vec![CapabilityProfile {
            capability_id: MODEL_CAPABILITY_ID.to_owned(),
            descriptor_version: MODEL_DESCRIPTOR_VERSION.to_owned(),
            descriptor_digest: sha256_digest(MODEL_DESCRIPTOR),
            request_operations: vec![MODEL_COMPLETE_OPERATION.to_owned()],
            operation_kinds: BTreeMap::from([(
                MODEL_COMPLETE_OPERATION.to_owned(),
                CapabilityOperationKind::Stream,
            )]),
        }],
        requires: vec![CapabilityRequirement {
            capability_id: CODEX_AUTH_CAPABILITY_ID.to_owned(),
            descriptor_version: CODEX_AUTH_DESCRIPTOR_VERSION.to_owned(),
            cardinality: RequirementCardinality::One,
        }],
        entrypoint: "default".to_owned(),
        execution_class: NATIVE_EXECUTION_CLASS.to_owned(),
        support_channel: SupportChannel::Experimental,
        trust: TrustLevel::Trusted,
        attachment: AttachmentProfile::ReplaceOne {
            consumer_instance: "agent".to_owned(),
            capability_id: MODEL_CAPABILITY_ID.to_owned(),
            descriptor_version: MODEL_DESCRIPTOR_VERSION.to_owned(),
            displaced_provider_instance: "model".to_owned(),
            allowed_displaced_packages: BTreeSet::from([FIXTURE_MODEL_PACKAGE_ID.to_owned()]),
            base_configuration_replacements: vec![BaseConfigurationReplacement {
                instance_key: "agent".to_owned(),
                allowed_package: AGENT_LOOP_PACKAGE_ID.to_owned(),
                expected_configuration: FIXTURE_AGENT_CONFIGURATION.to_owned(),
                replacement_configuration: CODEX_AGENT_CONFIGURATION.to_owned(),
            }],
        },
    }
}

fn codex_auth_profile() -> ExecutablePluginProfile {
    ExecutablePluginProfile {
        registration_id: "native-codex-auth-v1".to_owned(),
        adapter_profile: NATIVE_AUTH_PROFILE.to_owned(),
        package_id: CODEX_AUTH_PACKAGE_ID.to_owned(),
        factory_identity: OpenAiCodexAuthFactory.factory_identity(),
        configuration_schema_digest: sha256_digest(CODEX_AUTH_CONFIGURATION_SCHEMA),
        configuration: CODEX_AUTH_CONFIGURATION.to_owned(),
        provides: vec![CapabilityProfile {
            capability_id: CODEX_AUTH_CAPABILITY_ID.to_owned(),
            descriptor_version: CODEX_AUTH_DESCRIPTOR_VERSION.to_owned(),
            descriptor_digest: sha256_digest(CODEX_AUTH_DESCRIPTOR),
            request_operations: vec![CODEX_AUTH_ACCESS_OPERATION.to_owned()],
            operation_kinds: BTreeMap::new(),
        }],
        requires: Vec::new(),
        entrypoint: "default".to_owned(),
        execution_class: NATIVE_EXECUTION_CLASS.to_owned(),
        support_channel: SupportChannel::Experimental,
        trust: TrustLevel::Trusted,
        attachment: AttachmentProfile::IntraPluginOnly,
    }
}

impl AttachmentProfile {
    fn capability_edge(&self) -> Option<(&str, &str, &str)> {
        match self {
            Self::AppendMany {
                consumer_instance,
                capability_id,
                descriptor_version,
            }
            | Self::ReplaceOne {
                consumer_instance,
                capability_id,
                descriptor_version,
                ..
            } => Some((consumer_instance, capability_id, descriptor_version)),
            Self::IntraPluginOnly => None,
        }
    }
}

fn binding_template_parts<'a>(
    manifest: &'a PluginManifest,
    template: &BindingTemplate,
) -> Result<(&'a CapabilityRequirement, &'a CapabilityDeclaration), String> {
    let consumer = manifest
        .module_contributions
        .iter()
        .find(|contribution| contribution.id == template.consumer_contribution_id)
        .ok_or_else(|| {
            format!(
                "Plugin binding template consumer `{}` is absent",
                template.consumer_contribution_id
            )
        })?;
    let requirement = consumer
        .requires
        .iter()
        .find(|requirement| requirement.capability_id == template.capability_id)
        .ok_or_else(|| {
            format!(
                "Plugin binding template does not match a requirement on `{}`",
                consumer.id
            )
        })?;
    let provider_contribution = manifest
        .module_contributions
        .iter()
        .find(|contribution| contribution.id == template.provider_contribution_id)
        .ok_or_else(|| {
            format!(
                "Plugin binding template provider `{}` is absent",
                template.provider_contribution_id
            )
        })?;
    let capability = provider_contribution
        .provides
        .iter()
        .find(|provided| {
            provided.capability_id == requirement.capability_id
                && provided.descriptor_version == requirement.descriptor_version
        })
        .ok_or_else(|| {
            format!(
                "Plugin binding template provider `{}` is incompatible with `{}`",
                provider_contribution.id, requirement.capability_id
            )
        })?;
    Ok((requirement, capability))
}

fn is_canonical_json(value: &str) -> Result<bool, String> {
    let parsed: serde_json::Value = serde_json::from_str(value)
        .map_err(|error| format!("configuration is invalid JSON: {error}"))?;
    Ok(serde_json::to_string(&parsed).map_err(|error| error.to_string())? == value)
}

fn require_cardinality(
    consumer: &lenso_app_plan::ModuleInstancePlan,
    capability_id: &str,
    descriptor_version: &str,
    cardinality: CapabilityCardinality,
    profile_id: &str,
) -> Result<(), String> {
    if consumer.required_capabilities().iter().any(|requirement| {
        requirement.capability_id() == capability_id
            && requirement.descriptor_version() == descriptor_version
            && requirement.cardinality() == cardinality
    }) {
        Ok(())
    } else {
        Err(format!(
            "consumer Instance `{}` does not accept Plugin profile `{profile_id}` as a {cardinality:?} Capability {capability_id}@{descriptor_version}",
            consumer.instance_key()
        ))
    }
}

fn validate_displaced_provider(
    base_plan: &ResolvedAppPlan,
    consumer_instance: &str,
    capability_id: &str,
    descriptor_version: &str,
    displaced_provider_instance: &str,
    allowed_displaced_packages: &BTreeSet<String>,
    profile_id: &str,
) -> Result<(), String> {
    let matching_bindings = base_plan
        .capability_bindings()
        .iter()
        .filter(|binding| {
            binding.consumer_instance() == consumer_instance
                && binding.capability_id() == capability_id
                && binding.descriptor_version() == descriptor_version
        })
        .collect::<Vec<_>>();
    if !matches!(matching_bindings.as_slice(), [binding] if binding.provider_instance() == displaced_provider_instance)
    {
        return Err(format!(
            "Plugin profile `{profile_id}` does not close exactly one displaced binding to `{displaced_provider_instance}`"
        ));
    }
    let displaced = base_plan
        .module_instance(displaced_provider_instance)
        .ok_or_else(|| {
            format!(
                "Plugin profile `{profile_id}` displaced Instance `{displaced_provider_instance}` is absent"
            )
        })?;
    if !allowed_displaced_packages.contains(displaced.package_id()) {
        return Err(format!(
            "Plugin profile `{profile_id}` cannot displace package `{}`",
            displaced.package_id()
        ));
    }
    let other_consumers = base_plan.capability_bindings().iter().any(|binding| {
        binding.provider_instance() == displaced_provider_instance
            && !(binding.consumer_instance() == consumer_instance
                && binding.capability_id() == capability_id
                && binding.descriptor_version() == descriptor_version)
    });
    if other_consumers {
        return Err(format!(
            "Plugin profile `{profile_id}` cannot remove displaced Instance `{displaced_provider_instance}` while another consumer references it"
        ));
    }
    Ok(())
}

fn rejected<T>(detail: impl Into<String>) -> Result<T, ControlPlaneError> {
    Err(ControlPlaneError::AdmissionRejected {
        detail: detail.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lenso_plugin_control_plane::{CapabilityDeclaration, ImplementationVariant};

    #[test]
    fn catalog_registers_multiple_distinct_profiles_deterministically() {
        let mut second = text_tools_profile();
        second.registration_id = "native-more-text-tools-v1".to_owned();
        second.package_id = "example.more-text-tools".to_owned();
        second.factory_identity = "example.more-text-tools@1.0.0".to_owned();
        let contribution = contribution_for(&second, "test-target");
        let catalog = PluginProfileCatalog::default()
            .register(text_tools_profile())
            .unwrap()
            .register(fixture_model_profile())
            .unwrap()
            .register(codex_model_profile())
            .unwrap()
            .register(codex_auth_profile())
            .unwrap()
            .register(second)
            .unwrap();

        assert_eq!(
            catalog.profiles_for_execution_class(NATIVE_EXECUTION_CLASS),
            [
                NATIVE_AUTH_PROFILE,
                NATIVE_MODEL_PROFILE,
                NATIVE_TOOL_PROFILE
            ]
        );
        assert_eq!(
            catalog.support_channels_for_execution_class(NATIVE_EXECUTION_CLASS),
            [SupportChannel::Stable, SupportChannel::Experimental]
        );
        assert_eq!(
            catalog.trust_levels_for_execution_class(NATIVE_EXECUTION_CLASS),
            [TrustLevel::Trusted]
        );
        catalog
            .validate_contribution(&contribution, "test-target")
            .unwrap();
    }

    #[test]
    fn duplicate_profile_or_factory_is_rejected() {
        let catalog = PluginProfileCatalog::default()
            .register(text_tools_profile())
            .unwrap();
        assert!(catalog.clone().register(text_tools_profile()).is_err());
        let mut duplicate_factory = text_tools_profile();
        duplicate_factory.registration_id = "another-profile".to_owned();
        assert!(catalog.register(duplicate_factory).is_err());
    }

    fn contribution_for(profile: &ExecutablePluginProfile, target: &str) -> ModuleContribution {
        ModuleContribution {
            id: "more-text-tools".to_owned(),
            package_id: profile.package_id.clone(),
            configuration_schema_digest: profile.configuration_schema_digest.clone(),
            provides: profile
                .provides
                .iter()
                .map(|provided| CapabilityDeclaration {
                    capability_id: provided.capability_id.clone(),
                    descriptor_version: provided.descriptor_version.clone(),
                    descriptor_digest: provided.descriptor_digest.clone(),
                    request_operations: provided.request_operations.clone(),
                    operation_kinds: provided.operation_kinds.clone(),
                })
                .collect(),
            requires: profile.requires.clone(),
            implementations: vec![ImplementationVariant {
                id: "native".to_owned(),
                artifact: None,
                built_in_factory: Some(profile.factory_identity.clone()),
                entrypoint: profile.entrypoint.clone(),
                execution_class: profile.execution_class.clone(),
                targets: vec![target.to_owned()],
                profiles: vec![profile.adapter_profile.clone()],
                support_channel: profile.support_channel,
                trust: profile.trust,
            }],
            permission_request_ids: Vec::new(),
            state: None,
        }
    }
}
