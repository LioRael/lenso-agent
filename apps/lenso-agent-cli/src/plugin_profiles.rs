use std::collections::{BTreeMap, BTreeSet};

use lenso_agent_auth_openai_codex_module::{
    FACTORY_IDENTITY as CODEX_AUTH_FACTORY_IDENTITY, PACKAGE_ID as CODEX_AUTH_PACKAGE_ID,
};
use lenso_agent_http_fetch_module::PACKAGE_ID as HTTP_FETCH_PACKAGE_ID;
use lenso_agent_loop_module::PACKAGE_ID as AGENT_LOOP_PACKAGE_ID;
use lenso_agent_model_fixture_module::{
    FACTORY_IDENTITY as FIXTURE_MODEL_FACTORY_IDENTITY, MODEL_ID as FIXTURE_MODEL_ID,
    PACKAGE_ID as FIXTURE_MODEL_PACKAGE_ID,
};
use lenso_agent_model_openai_codex_direct_module::{
    FACTORY_IDENTITY as CODEX_MODEL_FACTORY_IDENTITY, PACKAGE_ID as CODEX_MODEL_PACKAGE_ID,
};
use lenso_agent_model_openai_compatible_module::{
    FACTORY_IDENTITY as OPENAI_MODEL_FACTORY_IDENTITY, PACKAGE_ID as OPENAI_MODEL_PACKAGE_ID,
};
use lenso_agent_process_native_module::{
    FACTORY_IDENTITY as PROCESS_NATIVE_FACTORY_IDENTITY, PACKAGE_ID as PROCESS_NATIVE_PACKAGE_ID,
};
use lenso_agent_process_tools_module::{
    FACTORY_IDENTITY as PROCESS_TOOLS_FACTORY_IDENTITY, PACKAGE_ID as PROCESS_TOOLS_PACKAGE_ID,
};
use lenso_agent_skills_filesystem_module::{
    FACTORY_IDENTITY as SKILLS_FACTORY_IDENTITY, PACKAGE_ID as SKILLS_PACKAGE_ID,
};
use lenso_agent_subagent_tools_module::{
    FACTORY_IDENTITY as SUBAGENT_TOOLS_FACTORY_IDENTITY, PACKAGE_ID as SUBAGENT_TOOLS_PACKAGE_ID,
};
use lenso_agent_text_tools_module::{
    FACTORY_IDENTITY as TEXT_TOOLS_FACTORY_IDENTITY, PACKAGE_ID as TEXT_TOOLS_PACKAGE_ID,
};
use lenso_agent_workspace_edit_module::{
    FACTORY_IDENTITY as WORKSPACE_EDIT_FACTORY_IDENTITY, PACKAGE_ID as WORKSPACE_EDIT_PACKAGE_ID,
};
use lenso_app_plan::{
    CapabilityBinding, CapabilityCardinality, CapabilityOperationKind, ResolvedAppPlan,
};
use lenso_capability_agent::{
    CAPABILITY_ID as AGENT_CAPABILITY_ID, DESCRIPTOR_VERSION as AGENT_DESCRIPTOR_VERSION,
    RUN_TURN_OPERATION,
};
use lenso_capability_agent_auth_openai_codex::{
    ACCESS_OPERATION as CODEX_AUTH_ACCESS_OPERATION, CAPABILITY_ID as CODEX_AUTH_CAPABILITY_ID,
    DESCRIPTOR_VERSION as CODEX_AUTH_DESCRIPTOR_VERSION,
};
use lenso_capability_agent_http_fetch::{
    CAPABILITY_ID as HTTP_FETCH_CAPABILITY_ID, DESCRIPTOR_VERSION as HTTP_FETCH_DESCRIPTOR_VERSION,
};
use lenso_capability_agent_model::{
    CAPABILITY_ID as MODEL_CAPABILITY_ID, COMPLETE_OPERATION as MODEL_COMPLETE_OPERATION,
    DESCRIPTOR_VERSION as MODEL_DESCRIPTOR_VERSION,
};
use lenso_capability_agent_process::{
    CAPABILITY_ID as PROCESS_CAPABILITY_ID, CATALOG_OPERATION as PROCESS_CATALOG_OPERATION,
    DESCRIPTOR_VERSION as PROCESS_DESCRIPTOR_VERSION, RUN_OPERATION as PROCESS_RUN_OPERATION,
};
use lenso_capability_agent_prompt::{
    CAPABILITY_ID as PROMPT_CAPABILITY_ID, DESCRIPTOR_VERSION as PROMPT_DESCRIPTOR_VERSION,
};
use lenso_capability_agent_prompt_provider::{
    CAPABILITY_ID as PROMPT_PROVIDER_CAPABILITY_ID,
    CONTRIBUTE_OPERATION as PROMPT_PROVIDER_CONTRIBUTE_OPERATION,
    DESCRIPTOR_VERSION as PROMPT_PROVIDER_DESCRIPTOR_VERSION,
};
use lenso_capability_agent_session::{
    CAPABILITY_ID as SESSION_CAPABILITY_ID, DESCRIPTOR_VERSION as SESSION_DESCRIPTOR_VERSION,
};
use lenso_capability_agent_tool_provider::{
    CAPABILITY_ID as TOOL_PROVIDER_CAPABILITY_ID,
    CATALOG_OPERATION as TOOL_PROVIDER_CATALOG_OPERATION,
    DESCRIPTOR_VERSION as TOOL_PROVIDER_DESCRIPTOR_VERSION,
    EXECUTE_OPERATION as TOOL_PROVIDER_EXECUTE_OPERATION,
};
use lenso_capability_agent_tools::{
    CAPABILITY_ID as TOOLS_CAPABILITY_ID, DESCRIPTOR_VERSION as TOOLS_DESCRIPTOR_VERSION,
};
use lenso_capability_agent_workspace_read::{
    CAPABILITY_ID as WORKSPACE_READ_CAPABILITY_ID,
    DESCRIPTOR_VERSION as WORKSPACE_READ_DESCRIPTOR_VERSION,
};
use lenso_capability_secrets::{
    CAPABILITY_ID as SECRETS_CAPABILITY_ID, DESCRIPTOR_VERSION as SECRETS_DESCRIPTOR_VERSION,
    RESOLVE_OPERATION as SECRETS_RESOLVE_OPERATION,
};
use lenso_plugin_control_plane::{
    ApprovedGrant, BindingTemplate, CapabilityDeclaration, CapabilityRequirement,
    ControlPlaneError, EnforcementKind, ModuleContribution, PermissionRequest, PluginManifest,
    RequirementCardinality, SupportChannel, TrustLevel, sha256_digest,
};

pub(crate) const NATIVE_EXECUTION_CLASS: &str = "lenso.native-rust@1";
pub(crate) const TOOL_PROVIDER_PROFILE: &str = "agent-tool-provider-v2";
pub(crate) const NATIVE_MODEL_PROFILE: &str = "agent-model-provider-v1";
pub(crate) const NATIVE_AUTH_PROFILE: &str = "agent-auth-provider-v1";
pub(crate) const NATIVE_SKILLS_PROFILE: &str = "agent-skills-provider-v1";
pub(crate) const NATIVE_PROCESS_PROFILE: &str = "agent-process-provider-v1";
pub(crate) const NATIVE_SECRETS_PROFILE: &str = "secrets-provider-v1";
pub(crate) const AGENT_PROVIDER_PROFILE: &str = "agent-provider-v1";
pub(crate) const SUBAGENT_PROVIDER_PROFILE: &str = "agent-subagent-provider-v1";
pub(crate) const QUICKJS_EXECUTION_CLASS: &str = "lenso.quickjs@1";
pub(crate) const WASM_EXECUTION_CLASS: &str = "lenso.wasm-component@1";

const EMPTY_CONFIGURATION_SCHEMA: &[u8] = br#"{"additionalProperties":false,"type":"object"}"#;
const TOOL_PROVIDER_DESCRIPTOR: &[u8] =
    include_bytes!("../../../crates/lenso-capability-agent-tool-provider/capability.json");
const MODEL_DESCRIPTOR: &[u8] =
    include_bytes!("../../../crates/lenso-capability-agent-model/capability.json");
const FIXTURE_MODEL_CONFIGURATION_SCHEMA: &[u8] =
    include_bytes!("../../../crates/lenso-agent-model-fixture-module/config.schema.json");
const WORKSPACE_EDIT_CONFIGURATION_SCHEMA: &[u8] =
    include_bytes!("../../../crates/lenso-agent-workspace-edit-module/config.schema.json");
const SKILLS_CONFIGURATION_SCHEMA: &[u8] =
    include_bytes!("../../../crates/lenso-agent-skills-filesystem-module/config.schema.json");
const PROCESS_TOOLS_CONFIGURATION_SCHEMA: &[u8] =
    include_bytes!("../../../crates/lenso-agent-process-tools-module/config.schema.json");
const PROCESS_NATIVE_CONFIGURATION_SCHEMA: &[u8] =
    include_bytes!("../../../crates/lenso-agent-process-native-module/config.schema.json");
const SUBAGENT_TOOLS_CONFIGURATION_SCHEMA: &[u8] =
    include_bytes!("../../../crates/lenso-agent-subagent-tools-module/config.schema.json");
const OPENAI_MODEL_CONFIGURATION_SCHEMA: &[u8] =
    include_bytes!("../../../crates/lenso-agent-model-openai-compatible-module/config.schema.json");
const PROMPT_PROVIDER_DESCRIPTOR: &[u8] =
    include_bytes!("../../../crates/lenso-capability-agent-prompt-provider/capability.json");
const PROCESS_DESCRIPTOR: &[u8] =
    include_bytes!("../../../crates/lenso-capability-agent-process/capability.json");
const CODEX_MODEL_CONFIGURATION_SCHEMA: &[u8] = include_bytes!(
    "../../../crates/lenso-agent-model-openai-codex-direct-module/config.schema.json"
);
const CODEX_AUTH_CONFIGURATION_SCHEMA: &[u8] =
    include_bytes!("../../../crates/lenso-agent-auth-openai-codex-module/config.schema.json");
const CODEX_AUTH_DESCRIPTOR: &[u8] =
    include_bytes!("../../../crates/lenso-capability-agent-auth-openai-codex/capability.json");
const AGENT_DESCRIPTOR: &[u8] =
    include_bytes!("../../../crates/lenso-capability-agent/capability.json");
const GUEST_AGENT_PACKAGE_ID: &str = "lenso.agent.guest";
const WORKSPACE_READ_PACKAGE_ID: &str = "lenso.agent.workspace-import-read";
const WORKSPACE_READ_INSTANCE: &str = "workspace-import-read";
const HTTP_FETCH_INSTANCE: &str = "http-fetch";
const FIXTURE_AGENT_CONFIGURATION: &str = r#"{"model":"fixture/readme-summary-v1","max_steps":8,"max_tool_calls":4,"max_parallel_tool_calls":4,"max_output_tokens":1024,"max_history_events":200}"#;
const CODEX_AGENT_CONFIGURATION: &str = r#"{"max_history_events":200,"max_output_tokens":1024,"max_steps":8,"max_tool_calls":4,"max_parallel_tool_calls":4,"model":"gpt-5.6-luna"}"#;
const CODEX_MODEL_CONFIGURATION: &str = r#"{"base_url":"https://chatgpt.com/backend-api","max_event_bytes":1048576,"model":"gpt-5.6-luna","reasoning_effort":"medium"}"#;
const CODEX_AUTH_CONFIGURATION: &str =
    r#"{"issuer":"https://auth.openai.com","profile":"default","refresh_margin_seconds":60}"#;
const WORKSPACE_EDIT_CONFIGURATION: &str =
    r#"{"max_edit_bytes":131072,"max_file_bytes":1048576,"root":"."}"#;
const SKILLS_CONFIGURATION: &str = r#"{"catalog_contribution_id":"agents.skills.catalog","max_catalog_bytes":262144,"max_file_bytes":262144,"max_prompt_catalog_bytes":8000,"max_resource_entries":8192,"max_resource_file_bytes":262144,"max_resource_manifest_bytes":524288,"max_resource_total_bytes":16777216,"max_skills":256,"max_total_bytes":8388608,"root":"~/.agents/skills"}"#;
const PROCESS_TOOLS_CONFIGURATION: &str = r#"{"default_timeout_ms":120000}"#;
const PROCESS_NATIVE_CONFIGURATION: &str = r#"{"allowed_programs":["cargo","git","rg"],"environment_allowlist":["PATH","HOME","CARGO_HOME","RUSTUP_HOME","TMPDIR","LANG","LC_ALL"],"max_argument_bytes":131072,"max_output_bytes":262144,"max_timeout_ms":600000,"root":"."}"#;
const SUBAGENT_TOOLS_CONFIGURATION: &str =
    r#"{"max_output_bytes":1048576,"max_task_bytes":262144}"#;
const OPENAI_MODEL_CONFIGURATION: &str = r#"{"api_key_ref":"model/openai-api-key","base_url":"https://api.openai.com/v1","model":"gpt-4o-mini"}"#;
const OPENAI_AGENT_CONFIGURATION: &str = r#"{"max_history_events":200,"max_output_tokens":1024,"max_steps":8,"max_tool_calls":4,"max_parallel_tool_calls":4,"model":"gpt-4o-mini"}"#;
const SECRETS_CONFIGURATION: &str = r#"{"references":{"model/openai-api-key":"OPENAI_API_KEY"}}"#;
const SECRETS_PACKAGE_ID: &str = "lenso.secrets.env";
const SECRETS_FACTORY_IDENTITY: &str = "lenso.secrets.env@0.1.1";
const SECRETS_CONFIGURATION_SCHEMA_DIGEST: &str =
    "sha256:2fafb2e087e788ab1a9f52b5b3cb9f050a79a7e9d6309f369477f60de428faa2";
const SECRETS_DESCRIPTOR_DIGEST: &str =
    "sha256:c45e1c4ea7e77a0ba367d573f092c745d552c489ae0db6828cc6f218763ecf05";

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
        max_concurrency: usize,
    },
    AppendManySet {
        edges: Vec<AttachmentEdge>,
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
pub(crate) struct AttachmentEdge {
    consumer_instance: String,
    capability_id: String,
    descriptor_version: String,
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
    AppendManySet(Vec<CapabilityBinding>),
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
    package: PackagePolicy,
    authority: ImplementationAuthority,
    configuration_schema_digest: String,
    configuration: String,
    provides: Vec<CapabilityProfile>,
    requires: Vec<CapabilityRequirement>,
    entrypoint: String,
    execution_class: String,
    support_channel: SupportChannel,
    trust: TrustLevel,
    attachment: AttachmentProfile,
    permission_requests: Vec<PermissionProfile>,
    fixed_host_imports: Vec<FixedHostImport>,
    inherit_displaced_requirements: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FixedHostImport {
    capability_id: String,
    descriptor_version: String,
    provider_instance: String,
    allowed_provider_packages: BTreeSet<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PermissionProfile {
    request_id: String,
    resource_kind: String,
    required: bool,
    scope_policy: PermissionScopePolicy,
    enforcement_kind: EnforcementKind,
    enforcer_identity: String,
    provider_instance: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PermissionScopePolicy {
    HttpOrigins,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ImplementationAuthority {
    BuiltIn { factory_identity: String },
    Artifact,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PackagePolicy {
    Exact(String),
    AnyNonEmpty,
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
        if profile.built_in_factory().is_some_and(|factory| {
            self.profiles
                .values()
                .any(|registered| registered.built_in_factory() == Some(factory))
        }) {
            return Err(format!(
                "Plugin factory `{}` has more than one attachment profile",
                profile.built_in_factory().expect("checked above")
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
        let mut expected_permission_ids = BTreeSet::new();
        for contribution in &manifest.module_contributions {
            let profile = self.matching_profile(contribution, target)?;
            for permission in &profile.permission_requests {
                if !expected_permission_ids.insert(permission.request_id.as_str()) {
                    return rejected(format!(
                        "permission request `{}` is assigned to more than one contribution",
                        permission.request_id
                    ));
                }
                let request = manifest
                    .permission_requests
                    .iter()
                    .find(|request| request.id == permission.request_id)
                    .ok_or_else(|| ControlPlaneError::AdmissionRejected {
                        detail: format!(
                            "Plugin contribution `{}` is missing permission request `{}`",
                            contribution.id, permission.request_id
                        ),
                    })?;
                permission.validate_request(request)?;
            }
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
                AttachmentProfile::AppendMany { .. } | AttachmentProfile::AppendManySet { .. } => {}
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
                let may_import_from_host = profile.permits_host_requirement(requirement);
                if count > 1 || (count == 0 && !may_import_from_host) {
                    return rejected(format!(
                        "Plugin contribution `{}` does not close exactly one binding for `{}`",
                        contribution.id, requirement.capability_id
                    ));
                }
            }
        }
        let actual_permission_ids = manifest
            .permission_requests
            .iter()
            .map(|request| request.id.as_str())
            .collect::<BTreeSet<_>>();
        if actual_permission_ids != expected_permission_ids
            || actual_permission_ids.len() != manifest.permission_requests.len()
        {
            return rejected(
                "Plugin permission requests must exactly match registered contribution profiles",
            );
        }
        Ok(())
    }

    pub(crate) fn approved_grants_for(
        &self,
        manifest: &PluginManifest,
        selected_contribution_ids: &[String],
        target: &str,
    ) -> Result<Vec<ApprovedGrant>, ControlPlaneError> {
        let mut grants = Vec::new();
        for contribution_id in selected_contribution_ids {
            let contribution = manifest
                .module_contributions
                .iter()
                .find(|contribution| &contribution.id == contribution_id)
                .ok_or_else(|| ControlPlaneError::AdmissionRejected {
                    detail: format!("selected contribution `{contribution_id}` is missing"),
                })?;
            let profile = self.matching_profile(contribution, target)?;
            for permission in &profile.permission_requests {
                let request = manifest
                    .permission_requests
                    .iter()
                    .find(|request| request.id == permission.request_id)
                    .ok_or_else(|| ControlPlaneError::AdmissionRejected {
                        detail: format!(
                            "permission request `{}` is missing",
                            permission.request_id
                        ),
                    })?;
                permission.validate_request(request)?;
                grants.push(ApprovedGrant {
                    instance_key: plugin_instance_key(&manifest.plugin_id, contribution_id),
                    permission_request_id: request.id.clone(),
                    scope: request.scope.clone(),
                    enforcement_kind: permission.enforcement_kind,
                    enforcer_identity: permission.enforcer_identity.clone(),
                });
            }
        }
        grants.sort_by(|left, right| {
            (&left.instance_key, &left.permission_request_id)
                .cmp(&(&right.instance_key, &right.permission_request_id))
        });
        Ok(grants)
    }

    pub(crate) fn validate_permission_enforcement(
        &self,
        contribution: &ModuleContribution,
        target: &str,
        consumer_instance: &str,
        grants: &[ApprovedGrant],
        base_plan: &ResolvedAppPlan,
    ) -> Result<(), String> {
        let profile = self
            .matching_profile(contribution, target)
            .map_err(|error| format!("Plugin profile selection failed: {error}"))?;
        for permission in &profile.permission_requests {
            let matching = grants
                .iter()
                .filter(|grant| {
                    grant.instance_key == consumer_instance
                        && grant.permission_request_id == permission.request_id
                })
                .collect::<Vec<_>>();
            let [grant] = matching.as_slice() else {
                return Err(format!(
                    "Plugin Instance `{consumer_instance}` does not have exactly one effective `{}` grant",
                    permission.request_id
                ));
            };
            if grant.enforcement_kind != permission.enforcement_kind
                || grant.enforcer_identity != permission.enforcer_identity
            {
                return Err(format!(
                    "Plugin Instance `{consumer_instance}` permission grant has the wrong enforcer"
                ));
            }
            let provider = base_plan
                .module_instance(&permission.provider_instance)
                .ok_or_else(|| {
                    format!(
                        "permission enforcer Instance `{}` is absent from the App",
                        permission.provider_instance
                    )
                })?;
            permission.validate_provider_configuration(provider.configuration(), &grant.scope)?;
        }
        Ok(())
    }

    pub(crate) fn automatic_local_admission(
        &self,
        manifest: &PluginManifest,
        selected_contribution_ids: &[String],
        target: &str,
    ) -> Result<Option<&'static str>, ControlPlaneError> {
        if selected_contribution_ids.is_empty() {
            return Ok(Some("automatic:local-passive-release"));
        }
        for contribution_id in selected_contribution_ids {
            let contribution = manifest
                .module_contributions
                .iter()
                .find(|contribution| &contribution.id == contribution_id)
                .ok_or_else(|| ControlPlaneError::AdmissionRejected {
                    detail: format!(
                        "selected Module contribution `{contribution_id}` is missing from the Manifest"
                    ),
                })?;
            let profile = self.matching_profile(contribution, target)?;
            if !matches!(profile.attachment, AttachmentProfile::AppendMany { .. })
                || profile.support_channel != SupportChannel::Stable
                || profile.trust != TrustLevel::Trusted
                || !profile.requires.is_empty()
                || contribution.state.is_some()
                || !contribution.permission_request_ids.is_empty()
                || contribution
                    .implementations
                    .iter()
                    .any(|implementation| implementation.artifact.is_some())
            {
                return Ok(None);
            }
        }
        Ok(Some("automatic:local-trusted-stateless-append-many"))
    }

    pub(crate) fn permits_host_requirements(
        &self,
        contribution: &ModuleContribution,
        target: &str,
    ) -> Result<bool, ControlPlaneError> {
        self.matching_profile(contribution, target).map(|profile| {
            profile.inherit_displaced_requirements || !profile.fixed_host_imports.is_empty()
        })
    }

    pub(crate) fn fixed_host_bindings_for(
        &self,
        contribution: &ModuleContribution,
        target: &str,
        consumer_instance: &str,
        base_plan: &ResolvedAppPlan,
    ) -> Result<Vec<CapabilityBinding>, String> {
        let profile = self
            .matching_profile(contribution, target)
            .map_err(|error| format!("Plugin profile selection failed: {error}"))?;
        profile
            .fixed_host_imports
            .iter()
            .map(|host_import| {
                let provider = base_plan
                    .module_instance(&host_import.provider_instance)
                    .ok_or_else(|| {
                        format!(
                            "Plugin profile `{}` requires Host provider Instance `{}`",
                            profile.registration_id, host_import.provider_instance
                        )
                    })?;
                if !host_import
                    .allowed_provider_packages
                    .contains(provider.package_id())
                {
                    return Err(format!(
                        "Plugin profile `{}` cannot import Host provider package `{}`",
                        profile.registration_id,
                        provider.package_id()
                    ));
                }
                let matching_endpoints = provider
                    .provided_capabilities()
                    .iter()
                    .filter(|provided| {
                        provided.capability_id() == host_import.capability_id
                            && provided.descriptor_version() == host_import.descriptor_version
                    })
                    .count();
                if matching_endpoints != 1 {
                    return Err(format!(
                        "Plugin profile `{}` Host provider `{}` does not expose exactly one `{}@{}` endpoint",
                        profile.registration_id,
                        host_import.provider_instance,
                        host_import.capability_id,
                        host_import.descriptor_version
                    ));
                }
                Ok(CapabilityBinding::new(
                    consumer_instance,
                    &host_import.capability_id,
                    &host_import.descriptor_version,
                    &host_import.provider_instance,
                ))
            })
            .collect()
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
        match &profile.attachment {
            AttachmentProfile::AppendMany {
                consumer_instance,
                capability_id,
                descriptor_version,
                max_concurrency,
            } => {
                let consumer =
                    require_consumer(base_plan, consumer_instance, &profile.registration_id)?;
                require_cardinality(
                    consumer,
                    capability_id,
                    descriptor_version,
                    CapabilityCardinality::Many,
                    &profile.registration_id,
                )?;
                Ok(ResolvedAttachment::AppendMany(
                    CapabilityBinding::new(
                        consumer_instance,
                        capability_id,
                        descriptor_version,
                        instance_key,
                    )
                    .with_limits(0, *max_concurrency),
                ))
            }
            AttachmentProfile::AppendManySet { edges } => {
                let bindings = edges
                    .iter()
                    .map(|edge| {
                        let consumer = require_consumer(
                            base_plan,
                            &edge.consumer_instance,
                            &profile.registration_id,
                        )?;
                        require_cardinality(
                            consumer,
                            &edge.capability_id,
                            &edge.descriptor_version,
                            CapabilityCardinality::Many,
                            &profile.registration_id,
                        )?;
                        Ok(CapabilityBinding::new(
                            &edge.consumer_instance,
                            &edge.capability_id,
                            &edge.descriptor_version,
                            instance_key,
                        ))
                    })
                    .collect::<Result<Vec<_>, String>>()?;
                Ok(ResolvedAttachment::AppendManySet(bindings))
            }
            AttachmentProfile::ReplaceOne {
                consumer_instance,
                capability_id,
                descriptor_version,
                displaced_provider_instance,
                allowed_displaced_packages,
                base_configuration_replacements,
                ..
            } => {
                let consumer =
                    require_consumer(base_plan, consumer_instance, &profile.registration_id)?;
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
                    binding: CapabilityBinding::new(
                        consumer_instance,
                        capability_id,
                        descriptor_version,
                        instance_key,
                    ),
                    displaced_provider_instance: displaced_provider_instance.clone(),
                    base_configuration_replacements: base_configuration_replacements.clone(),
                })
            }
            AttachmentProfile::IntraPluginOnly => Ok(ResolvedAttachment::IntraPluginOnly),
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
    fn built_in_factory(&self) -> Option<&str> {
        match &self.authority {
            ImplementationAuthority::BuiltIn { factory_identity } => Some(factory_identity),
            ImplementationAuthority::Artifact => None,
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one profile invariant audit keeps all executable authority checks together"
    )]
    fn validate(&self) -> Result<(), String> {
        if self.registration_id.is_empty()
            || self.adapter_profile.is_empty()
            || matches!(&self.package, PackagePolicy::Exact(package_id) if package_id.is_empty())
            || self.configuration_schema_digest.is_empty()
            || self.configuration.is_empty()
            || self.provides.is_empty()
            || self.entrypoint.is_empty()
            || self.execution_class.is_empty()
        {
            return Err("Plugin profile fields must be non-empty".to_owned());
        }
        if matches!(
            &self.authority,
            ImplementationAuthority::BuiltIn { factory_identity } if factory_identity.is_empty()
        ) {
            return Err("built-in Plugin profile factory identity must be non-empty".to_owned());
        }
        self.validate_attachment()?;
        self.validate_configuration()
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one profile invariant audit keeps all attachment authority checks together"
    )]
    fn validate_attachment(&self) -> Result<(), String> {
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
        if let AttachmentProfile::AppendManySet { edges } = &self.attachment {
            if edges.len() < 2 {
                return Err(format!(
                    "Plugin profile `{}` multi-attachment policy requires at least two edges",
                    self.registration_id
                ));
            }
            let mut unique_edges = BTreeSet::new();
            for edge in edges {
                if edge.consumer_instance.is_empty()
                    || edge.capability_id.is_empty()
                    || edge.descriptor_version.is_empty()
                    || !unique_edges.insert((
                        edge.consumer_instance.as_str(),
                        edge.capability_id.as_str(),
                        edge.descriptor_version.as_str(),
                    ))
                    || self
                        .provides
                        .iter()
                        .filter(|provided| {
                            provided.capability_id == edge.capability_id
                                && provided.descriptor_version == edge.descriptor_version
                        })
                        .count()
                        != 1
                {
                    return Err(format!(
                        "Plugin profile `{}` has an invalid multi-attachment edge",
                        self.registration_id
                    ));
                }
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
        if self.inherit_displaced_requirements
            && !matches!(self.attachment, AttachmentProfile::ReplaceOne { .. })
        {
            return Err(format!(
                "Plugin profile `{}` may inherit Host requirements only when replacing one provider",
                self.registration_id
            ));
        }
        self.validate_host_import_policy()?;
        let mut permission_ids = BTreeSet::new();
        for permission in &self.permission_requests {
            if permission.request_id.is_empty()
                || permission.resource_kind.is_empty()
                || permission.enforcer_identity.is_empty()
                || permission.provider_instance.is_empty()
                || !permission_ids.insert(permission.request_id.as_str())
                || !self.fixed_host_imports.iter().any(|host_import| {
                    host_import.provider_instance == permission.provider_instance
                })
            {
                return Err(format!(
                    "Plugin profile `{}` permission policy is invalid",
                    self.registration_id
                ));
            }
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
        Ok(())
    }

    fn validate_configuration(&self) -> Result<(), String> {
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

    fn validate_host_import_policy(&self) -> Result<(), String> {
        if self.inherit_displaced_requirements && !self.fixed_host_imports.is_empty() {
            return Err(format!(
                "Plugin profile `{}` cannot combine inherited and fixed Host imports",
                self.registration_id
            ));
        }
        let mut imported_capabilities = BTreeSet::new();
        for host_import in &self.fixed_host_imports {
            if host_import.capability_id.is_empty()
                || host_import.descriptor_version.is_empty()
                || host_import.provider_instance.is_empty()
                || host_import.allowed_provider_packages.is_empty()
                || !imported_capabilities.insert((
                    host_import.capability_id.as_str(),
                    host_import.descriptor_version.as_str(),
                ))
                || !self.requires.iter().any(|requirement| {
                    requirement.capability_id == host_import.capability_id
                        && requirement.descriptor_version == host_import.descriptor_version
                        && requirement.cardinality == RequirementCardinality::One
                })
            {
                return Err(format!(
                    "Plugin profile `{}` fixed Host import policy is invalid",
                    self.registration_id
                ));
            }
        }
        Ok(())
    }

    fn matches(&self, contribution: &ModuleContribution, target: &str) -> bool {
        self.package.matches(&contribution.package_id)
            && contribution.configuration_schema_digest == self.configuration_schema_digest
            && requirements_match(&contribution.requires, &self.requires)
            && permission_ids_match(
                &contribution.permission_request_ids,
                &self.permission_requests,
            )
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
                let authority_matches = match &self.authority {
                    ImplementationAuthority::BuiltIn { factory_identity } => {
                        implementation.artifact.is_none()
                            && implementation.built_in_factory.as_deref()
                                == Some(factory_identity.as_str())
                    }
                    ImplementationAuthority::Artifact => {
                        implementation.artifact.is_some()
                            && implementation.built_in_factory.is_none()
                    }
                };
                authority_matches
                    && implementation.entrypoint == self.entrypoint
                    && implementation.execution_class == self.execution_class
                    && implementation.profiles == [self.adapter_profile.as_str()]
                    && implementation.support_channel == self.support_channel
                    && implementation.trust == self.trust
            })
    }

    fn permits_host_requirement(&self, requirement: &CapabilityRequirement) -> bool {
        self.inherit_displaced_requirements
            || self.fixed_host_imports.iter().any(|host_import| {
                host_import.capability_id == requirement.capability_id
                    && host_import.descriptor_version == requirement.descriptor_version
                    && requirement.cardinality == RequirementCardinality::One
            })
    }
}

impl PackagePolicy {
    fn matches(&self, package_id: &str) -> bool {
        match self {
            Self::Exact(expected) => package_id == expected,
            Self::AnyNonEmpty => !package_id.is_empty(),
        }
    }
}

impl PermissionProfile {
    fn validate_request(&self, request: &PermissionRequest) -> Result<(), ControlPlaneError> {
        if request.id != self.request_id
            || request.resource_kind != self.resource_kind
            || request.required != self.required
            || request.explanation_key.trim().is_empty()
        {
            return rejected(format!(
                "permission request `{}` does not match its registered Host policy",
                request.id
            ));
        }
        match self.scope_policy {
            PermissionScopePolicy::HttpOrigins => {
                http_origins(&request.scope).map_err(|detail| {
                    ControlPlaneError::AdmissionRejected {
                        detail: format!("permission request `{}` {detail}", request.id),
                    }
                })?;
            }
        }
        Ok(())
    }

    fn validate_provider_configuration(
        &self,
        configuration: &str,
        approved_scope: &serde_json::Value,
    ) -> Result<(), String> {
        match self.scope_policy {
            PermissionScopePolicy::HttpOrigins => {
                let requested = http_origins(approved_scope)?;
                let configuration = serde_json::from_str::<serde_json::Value>(configuration)
                    .map_err(|error| format!("permission enforcer config is invalid: {error}"))?;
                let allowed = configuration
                    .get("allowed_origins")
                    .and_then(serde_json::Value::as_array)
                    .ok_or_else(|| {
                        "permission enforcer does not declare allowed_origins".to_owned()
                    })?
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .ok_or_else(|| "permission enforcer origin is not a string".to_owned())
                    })
                    .collect::<Result<BTreeSet<_>, _>>()?;
                if requested
                    .iter()
                    .any(|origin| !allowed.contains(origin.as_str()))
                {
                    return Err(
                        "approved network scope exceeds the App HTTP enforcer allowlist".to_owned(),
                    );
                }
            }
        }
        Ok(())
    }
}

fn http_origins(scope: &serde_json::Value) -> Result<Vec<String>, String> {
    let object = scope
        .as_object()
        .filter(|object| object.len() == 1)
        .ok_or_else(|| "scope must contain only `origins`".to_owned())?;
    let origins = object
        .get("origins")
        .and_then(serde_json::Value::as_array)
        .filter(|origins| !origins.is_empty() && origins.len() <= 8)
        .ok_or_else(|| "scope must contain between 1 and 8 origins".to_owned())?;
    let mut normalized = Vec::with_capacity(origins.len());
    for origin in origins {
        let origin = origin
            .as_str()
            .ok_or_else(|| "origin must be a string".to_owned())?;
        let url = reqwest::Url::parse(origin)
            .map_err(|_| format!("origin `{origin}` is not a valid URL origin"))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.origin().ascii_serialization() != origin
        {
            return Err(format!(
                "origin `{origin}` is not normalized HTTP authority"
            ));
        }
        normalized.push(origin.to_owned());
    }
    if normalized.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err("origins must be sorted and unique".to_owned());
    }
    Ok(normalized)
}

fn permission_ids_match(actual: &[String], expected: &[PermissionProfile]) -> bool {
    actual.len() == expected.len()
        && actual
            .iter()
            .zip(expected)
            .all(|(actual, expected)| actual == &expected.request_id)
}

pub(crate) fn plugin_instance_key(plugin_id: &str, contribution_id: &str) -> String {
    format!("plugin:{}:{plugin_id}:{contribution_id}", plugin_id.len())
}

fn requirements_match(
    actual: &[CapabilityRequirement],
    expected: &[CapabilityRequirement],
) -> bool {
    actual.len() == expected.len()
        && actual
            .iter()
            .all(|requirement| expected.contains(requirement))
        && expected
            .iter()
            .all(|requirement| actual.contains(requirement))
}

pub(crate) fn harness_plugin_profiles() -> Result<PluginProfileCatalog, String> {
    PluginProfileCatalog::default()
        .register(text_tools_profile())?
        .register(workspace_edit_profile())?
        .register(skills_profile())?
        .register(process_native_profile())?
        .register(process_tools_profile())?
        .register(subagent_tools_profile())?
        .register(fixture_model_profile())?
        .register(openai_model_profile())?
        .register(secrets_profile())?
        .register(codex_model_profile())?
        .register(codex_auth_profile())?
        .register(guest_agent_profile(
            "quickjs-agent-provider-v1",
            QUICKJS_EXECUTION_CLASS,
            "plugin.mjs",
            TrustLevel::Constrained,
        ))?
        .register(guest_agent_profile(
            "wasm-agent-provider-v1",
            WASM_EXECUTION_CLASS,
            "plugin",
            TrustLevel::Isolated,
        ))?
        .register(third_party_wasm_tool_profile())?
        .register(third_party_wasm_workspace_read_tool_profile())?
        .register(third_party_wasm_http_fetch_tool_profile())
}

fn subagent_tools_profile() -> ExecutablePluginProfile {
    ExecutablePluginProfile {
        registration_id: "native-subagent-tools-v1".to_owned(),
        adapter_profile: SUBAGENT_PROVIDER_PROFILE.to_owned(),
        package: PackagePolicy::Exact(SUBAGENT_TOOLS_PACKAGE_ID.to_owned()),
        authority: ImplementationAuthority::BuiltIn {
            factory_identity: SUBAGENT_TOOLS_FACTORY_IDENTITY.to_owned(),
        },
        configuration_schema_digest: sha256_digest(SUBAGENT_TOOLS_CONFIGURATION_SCHEMA),
        configuration: SUBAGENT_TOOLS_CONFIGURATION.to_owned(),
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
        requires: vec![CapabilityRequirement {
            capability_id: AGENT_CAPABILITY_ID.to_owned(),
            descriptor_version: AGENT_DESCRIPTOR_VERSION.to_owned(),
            cardinality: RequirementCardinality::One,
        }],
        entrypoint: "default".to_owned(),
        execution_class: NATIVE_EXECUTION_CLASS.to_owned(),
        support_channel: SupportChannel::Experimental,
        trust: TrustLevel::Trusted,
        attachment: AttachmentProfile::AppendMany {
            consumer_instance: "tools".to_owned(),
            capability_id: TOOL_PROVIDER_CAPABILITY_ID.to_owned(),
            descriptor_version: TOOL_PROVIDER_DESCRIPTOR_VERSION.to_owned(),
            max_concurrency: 1,
        },
        permission_requests: Vec::new(),
        fixed_host_imports: vec![FixedHostImport {
            capability_id: AGENT_CAPABILITY_ID.to_owned(),
            descriptor_version: AGENT_DESCRIPTOR_VERSION.to_owned(),
            provider_instance: "subagent-agent".to_owned(),
            allowed_provider_packages: BTreeSet::from([AGENT_LOOP_PACKAGE_ID.to_owned()]),
        }],
        inherit_displaced_requirements: false,
    }
}

fn skills_profile() -> ExecutablePluginProfile {
    ExecutablePluginProfile {
        registration_id: "native-skills-filesystem-v1".to_owned(),
        adapter_profile: NATIVE_SKILLS_PROFILE.to_owned(),
        package: PackagePolicy::Exact(SKILLS_PACKAGE_ID.to_owned()),
        authority: ImplementationAuthority::BuiltIn {
            factory_identity: SKILLS_FACTORY_IDENTITY.to_owned(),
        },
        configuration_schema_digest: sha256_digest(SKILLS_CONFIGURATION_SCHEMA),
        configuration: SKILLS_CONFIGURATION.to_owned(),
        provides: vec![
            CapabilityProfile {
                capability_id: PROMPT_PROVIDER_CAPABILITY_ID.to_owned(),
                descriptor_version: PROMPT_PROVIDER_DESCRIPTOR_VERSION.to_owned(),
                descriptor_digest: sha256_digest(PROMPT_PROVIDER_DESCRIPTOR),
                request_operations: vec![PROMPT_PROVIDER_CONTRIBUTE_OPERATION.to_owned()],
                operation_kinds: BTreeMap::new(),
            },
            CapabilityProfile {
                capability_id: TOOL_PROVIDER_CAPABILITY_ID.to_owned(),
                descriptor_version: TOOL_PROVIDER_DESCRIPTOR_VERSION.to_owned(),
                descriptor_digest: sha256_digest(TOOL_PROVIDER_DESCRIPTOR),
                request_operations: vec![
                    TOOL_PROVIDER_CATALOG_OPERATION.to_owned(),
                    TOOL_PROVIDER_EXECUTE_OPERATION.to_owned(),
                ],
                operation_kinds: BTreeMap::new(),
            },
        ],
        requires: Vec::new(),
        entrypoint: "default".to_owned(),
        execution_class: NATIVE_EXECUTION_CLASS.to_owned(),
        support_channel: SupportChannel::Experimental,
        trust: TrustLevel::Trusted,
        attachment: AttachmentProfile::AppendManySet {
            edges: vec![
                AttachmentEdge {
                    consumer_instance: "prompt".to_owned(),
                    capability_id: PROMPT_PROVIDER_CAPABILITY_ID.to_owned(),
                    descriptor_version: PROMPT_PROVIDER_DESCRIPTOR_VERSION.to_owned(),
                },
                AttachmentEdge {
                    consumer_instance: "tools".to_owned(),
                    capability_id: TOOL_PROVIDER_CAPABILITY_ID.to_owned(),
                    descriptor_version: TOOL_PROVIDER_DESCRIPTOR_VERSION.to_owned(),
                },
            ],
        },
        permission_requests: Vec::new(),
        fixed_host_imports: Vec::new(),
        inherit_displaced_requirements: false,
    }
}

fn process_native_profile() -> ExecutablePluginProfile {
    ExecutablePluginProfile {
        registration_id: "native-process-v1".to_owned(),
        adapter_profile: NATIVE_PROCESS_PROFILE.to_owned(),
        package: PackagePolicy::Exact(PROCESS_NATIVE_PACKAGE_ID.to_owned()),
        authority: ImplementationAuthority::BuiltIn {
            factory_identity: PROCESS_NATIVE_FACTORY_IDENTITY.to_owned(),
        },
        configuration_schema_digest: sha256_digest(PROCESS_NATIVE_CONFIGURATION_SCHEMA),
        configuration: PROCESS_NATIVE_CONFIGURATION.to_owned(),
        provides: vec![CapabilityProfile {
            capability_id: PROCESS_CAPABILITY_ID.to_owned(),
            descriptor_version: PROCESS_DESCRIPTOR_VERSION.to_owned(),
            descriptor_digest: sha256_digest(PROCESS_DESCRIPTOR),
            request_operations: vec![
                PROCESS_CATALOG_OPERATION.to_owned(),
                PROCESS_RUN_OPERATION.to_owned(),
            ],
            operation_kinds: BTreeMap::new(),
        }],
        requires: Vec::new(),
        entrypoint: "default".to_owned(),
        execution_class: NATIVE_EXECUTION_CLASS.to_owned(),
        support_channel: SupportChannel::Experimental,
        trust: TrustLevel::Trusted,
        attachment: AttachmentProfile::IntraPluginOnly,
        permission_requests: Vec::new(),
        fixed_host_imports: Vec::new(),
        inherit_displaced_requirements: false,
    }
}

fn process_tools_profile() -> ExecutablePluginProfile {
    ExecutablePluginProfile {
        registration_id: "native-process-tools-v1".to_owned(),
        adapter_profile: TOOL_PROVIDER_PROFILE.to_owned(),
        package: PackagePolicy::Exact(PROCESS_TOOLS_PACKAGE_ID.to_owned()),
        authority: ImplementationAuthority::BuiltIn {
            factory_identity: PROCESS_TOOLS_FACTORY_IDENTITY.to_owned(),
        },
        configuration_schema_digest: sha256_digest(PROCESS_TOOLS_CONFIGURATION_SCHEMA),
        configuration: PROCESS_TOOLS_CONFIGURATION.to_owned(),
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
        requires: vec![CapabilityRequirement {
            capability_id: PROCESS_CAPABILITY_ID.to_owned(),
            descriptor_version: PROCESS_DESCRIPTOR_VERSION.to_owned(),
            cardinality: RequirementCardinality::One,
        }],
        entrypoint: "default".to_owned(),
        execution_class: NATIVE_EXECUTION_CLASS.to_owned(),
        support_channel: SupportChannel::Experimental,
        trust: TrustLevel::Trusted,
        attachment: AttachmentProfile::AppendMany {
            consumer_instance: "tools".to_owned(),
            capability_id: TOOL_PROVIDER_CAPABILITY_ID.to_owned(),
            descriptor_version: TOOL_PROVIDER_DESCRIPTOR_VERSION.to_owned(),
            max_concurrency: 4,
        },
        permission_requests: Vec::new(),
        fixed_host_imports: Vec::new(),
        inherit_displaced_requirements: false,
    }
}

fn openai_model_profile() -> ExecutablePluginProfile {
    ExecutablePluginProfile {
        registration_id: "native-openai-compatible-model-v1".to_owned(),
        adapter_profile: NATIVE_MODEL_PROFILE.to_owned(),
        package: PackagePolicy::Exact(OPENAI_MODEL_PACKAGE_ID.to_owned()),
        authority: ImplementationAuthority::BuiltIn {
            factory_identity: OPENAI_MODEL_FACTORY_IDENTITY.to_owned(),
        },
        configuration_schema_digest: sha256_digest(OPENAI_MODEL_CONFIGURATION_SCHEMA),
        configuration: OPENAI_MODEL_CONFIGURATION.to_owned(),
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
            capability_id: SECRETS_CAPABILITY_ID.to_owned(),
            descriptor_version: SECRETS_DESCRIPTOR_VERSION.to_owned(),
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
            base_configuration_replacements: vec![
                BaseConfigurationReplacement {
                    instance_key: "agent".to_owned(),
                    allowed_package: AGENT_LOOP_PACKAGE_ID.to_owned(),
                    expected_configuration: FIXTURE_AGENT_CONFIGURATION.to_owned(),
                    replacement_configuration: OPENAI_AGENT_CONFIGURATION.to_owned(),
                },
                BaseConfigurationReplacement {
                    instance_key: "subagent-agent".to_owned(),
                    allowed_package: AGENT_LOOP_PACKAGE_ID.to_owned(),
                    expected_configuration: FIXTURE_AGENT_CONFIGURATION.to_owned(),
                    replacement_configuration: OPENAI_AGENT_CONFIGURATION.to_owned(),
                },
            ],
        },
        permission_requests: Vec::new(),
        fixed_host_imports: Vec::new(),
        inherit_displaced_requirements: false,
    }
}

fn secrets_profile() -> ExecutablePluginProfile {
    ExecutablePluginProfile {
        registration_id: "native-env-secrets-v1".to_owned(),
        adapter_profile: NATIVE_SECRETS_PROFILE.to_owned(),
        package: PackagePolicy::Exact(SECRETS_PACKAGE_ID.to_owned()),
        authority: ImplementationAuthority::BuiltIn {
            factory_identity: SECRETS_FACTORY_IDENTITY.to_owned(),
        },
        configuration_schema_digest: SECRETS_CONFIGURATION_SCHEMA_DIGEST.to_owned(),
        configuration: SECRETS_CONFIGURATION.to_owned(),
        provides: vec![CapabilityProfile {
            capability_id: SECRETS_CAPABILITY_ID.to_owned(),
            descriptor_version: SECRETS_DESCRIPTOR_VERSION.to_owned(),
            descriptor_digest: SECRETS_DESCRIPTOR_DIGEST.to_owned(),
            request_operations: vec![SECRETS_RESOLVE_OPERATION.to_owned()],
            operation_kinds: BTreeMap::new(),
        }],
        requires: Vec::new(),
        entrypoint: "default".to_owned(),
        execution_class: NATIVE_EXECUTION_CLASS.to_owned(),
        support_channel: SupportChannel::Experimental,
        trust: TrustLevel::Trusted,
        attachment: AttachmentProfile::IntraPluginOnly,
        permission_requests: Vec::new(),
        fixed_host_imports: Vec::new(),
        inherit_displaced_requirements: false,
    }
}

fn workspace_edit_profile() -> ExecutablePluginProfile {
    ExecutablePluginProfile {
        registration_id: "native-workspace-edit-v1".to_owned(),
        adapter_profile: TOOL_PROVIDER_PROFILE.to_owned(),
        package: PackagePolicy::Exact(WORKSPACE_EDIT_PACKAGE_ID.to_owned()),
        authority: ImplementationAuthority::BuiltIn {
            factory_identity: WORKSPACE_EDIT_FACTORY_IDENTITY.to_owned(),
        },
        configuration_schema_digest: sha256_digest(WORKSPACE_EDIT_CONFIGURATION_SCHEMA),
        configuration: WORKSPACE_EDIT_CONFIGURATION.to_owned(),
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
        support_channel: SupportChannel::Experimental,
        trust: TrustLevel::Trusted,
        attachment: AttachmentProfile::AppendMany {
            consumer_instance: "tools".to_owned(),
            capability_id: TOOL_PROVIDER_CAPABILITY_ID.to_owned(),
            descriptor_version: TOOL_PROVIDER_DESCRIPTOR_VERSION.to_owned(),
            max_concurrency: 4,
        },
        permission_requests: Vec::new(),
        fixed_host_imports: Vec::new(),
        inherit_displaced_requirements: false,
    }
}

fn text_tools_profile() -> ExecutablePluginProfile {
    ExecutablePluginProfile {
        registration_id: "native-text-tools-v1".to_owned(),
        adapter_profile: TOOL_PROVIDER_PROFILE.to_owned(),
        package: PackagePolicy::Exact(TEXT_TOOLS_PACKAGE_ID.to_owned()),
        authority: ImplementationAuthority::BuiltIn {
            factory_identity: TEXT_TOOLS_FACTORY_IDENTITY.to_owned(),
        },
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
            max_concurrency: 4,
        },
        permission_requests: Vec::new(),
        fixed_host_imports: Vec::new(),
        inherit_displaced_requirements: false,
    }
}

fn third_party_wasm_tool_profile() -> ExecutablePluginProfile {
    ExecutablePluginProfile {
        registration_id: "third-party-wasm-tool-provider-v1".to_owned(),
        adapter_profile: TOOL_PROVIDER_PROFILE.to_owned(),
        package: PackagePolicy::AnyNonEmpty,
        authority: ImplementationAuthority::Artifact,
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
        entrypoint: "plugin".to_owned(),
        execution_class: WASM_EXECUTION_CLASS.to_owned(),
        support_channel: SupportChannel::Experimental,
        trust: TrustLevel::Isolated,
        attachment: AttachmentProfile::AppendMany {
            consumer_instance: "tools".to_owned(),
            capability_id: TOOL_PROVIDER_CAPABILITY_ID.to_owned(),
            descriptor_version: TOOL_PROVIDER_DESCRIPTOR_VERSION.to_owned(),
            max_concurrency: 4,
        },
        permission_requests: Vec::new(),
        fixed_host_imports: Vec::new(),
        inherit_displaced_requirements: false,
    }
}

fn third_party_wasm_workspace_read_tool_profile() -> ExecutablePluginProfile {
    let mut profile = third_party_wasm_tool_profile();
    "third-party-wasm-workspace-read-tool-provider-v1".clone_into(&mut profile.registration_id);
    profile.requires = vec![CapabilityRequirement {
        capability_id: WORKSPACE_READ_CAPABILITY_ID.to_owned(),
        descriptor_version: WORKSPACE_READ_DESCRIPTOR_VERSION.to_owned(),
        cardinality: RequirementCardinality::One,
    }];
    profile.fixed_host_imports = vec![FixedHostImport {
        capability_id: WORKSPACE_READ_CAPABILITY_ID.to_owned(),
        descriptor_version: WORKSPACE_READ_DESCRIPTOR_VERSION.to_owned(),
        provider_instance: WORKSPACE_READ_INSTANCE.to_owned(),
        allowed_provider_packages: BTreeSet::from([WORKSPACE_READ_PACKAGE_ID.to_owned()]),
    }];
    profile
}

fn third_party_wasm_http_fetch_tool_profile() -> ExecutablePluginProfile {
    let mut profile = third_party_wasm_tool_profile();
    "third-party-wasm-http-fetch-tool-provider-v1".clone_into(&mut profile.registration_id);
    profile.requires = vec![CapabilityRequirement {
        capability_id: HTTP_FETCH_CAPABILITY_ID.to_owned(),
        descriptor_version: HTTP_FETCH_DESCRIPTOR_VERSION.to_owned(),
        cardinality: RequirementCardinality::One,
    }];
    profile.permission_requests = vec![PermissionProfile {
        request_id: "network".to_owned(),
        resource_kind: "network".to_owned(),
        required: true,
        scope_policy: PermissionScopePolicy::HttpOrigins,
        enforcement_kind: EnforcementKind::Capability,
        enforcer_identity: HTTP_FETCH_PACKAGE_ID.to_owned(),
        provider_instance: HTTP_FETCH_INSTANCE.to_owned(),
    }];
    profile.fixed_host_imports = vec![FixedHostImport {
        capability_id: HTTP_FETCH_CAPABILITY_ID.to_owned(),
        descriptor_version: HTTP_FETCH_DESCRIPTOR_VERSION.to_owned(),
        provider_instance: HTTP_FETCH_INSTANCE.to_owned(),
        allowed_provider_packages: BTreeSet::from([HTTP_FETCH_PACKAGE_ID.to_owned()]),
    }];
    profile
}

fn fixture_model_profile() -> ExecutablePluginProfile {
    ExecutablePluginProfile {
        registration_id: "native-fixture-model-v1".to_owned(),
        adapter_profile: NATIVE_MODEL_PROFILE.to_owned(),
        package: PackagePolicy::Exact(FIXTURE_MODEL_PACKAGE_ID.to_owned()),
        authority: ImplementationAuthority::BuiltIn {
            factory_identity: FIXTURE_MODEL_FACTORY_IDENTITY.to_owned(),
        },
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
        permission_requests: Vec::new(),
        fixed_host_imports: Vec::new(),
        inherit_displaced_requirements: false,
    }
}

fn codex_model_profile() -> ExecutablePluginProfile {
    ExecutablePluginProfile {
        registration_id: "native-codex-direct-model-v1".to_owned(),
        adapter_profile: NATIVE_MODEL_PROFILE.to_owned(),
        package: PackagePolicy::Exact(CODEX_MODEL_PACKAGE_ID.to_owned()),
        authority: ImplementationAuthority::BuiltIn {
            factory_identity: CODEX_MODEL_FACTORY_IDENTITY.to_owned(),
        },
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
            base_configuration_replacements: vec![
                BaseConfigurationReplacement {
                    instance_key: "agent".to_owned(),
                    allowed_package: AGENT_LOOP_PACKAGE_ID.to_owned(),
                    expected_configuration: FIXTURE_AGENT_CONFIGURATION.to_owned(),
                    replacement_configuration: CODEX_AGENT_CONFIGURATION.to_owned(),
                },
                BaseConfigurationReplacement {
                    instance_key: "subagent-agent".to_owned(),
                    allowed_package: AGENT_LOOP_PACKAGE_ID.to_owned(),
                    expected_configuration: FIXTURE_AGENT_CONFIGURATION.to_owned(),
                    replacement_configuration: CODEX_AGENT_CONFIGURATION.to_owned(),
                },
            ],
        },
        permission_requests: Vec::new(),
        fixed_host_imports: Vec::new(),
        inherit_displaced_requirements: false,
    }
}

fn codex_auth_profile() -> ExecutablePluginProfile {
    ExecutablePluginProfile {
        registration_id: "native-codex-auth-v1".to_owned(),
        adapter_profile: NATIVE_AUTH_PROFILE.to_owned(),
        package: PackagePolicy::Exact(CODEX_AUTH_PACKAGE_ID.to_owned()),
        authority: ImplementationAuthority::BuiltIn {
            factory_identity: CODEX_AUTH_FACTORY_IDENTITY.to_owned(),
        },
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
        permission_requests: Vec::new(),
        fixed_host_imports: Vec::new(),
        inherit_displaced_requirements: false,
    }
}

fn guest_agent_profile(
    registration_id: &str,
    execution_class: &str,
    entrypoint: &str,
    trust: TrustLevel,
) -> ExecutablePluginProfile {
    ExecutablePluginProfile {
        registration_id: registration_id.to_owned(),
        adapter_profile: AGENT_PROVIDER_PROFILE.to_owned(),
        package: PackagePolicy::Exact(GUEST_AGENT_PACKAGE_ID.to_owned()),
        authority: ImplementationAuthority::Artifact,
        configuration_schema_digest: sha256_digest(EMPTY_CONFIGURATION_SCHEMA),
        configuration: "{}".to_owned(),
        provides: vec![CapabilityProfile {
            capability_id: AGENT_CAPABILITY_ID.to_owned(),
            descriptor_version: AGENT_DESCRIPTOR_VERSION.to_owned(),
            descriptor_digest: sha256_digest(AGENT_DESCRIPTOR),
            request_operations: vec![RUN_TURN_OPERATION.to_owned()],
            operation_kinds: BTreeMap::from([(
                RUN_TURN_OPERATION.to_owned(),
                CapabilityOperationKind::Stream,
            )]),
        }],
        requires: vec![
            CapabilityRequirement {
                capability_id: MODEL_CAPABILITY_ID.to_owned(),
                descriptor_version: MODEL_DESCRIPTOR_VERSION.to_owned(),
                cardinality: RequirementCardinality::One,
            },
            CapabilityRequirement {
                capability_id: PROMPT_CAPABILITY_ID.to_owned(),
                descriptor_version: PROMPT_DESCRIPTOR_VERSION.to_owned(),
                cardinality: RequirementCardinality::One,
            },
            CapabilityRequirement {
                capability_id: TOOLS_CAPABILITY_ID.to_owned(),
                descriptor_version: TOOLS_DESCRIPTOR_VERSION.to_owned(),
                cardinality: RequirementCardinality::One,
            },
            CapabilityRequirement {
                capability_id: SESSION_CAPABILITY_ID.to_owned(),
                descriptor_version: SESSION_DESCRIPTOR_VERSION.to_owned(),
                cardinality: RequirementCardinality::One,
            },
        ],
        entrypoint: entrypoint.to_owned(),
        execution_class: execution_class.to_owned(),
        support_channel: SupportChannel::Experimental,
        trust,
        attachment: AttachmentProfile::ReplaceOne {
            consumer_instance: "cli".to_owned(),
            capability_id: AGENT_CAPABILITY_ID.to_owned(),
            descriptor_version: AGENT_DESCRIPTOR_VERSION.to_owned(),
            displaced_provider_instance: "agent".to_owned(),
            allowed_displaced_packages: BTreeSet::from([AGENT_LOOP_PACKAGE_ID.to_owned()]),
            base_configuration_replacements: Vec::new(),
        },
        permission_requests: Vec::new(),
        fixed_host_imports: Vec::new(),
        inherit_displaced_requirements: true,
    }
}

impl AttachmentProfile {
    fn capability_edge(&self) -> Option<(&str, &str, &str)> {
        match self {
            Self::AppendMany {
                consumer_instance,
                capability_id,
                descriptor_version,
                ..
            }
            | Self::ReplaceOne {
                consumer_instance,
                capability_id,
                descriptor_version,
                ..
            } => Some((consumer_instance, capability_id, descriptor_version)),
            Self::AppendManySet { .. } | Self::IntraPluginOnly => None,
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

fn require_consumer<'a>(
    base_plan: &'a ResolvedAppPlan,
    consumer_instance: &str,
    registration_id: &str,
) -> Result<&'a lenso_app_plan::ModuleInstancePlan, String> {
    base_plan.module_instance(consumer_instance).ok_or_else(|| {
        format!(
            "Plugin profile `{registration_id}` requires consumer Instance `{consumer_instance}`"
        )
    })
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
    let incompatible_consumers = base_plan.capability_bindings().iter().any(|binding| {
        binding.provider_instance() == displaced_provider_instance
            && (binding.capability_id() != capability_id
                || binding.descriptor_version() != descriptor_version)
    });
    if incompatible_consumers {
        return Err(format!(
            "Plugin profile `{profile_id}` cannot replace displaced Instance `{displaced_provider_instance}` because it provides another Capability"
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
    use lenso_plugin_control_plane::{
        CapabilityDeclaration, ImplementationVariant, PermissionRequest,
    };

    #[test]
    fn catalog_registers_multiple_distinct_profiles_deterministically() {
        let mut second = text_tools_profile();
        second.registration_id = "native-more-text-tools-v1".to_owned();
        second.package = PackagePolicy::Exact("example.more-text-tools".to_owned());
        second.authority = ImplementationAuthority::BuiltIn {
            factory_identity: "example.more-text-tools@1.0.0".to_owned(),
        };
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
                TOOL_PROVIDER_PROFILE
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
    fn capability_requirement_order_does_not_affect_profile_matching() {
        let profile = guest_agent_profile(
            "quickjs-agent-provider-v1",
            QUICKJS_EXECUTION_CLASS,
            "plugin.mjs",
            TrustLevel::Constrained,
        );
        let mut contribution = contribution_for(&profile, "test-target");
        contribution.requires.reverse();

        PluginProfileCatalog::default()
            .register(profile)
            .unwrap()
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

    #[test]
    fn isolated_wasm_tool_profile_accepts_an_external_package_without_host_registration() {
        let profile = third_party_wasm_tool_profile();
        let mut contribution = contribution_for(&profile, "test-target");
        contribution.package_id = "dev.example.reverse-tools".to_owned();

        PluginProfileCatalog::default()
            .register(profile)
            .unwrap()
            .validate_contribution(&contribution, "test-target")
            .unwrap();
    }

    #[test]
    fn isolated_wasm_tool_profile_rejects_authority_expansion() {
        let profile = third_party_wasm_tool_profile();
        let mut contribution = contribution_for(&profile, "test-target");
        contribution.package_id = "dev.example.unsafe-tools".to_owned();
        contribution.requires.push(CapabilityRequirement {
            capability_id: "example.host@1".to_owned(),
            descriptor_version: "1.0.0".to_owned(),
            cardinality: RequirementCardinality::One,
        });

        let error = PluginProfileCatalog::default()
            .register(profile)
            .unwrap()
            .validate_contribution(&contribution, "test-target")
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("does not match a registered Plugin profile")
        );
    }

    #[test]
    fn workspace_reader_wasm_profile_accepts_only_its_exact_host_import() {
        let profile = third_party_wasm_workspace_read_tool_profile();
        let mut contribution = contribution_for(&profile, "test-target");
        contribution.package_id = "dev.example.workspace-reader".to_owned();
        let catalog = PluginProfileCatalog::default().register(profile).unwrap();
        catalog
            .validate_contribution(&contribution, "test-target")
            .unwrap();

        contribution.requires.push(CapabilityRequirement {
            capability_id: "lenso.agent.process@1".to_owned(),
            descriptor_version: "1.0.0".to_owned(),
            cardinality: RequirementCardinality::One,
        });
        assert!(
            catalog
                .validate_contribution(&contribution, "test-target")
                .unwrap_err()
                .to_string()
                .contains("does not match a registered Plugin profile")
        );
    }

    #[test]
    fn network_wasm_profile_closes_one_capability_enforced_grant() {
        let profile = third_party_wasm_http_fetch_tool_profile();
        let contribution = contribution_for(&profile, "test-target");
        let mut manifest = PluginManifest {
            schema_version: 1,
            plugin_id: "dev.example.network-tool".to_owned(),
            release_version: "1.0.0".to_owned(),
            artifacts: Vec::new(),
            module_contributions: vec![contribution],
            data_contributions: Vec::new(),
            permission_requests: vec![PermissionRequest {
                id: "network".to_owned(),
                resource_kind: "network".to_owned(),
                required: true,
                scope: serde_json::json!({"origins": ["https://api.example.com"]}),
                explanation_key: "network.api-example".to_owned(),
            }],
            features: Vec::new(),
            binding_templates: Vec::new(),
            product_metadata: Vec::new(),
        };
        let catalog = PluginProfileCatalog::default().register(profile).unwrap();
        catalog
            .validate_manifest_topology(&manifest, "test-target")
            .unwrap();
        let grants = catalog
            .approved_grants_for(&manifest, &["more-text-tools".to_owned()], "test-target")
            .unwrap();
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].enforcement_kind, EnforcementKind::Capability);
        assert_eq!(
            grants[0].scope,
            serde_json::json!({"origins": ["https://api.example.com"]})
        );

        manifest.permission_requests[0].scope =
            serde_json::json!({"origins": ["https://api.example.com/path"]});
        assert!(
            catalog
                .validate_manifest_topology(&manifest, "test-target")
                .unwrap_err()
                .to_string()
                .contains("not normalized HTTP authority")
        );
    }

    fn contribution_for(profile: &ExecutablePluginProfile, target: &str) -> ModuleContribution {
        ModuleContribution {
            id: "more-text-tools".to_owned(),
            package_id: match &profile.package {
                PackagePolicy::Exact(package_id) => package_id.clone(),
                PackagePolicy::AnyNonEmpty => "example.third-party".to_owned(),
            },
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
                artifact: matches!(profile.authority, ImplementationAuthority::Artifact)
                    .then(|| "artifact".to_owned()),
                built_in_factory: profile.built_in_factory().map(str::to_owned),
                entrypoint: profile.entrypoint.clone(),
                execution_class: profile.execution_class.clone(),
                targets: vec![target.to_owned()],
                profiles: vec![profile.adapter_profile.clone()],
                support_channel: profile.support_channel,
                trust: profile.trust,
            }],
            permission_request_ids: profile
                .permission_requests
                .iter()
                .map(|permission| permission.request_id.clone())
                .collect(),
            state: None,
        }
    }
}
