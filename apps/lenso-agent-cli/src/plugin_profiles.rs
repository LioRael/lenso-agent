use std::collections::{BTreeMap, BTreeSet};

use lenso_agent_text_tools_module::{
    FACTORY_IDENTITY as TEXT_TOOLS_FACTORY_IDENTITY, PACKAGE_ID as TEXT_TOOLS_PACKAGE_ID,
};
use lenso_app_plan::{CapabilityBinding, CapabilityCardinality, ResolvedAppPlan};
use lenso_capability_agent_tool_provider::{
    CAPABILITY_ID as TOOL_PROVIDER_CAPABILITY_ID,
    CATALOG_OPERATION as TOOL_PROVIDER_CATALOG_OPERATION,
    DESCRIPTOR_VERSION as TOOL_PROVIDER_DESCRIPTOR_VERSION,
    EXECUTE_OPERATION as TOOL_PROVIDER_EXECUTE_OPERATION,
};
use lenso_plugin_control_plane::{
    ControlPlaneError, ModuleContribution, SupportChannel, TrustLevel, sha256_digest,
};

pub(crate) const NATIVE_EXECUTION_CLASS: &str = "lenso.native-rust@1";
pub(crate) const NATIVE_TOOL_PROFILE: &str = "agent-tool-provider-v1";

const EMPTY_CONFIGURATION_SCHEMA: &[u8] = br#"{"additionalProperties":false,"type":"object"}"#;
const TOOL_PROVIDER_DESCRIPTOR: &[u8] =
    include_bytes!("../../../crates/lenso-capability-agent-tool-provider/capability.json");

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CapabilityProfile {
    capability_id: String,
    descriptor_version: String,
    descriptor_digest: String,
    request_operations: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AttachmentProfile {
    consumer_instance: String,
    capability_id: String,
    descriptor_version: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExecutablePluginProfile {
    registration_id: String,
    adapter_profile: String,
    package_id: String,
    factory_identity: String,
    configuration_schema_digest: String,
    provides: Vec<CapabilityProfile>,
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
        let mut channels = Vec::new();
        for profile in self
            .profiles
            .values()
            .filter(|profile| profile.execution_class == execution_class)
        {
            if !channels.contains(&profile.support_channel) {
                channels.push(profile.support_channel);
            }
        }
        channels
    }

    pub(crate) fn trust_levels_for_execution_class(
        &self,
        execution_class: &str,
    ) -> Vec<TrustLevel> {
        let mut levels = Vec::new();
        for profile in self
            .profiles
            .values()
            .filter(|profile| profile.execution_class == execution_class)
        {
            if !levels.contains(&profile.trust) {
                levels.push(profile.trust);
            }
        }
        levels
    }

    pub(crate) fn validate_contribution(
        &self,
        contribution: &ModuleContribution,
        target: &str,
    ) -> Result<(), ControlPlaneError> {
        self.matching_profile(contribution, target).map(|_| ())
    }

    pub(crate) fn binding_for(
        &self,
        contribution: &ModuleContribution,
        target: &str,
        instance_key: &str,
        base_plan: &ResolvedAppPlan,
    ) -> Result<CapabilityBinding, String> {
        let profile = self
            .matching_profile(contribution, target)
            .map_err(|error| format!("Plugin profile selection failed: {error}"))?;
        let attachment = &profile.attachment;
        let consumer = base_plan
            .module_instance(&attachment.consumer_instance)
            .ok_or_else(|| {
                format!(
                    "Plugin profile `{}` requires consumer Instance `{}`",
                    profile.registration_id, attachment.consumer_instance
                )
            })?;
        if !consumer.required_capabilities().iter().any(|requirement| {
            requirement.capability_id() == attachment.capability_id
                && requirement.descriptor_version() == attachment.descriptor_version
                && requirement.cardinality() == CapabilityCardinality::Many
        }) {
            return Err(format!(
                "consumer Instance `{}` does not accept Plugin profile `{}` as a many Capability {}@{}",
                attachment.consumer_instance,
                profile.registration_id,
                attachment.capability_id,
                attachment.descriptor_version
            ));
        }
        Ok(CapabilityBinding::new(
            &attachment.consumer_instance,
            &attachment.capability_id,
            &attachment.descriptor_version,
            instance_key,
        ))
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
            || self.provides.is_empty()
            || self.entrypoint.is_empty()
            || self.execution_class.is_empty()
            || self.attachment.consumer_instance.is_empty()
        {
            return Err("Plugin profile fields must be non-empty".to_owned());
        }
        let matching_provision = self.provides.iter().filter(|provided| {
            provided.capability_id == self.attachment.capability_id
                && provided.descriptor_version == self.attachment.descriptor_version
        });
        if matching_provision.count() != 1 {
            return Err(format!(
                "Plugin profile `{}` attachment must select exactly one provided Capability",
                self.registration_id
            ));
        }
        Ok(())
    }

    fn matches(&self, contribution: &ModuleContribution, target: &str) -> bool {
        contribution.package_id == self.package_id
            && contribution.configuration_schema_digest == self.configuration_schema_digest
            && contribution.requires.is_empty()
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
    PluginProfileCatalog::default().register(text_tools_profile())
}

fn text_tools_profile() -> ExecutablePluginProfile {
    ExecutablePluginProfile {
        registration_id: "native-text-tools-v1".to_owned(),
        adapter_profile: NATIVE_TOOL_PROFILE.to_owned(),
        package_id: TEXT_TOOLS_PACKAGE_ID.to_owned(),
        factory_identity: TEXT_TOOLS_FACTORY_IDENTITY.to_owned(),
        configuration_schema_digest: sha256_digest(EMPTY_CONFIGURATION_SCHEMA),
        provides: vec![CapabilityProfile {
            capability_id: TOOL_PROVIDER_CAPABILITY_ID.to_owned(),
            descriptor_version: TOOL_PROVIDER_DESCRIPTOR_VERSION.to_owned(),
            descriptor_digest: sha256_digest(TOOL_PROVIDER_DESCRIPTOR),
            request_operations: vec![
                TOOL_PROVIDER_CATALOG_OPERATION.to_owned(),
                TOOL_PROVIDER_EXECUTE_OPERATION.to_owned(),
            ],
        }],
        entrypoint: "default".to_owned(),
        execution_class: NATIVE_EXECUTION_CLASS.to_owned(),
        support_channel: SupportChannel::Stable,
        trust: TrustLevel::Trusted,
        attachment: AttachmentProfile {
            consumer_instance: "tools".to_owned(),
            capability_id: TOOL_PROVIDER_CAPABILITY_ID.to_owned(),
            descriptor_version: TOOL_PROVIDER_DESCRIPTOR_VERSION.to_owned(),
        },
    }
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
            .register(second)
            .unwrap();

        assert_eq!(
            catalog.profiles_for_execution_class(NATIVE_EXECUTION_CLASS),
            [NATIVE_TOOL_PROFILE]
        );
        assert_eq!(
            catalog.support_channels_for_execution_class(NATIVE_EXECUTION_CLASS),
            [SupportChannel::Stable]
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
                })
                .collect(),
            requires: Vec::new(),
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
