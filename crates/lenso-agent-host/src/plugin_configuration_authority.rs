use std::{rc::Rc, str::FromStr, sync::Arc};

use lenso_app_authoring::{
    PluginConfigurationApplication, PluginConfigurationAuthority, PluginConfigurationDiagnostic,
    PluginConfigurationProposal, PluginConfigurationProposalStatus, PluginRootAuthoringState,
    PluginRootRevision, PluginSelectionAuthority,
};
use lenso_app_plan::{CapabilityEndpointPlan, authoring::PluginDescriptor};
use lenso_capability_agent_plugin_configuration_authority as contract;
use lenso_capability_agent_plugin_selection_authority as selection_contract;
use lenso_kernel::{InvocationContext, RuntimeFailure};
use lenso_native_adapter::{NativePluginFactory, NativePluginFactoryContext, NativePluginInstance};

pub(crate) const BRIDGE_PLUGIN_ID: &str = "lenso.agent.plugin-configuration-authority-bridge";
pub(crate) const BRIDGE_PLUGIN_VERSION: &str = "0.1.0";

#[derive(Clone, Debug)]
pub(crate) struct PluginConfigurationAuthorityBridgeFactory {
    authority: Arc<dyn PluginConfigurationAuthority>,
    selection_authority: Option<Arc<dyn PluginSelectionAuthority>>,
}

impl PluginConfigurationAuthorityBridgeFactory {
    pub(crate) fn new(
        authority: Arc<dyn PluginConfigurationAuthority>,
        selection_authority: Option<Arc<dyn PluginSelectionAuthority>>,
    ) -> Self {
        Self {
            authority,
            selection_authority,
        }
    }
}

impl NativePluginFactory for PluginConfigurationAuthorityBridgeFactory {
    fn package_id(&self) -> &'static str {
        BRIDGE_PLUGIN_ID
    }

    fn package_version(&self) -> &'static str {
        BRIDGE_PLUGIN_VERSION
    }

    fn instantiate(
        &self,
        _context: NativePluginFactoryContext<'_>,
    ) -> Result<NativePluginInstance, RuntimeFailure> {
        let endpoint = contract::PluginConfigurationAuthorityEndpoint::new(AuthorityProvider {
            authority: Arc::clone(&self.authority),
        });
        let selection_endpoint =
            selection_contract::PluginSelectionAuthorityEndpoint::new(SelectionAuthorityProvider {
                authority: self.selection_authority.clone(),
                inspection: Arc::clone(&self.authority),
            });
        Ok(NativePluginInstance::new(vec![
            Rc::new(endpoint),
            Rc::new(selection_endpoint),
        ]))
    }
}

#[derive(Clone, Debug)]
struct SelectionAuthorityProvider {
    authority: Option<Arc<dyn PluginSelectionAuthority>>,
    inspection: Arc<dyn PluginConfigurationAuthority>,
}

impl selection_contract::PluginSelectionAuthorityProvider for SelectionAuthorityProvider {
    fn set_enabled(
        &self,
        _context: InvocationContext,
        request: selection_contract::SetEnabledRequest,
    ) -> lenso_kernel::NativeRequestFuture<selection_contract::PluginSelectionAuthority> {
        let result = set_enabled(
            self.inspection.as_ref(),
            self.authority.as_deref(),
            &request,
        );
        Box::pin(async move { result })
    }
}

#[derive(Clone, Debug)]
struct AuthorityProvider {
    authority: Arc<dyn PluginConfigurationAuthority>,
}

impl contract::PluginConfigurationAuthorityProvider for AuthorityProvider {
    fn inspect(
        &self,
        _context: InvocationContext,
        _request: contract::InspectRequest,
    ) -> lenso_kernel::NativeRequestFuture<contract::PluginConfigurationAuthorityInspect> {
        let result = inspect_authority(self.authority.as_ref());
        Box::pin(async move { result })
    }

    fn propose(
        &self,
        _context: InvocationContext,
        request: contract::ProposeRequest,
    ) -> lenso_kernel::NativeRequestFuture<contract::PluginConfigurationAuthorityPropose> {
        let result = propose_configuration(self.authority.as_ref(), &request);
        Box::pin(async move { result })
    }

    fn publish(
        &self,
        _context: InvocationContext,
        request: contract::PublishRequest,
    ) -> lenso_kernel::NativeRequestFuture<contract::PluginConfigurationAuthorityPublish> {
        let result = publish_configuration(self.authority.as_ref(), &request);
        Box::pin(async move { result })
    }
}

pub(crate) fn bridge_descriptor() -> PluginDescriptor {
    PluginDescriptor::new(
        BRIDGE_PLUGIN_ID,
        BRIDGE_PLUGIN_VERSION,
        "plugin-configuration-authority",
    )
    .with_runtime_package(BRIDGE_PLUGIN_ID, BRIDGE_PLUGIN_VERSION)
    .with_capability(
        CapabilityEndpointPlan::new(
            contract::CAPABILITY_ID,
            contract::DESCRIPTOR_VERSION,
            [
                contract::INSPECT_OPERATION,
                contract::PROPOSE_OPERATION,
                contract::PUBLISH_OPERATION,
            ],
        )
        .with_limits(8, 1),
    )
    .with_capability(
        CapabilityEndpointPlan::new(
            selection_contract::CAPABILITY_ID,
            selection_contract::DESCRIPTOR_VERSION,
            [selection_contract::SET_ENABLED_OPERATION],
        )
        .with_limits(4, 1),
    )
}

fn set_enabled(
    inspection: &dyn PluginConfigurationAuthority,
    authority: Option<&dyn PluginSelectionAuthority>,
    request: &selection_contract::SetEnabledRequest,
) -> Result<
    Result<selection_contract::SetEnabledResponse, selection_contract::SetEnabledError>,
    RuntimeFailure,
> {
    let Some(authority) = authority else {
        return Ok(Err(selection_contract::SetEnabledError::Unsupported));
    };
    let Ok(expected) = PluginRootRevision::from_str(&request.expected_revision) else {
        return Ok(Err(selection_contract::SetEnabledError::InvalidRequest));
    };
    let state = inspection.inspect().map_err(selection_authority_failure)?;
    if state.revision() != &expected {
        return Ok(Err(selection_contract::SetEnabledError::Conflict));
    }
    let Some(instance) = state
        .plugins()
        .iter()
        .find(|plugin| plugin.plugin_id() == request.plugin_id)
        .and_then(|plugin| {
            plugin
                .instances()
                .iter()
                .find(|instance| instance.id().instance_key() == request.instance)
        })
    else {
        return Ok(Err(selection_contract::SetEnabledError::NotFound));
    };
    if !request.enabled && !instance.is_disableable() {
        return Ok(Err(selection_contract::SetEnabledError::NotDisableable));
    }
    if request.enabled == instance.is_enabled() {
        return Ok(Err(selection_contract::SetEnabledError::AlreadySelected));
    }
    let publication = match authority.set_enabled(
        &expected,
        &request.plugin_id,
        &request.instance,
        request.enabled,
    ) {
        Ok(publication) => publication,
        Err(error) => {
            if inspection
                .inspect()
                .is_ok_and(|current| current.revision() != &expected)
            {
                return Ok(Err(selection_contract::SetEnabledError::Conflict));
            }
            return Err(selection_authority_failure(error));
        }
    };
    let source = authority.source();
    Ok(Ok(selection_contract::SetEnabledResponse {
        authority: selection_contract::AuthoritySource {
            kind: source.kind().to_owned(),
            reference: source.reference().to_owned(),
        },
        base_revision: publication.base_revision().as_str().to_owned(),
        enabled: publication.enabled(),
        instance: publication.instance().to_owned(),
        plugin_id: publication.plugin_id().to_owned(),
        revision: publication.revision().as_str().to_owned(),
        schema: "lenso.plugin-selection-publication.v1".to_owned(),
    }))
}

fn inspect_authority(
    authority: &dyn PluginConfigurationAuthority,
) -> Result<Result<contract::InspectResponse, contract::InspectError>, RuntimeFailure> {
    authority
        .inspect()
        .map(|state| Ok(inspect_response(authority, &state)))
        .map_err(authority_failure)
}

fn inspect_response(
    authority: &dyn PluginConfigurationAuthority,
    state: &PluginRootAuthoringState,
) -> contract::InspectResponse {
    contract::InspectResponse {
        authority: authority_source(authority),
        binding_count: bounded_count(state.resolved().plan().capability_bindings().len(), 65_536),
        enabled_instance_count: bounded_count(state.resolved().instances().len(), 65_536),
        plugins: state
            .plugins()
            .iter()
            .map(|plugin| contract::PluginInspection {
                instances: plugin
                    .instances()
                    .iter()
                    .map(|instance| {
                        let configuration = instance.root_configuration_toml();
                        contract::PluginInstanceInspection {
                            disableable: instance.is_disableable(),
                            has_root_difference: instance.has_root_difference(),
                            instance_key: instance.id().instance_key().to_owned(),
                            origin: if instance.is_host_default() {
                                "host-default"
                            } else {
                                "plugin-root"
                            }
                            .to_owned(),
                            root_configuration_bytes: configuration
                                .map_or(0, |value| bounded_count(value.len(), 262_144)),
                            root_configuration_present: configuration.is_some(),
                            selection: if instance.is_enabled() {
                                "enabled"
                            } else {
                                "disabled-by-root"
                            }
                            .to_owned(),
                            source_digest: instance.source_digest().as_str().to_owned(),
                        }
                    })
                    .collect(),
                package_id: plugin.plugin_id().to_owned(),
                package_revision: plugin.release_version().to_owned(),
                source: if plugin.is_root_supplied() {
                    "plugin-root"
                } else {
                    "host-build"
                }
                .to_owned(),
            })
            .collect(),
        revision: state.revision().as_str().to_owned(),
    }
}

fn propose_configuration(
    authority: &dyn PluginConfigurationAuthority,
    request: &contract::ProposeRequest,
) -> Result<Result<contract::ProposeResponse, contract::ProposeError>, RuntimeFailure> {
    let Ok(expected) = PluginRootRevision::from_str(&request.expected_revision) else {
        return Ok(Err(contract::ProposeError::InvalidRequest));
    };
    let state = authority.inspect().map_err(authority_failure)?;
    if state.revision() != &expected {
        return Ok(Err(contract::ProposeError::Conflict));
    }
    if !has_instance(&state, &request.plugin_id, &request.instance) {
        return Ok(Err(contract::ProposeError::NotFound));
    }
    let proposal = match authority.propose(
        &expected,
        &request.plugin_id,
        &request.instance,
        request.configuration_toml.as_bytes(),
    ) {
        Ok(proposal) => proposal,
        Err(error) => {
            if authority
                .inspect()
                .is_ok_and(|current| current.revision() != &expected)
            {
                return Ok(Err(contract::ProposeError::Conflict));
            }
            return Err(authority_failure(error));
        }
    };
    Ok(Ok(proposal_response(authority, &proposal)))
}

fn publish_configuration(
    authority: &dyn PluginConfigurationAuthority,
    request: &contract::PublishRequest,
) -> Result<Result<contract::PublishResponse, contract::PublishError>, RuntimeFailure> {
    let Ok(expected) = PluginRootRevision::from_str(&request.expected_revision) else {
        return Ok(Err(contract::PublishError::InvalidRequest));
    };
    let state = authority.inspect().map_err(authority_failure)?;
    if state.revision() != &expected {
        return Ok(Err(contract::PublishError::Conflict));
    }
    if !has_instance(&state, &request.plugin_id, &request.instance) {
        return Ok(Err(contract::PublishError::NotFound));
    }
    let proposal = authority
        .propose(
            &expected,
            &request.plugin_id,
            &request.instance,
            request.configuration_toml.as_bytes(),
        )
        .map_err(authority_failure)?;
    if proposal.digest() != request.proposal_digest {
        return Ok(Err(contract::PublishError::ProposalMismatch));
    }
    if proposal.status() != PluginConfigurationProposalStatus::Ready
        || proposal.application() == PluginConfigurationApplication::Blocked
    {
        return Ok(Err(contract::PublishError::ProposalNotReady));
    }
    let publication = match authority.publish(&proposal) {
        Ok(publication) => publication,
        Err(error) => {
            if authority
                .inspect()
                .is_ok_and(|current| current.revision() != &expected)
            {
                return Ok(Err(contract::PublishError::Conflict));
            }
            return Err(authority_failure(error));
        }
    };
    Ok(Ok(contract::PublishResponse {
        authority: authority_source(authority),
        base_revision: publication.base_revision().as_str().to_owned(),
        base_source_digest: publication.base_source_digest().as_str().to_owned(),
        proposal_digest: publication.proposal_digest().to_owned(),
        revision: publication.revision().as_str().to_owned(),
        schema: publication.schema().to_owned(),
    }))
}

fn proposal_response(
    authority: &dyn PluginConfigurationAuthority,
    proposal: &PluginConfigurationProposal,
) -> contract::ProposeResponse {
    contract::ProposeResponse {
        application: match proposal.application() {
            PluginConfigurationApplication::Noop => "noop",
            PluginConfigurationApplication::AppGeneration => "app_generation",
            PluginConfigurationApplication::Blocked => "blocked",
        }
        .to_owned(),
        authority: authority_source(authority),
        base_revision: proposal.base_revision().as_str().to_owned(),
        base_source_digest: proposal.base_source_digest().as_str().to_owned(),
        candidate_revision: proposal.candidate_revision().as_str().to_owned(),
        diagnostics: proposal
            .diagnostics()
            .iter()
            .map(diagnostic_response)
            .collect(),
        instance: proposal.instance_key().to_owned(),
        plugin_id: proposal.plugin_id().to_owned(),
        proposal_digest: proposal.digest().to_owned(),
        schema: proposal.schema().to_owned(),
        status: match proposal.status() {
            PluginConfigurationProposalStatus::Ready => "ready",
            PluginConfigurationProposalStatus::NeedsDecision => "needs_decision",
            PluginConfigurationProposalStatus::Rejected => "rejected",
        }
        .to_owned(),
    }
}

fn authority_source(authority: &dyn PluginConfigurationAuthority) -> contract::AuthoritySource {
    let source = authority.source();
    contract::AuthoritySource {
        kind: source.kind().to_owned(),
        reference: source.reference().to_owned(),
    }
}

fn diagnostic_response(diagnostic: &PluginConfigurationDiagnostic) -> contract::ProposalDiagnostic {
    contract::ProposalDiagnostic {
        code: diagnostic.code().to_owned(),
        detail: truncate_utf8(diagnostic.detail(), 4_096),
    }
}

fn has_instance(state: &PluginRootAuthoringState, plugin_id: &str, instance: &str) -> bool {
    state.plugins().iter().any(|plugin| {
        plugin.plugin_id() == plugin_id
            && plugin
                .instances()
                .iter()
                .any(|candidate| candidate.id().instance_key() == instance)
    })
}

fn bounded_count(value: usize, maximum: usize) -> i64 {
    i64::try_from(value.min(maximum)).expect("bounded counts fit into i64")
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_owned();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_owned()
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "map_err transfers the owned authority error into the Runtime failure"
)]
fn authority_failure(error: anyhow::Error) -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: format!("Plugin configuration authority failed: {error}"),
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "map_err transfers the owned authority error into the Runtime failure"
)]
fn selection_authority_failure(error: anyhow::Error) -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: format!("Plugin selection authority failed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use lenso_app_authoring::{
        LocalPluginRootAuthority, PluginConfigurationAuthoritySource,
        PluginConfigurationPublication,
    };
    use lenso_app_plan::authoring::{HostCatalog, HostDefaultPlugin, HostPluginRelease, HostSlot};

    use super::*;

    #[derive(Debug)]
    struct RecordingAuthority {
        inspections: AtomicUsize,
        local: LocalPluginRootAuthority,
        proposals: AtomicUsize,
        publications: AtomicUsize,
        source: PluginConfigurationAuthoritySource,
    }

    impl PluginConfigurationAuthority for RecordingAuthority {
        fn source(&self) -> PluginConfigurationAuthoritySource {
            self.source.clone()
        }

        fn inspect(&self) -> anyhow::Result<PluginRootAuthoringState> {
            self.inspections.fetch_add(1, Ordering::Relaxed);
            self.local.inspect()
        }

        fn propose(
            &self,
            expected_revision: &PluginRootRevision,
            plugin_id: &str,
            instance: &str,
            bytes: &[u8],
        ) -> anyhow::Result<PluginConfigurationProposal> {
            self.proposals.fetch_add(1, Ordering::Relaxed);
            self.local
                .propose(expected_revision, plugin_id, instance, bytes)
        }

        fn publish(
            &self,
            proposal: &PluginConfigurationProposal,
        ) -> anyhow::Result<PluginConfigurationPublication> {
            self.publications.fetch_add(1, Ordering::Relaxed);
            self.local.publish(proposal)
        }
    }

    fn fixture() -> (tempfile::TempDir, LocalPluginRootAuthority) {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".lenso")).unwrap();
        let catalog = HostCatalog::new(
            [
                HostSlot::many("tool-providers"),
                HostSlot::one("plugin-configuration-authority"),
                HostSlot::optional("optional"),
            ],
            [
                HostPluginRelease::new(bridge_descriptor()),
                HostPluginRelease::new(PluginDescriptor::new(
                    "example.optional",
                    "1.0.0",
                    "optional",
                )),
            ],
            [
                HostDefaultPlugin::new(BRIDGE_PLUGIN_ID, "default"),
                HostDefaultPlugin::new("example.optional", "default").disableable(),
            ],
        );
        fs::write(
            root.path().join(".lenso/host-catalog.json"),
            serde_json::to_vec(&catalog).unwrap(),
        )
        .unwrap();
        let authority = LocalPluginRootAuthority::new(root.path());
        (root, authority)
    }

    #[test]
    fn bridge_uses_the_selected_authority_for_proposal_and_publication() {
        let (root, authority) = fixture();
        let before = authority.inspect().unwrap();
        let configuration = "# reviewed bridge configuration\n".to_owned();
        let proposed = propose_configuration(
            &authority,
            &contract::ProposeRequest {
                configuration_toml: configuration.clone(),
                expected_revision: before.revision().as_str().to_owned(),
                instance: "default".to_owned(),
                plugin_id: BRIDGE_PLUGIN_ID.to_owned(),
            },
        )
        .unwrap()
        .unwrap();
        let published = publish_configuration(
            &authority,
            &contract::PublishRequest {
                configuration_toml: configuration,
                expected_revision: before.revision().as_str().to_owned(),
                instance: "default".to_owned(),
                plugin_id: BRIDGE_PLUGIN_ID.to_owned(),
                proposal_digest: proposed.proposal_digest,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(published.authority.kind, "local_plugin_root");
        assert!(
            root.path()
                .join(format!("plugins/{BRIDGE_PLUGIN_ID}/default.toml"))
                .is_file()
        );
    }

    #[test]
    fn bridge_rejects_a_stale_revision_before_publication() {
        let (_root, authority) = fixture();
        let result = propose_configuration(
            &authority,
            &contract::ProposeRequest {
                configuration_toml: "# stale bridge configuration\n".to_owned(),
                expected_revision: format!("sha256:{}", "0".repeat(64)),
                instance: "default".to_owned(),
                plugin_id: BRIDGE_PLUGIN_ID.to_owned(),
            },
        )
        .unwrap();
        assert_eq!(result, Err(contract::ProposeError::Conflict));
    }

    #[test]
    fn bridge_dispatches_a_direct_selection_change() {
        let (root, authority) = fixture();
        let before = authority.inspect().unwrap();

        let response = set_enabled(
            &authority,
            Some(&authority),
            &selection_contract::SetEnabledRequest {
                enabled: false,
                expected_revision: before.revision().as_str().to_owned(),
                instance: "default".to_owned(),
                plugin_id: "example.optional".to_owned(),
            },
        )
        .unwrap()
        .unwrap();

        assert!(!response.enabled);
        assert_eq!(response.plugin_id, "example.optional");
        assert!(
            root.path()
                .join("plugins/example.optional/default.disabled")
                .is_file()
        );
    }

    #[test]
    fn bridge_preserves_an_injected_custom_authority_identity_and_lifecycle() {
        let (_root, local) = fixture();
        let authority = RecordingAuthority {
            inspections: AtomicUsize::new(0),
            local,
            proposals: AtomicUsize::new(0),
            publications: AtomicUsize::new(0),
            source: PluginConfigurationAuthoritySource::new("remote_fixture", "tenant/app")
                .unwrap(),
        };
        let before = inspect_authority(&authority).unwrap().unwrap();
        assert_eq!(before.authority.kind, "remote_fixture");
        assert_eq!(before.authority.reference, "tenant/app");
        let configuration = "# custom authority publication\n".to_owned();
        let proposal = propose_configuration(
            &authority,
            &contract::ProposeRequest {
                configuration_toml: configuration.clone(),
                expected_revision: before.revision.clone(),
                instance: "default".to_owned(),
                plugin_id: BRIDGE_PLUGIN_ID.to_owned(),
            },
        )
        .unwrap()
        .unwrap();
        let publication = publish_configuration(
            &authority,
            &contract::PublishRequest {
                configuration_toml: configuration,
                expected_revision: before.revision,
                instance: "default".to_owned(),
                plugin_id: BRIDGE_PLUGIN_ID.to_owned(),
                proposal_digest: proposal.proposal_digest,
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(publication.authority.kind, "remote_fixture");
        assert!(authority.inspections.load(Ordering::Relaxed) >= 3);
        assert_eq!(authority.proposals.load(Ordering::Relaxed), 2);
        assert_eq!(authority.publications.load(Ordering::Relaxed), 1);
    }
}
