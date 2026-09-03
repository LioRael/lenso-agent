use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    future::Future,
    io::Write,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path as AxumPath, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post, put},
};
use lenso_agent_host::PluginManagementTarget;
use lenso_app_authoring::{
    PluginConfigurationApplication, PluginConfigurationAuthority,
    PluginConfigurationAuthoritySource, PluginConfigurationDiagnostic, PluginConfigurationProposal,
    PluginConfigurationProposalStatus, PluginConfigurationSourceDigest, PluginRootAuthoringState,
    PluginRootRevision, PluginSelectionAuthority, add_bundle,
};
use lenso_capability_agent_plugin_management_target as target_contract;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::sync::oneshot;

use super::{
    ApiProblem, PluginConfigurationHistoryAuthority, PluginConfigurationPublicationRecord,
    RuntimeCommand, TrustedPluginBundle, WebRuntime,
};
use crate::plugin_control_api::{
    DesiredPluginSelection, PluginConfigurationAuthorityResponse, PluginInventoryResponse,
    PluginMutationResponse, PluginObservationFence, PluginOperation, PluginOperationResponse,
    PluginRuntimeCommand,
};

const MAX_STAGING_ENTRIES: usize = 16_384;
const MAX_PLUGIN_CONFIGURATION_BYTES: usize = 256 * 1024;
const MAX_PLUGIN_CONFIGURATION_REQUEST_BYTES: usize =
    MAX_PLUGIN_CONFIGURATION_BYTES * 6 + 16 * 1024;

#[derive(Clone, Debug)]
pub(super) struct PluginControl {
    app_root: PathBuf,
    authority_home: PathBuf,
    configuration_authority: Arc<dyn PluginConfigurationAuthority>,
    selection_authority: Option<Arc<dyn PluginSelectionAuthority>>,
    configuration_history: Option<Arc<dyn PluginConfigurationHistoryAuthority>>,
    configuration_authority_is_builtin_local: bool,
    mutation: Arc<Mutex<()>>,
    profile: Option<String>,
    trusted_bundles: BTreeMap<String, PathBuf>,
}

pub(super) struct PluginControlAuthorities {
    pub(super) configuration: Arc<dyn PluginConfigurationAuthority>,
    pub(super) configuration_is_builtin_local: bool,
    pub(super) history: Option<Arc<dyn PluginConfigurationHistoryAuthority>>,
    pub(super) selection: Option<Arc<dyn PluginSelectionAuthority>>,
}

#[derive(Clone, Debug)]
pub(super) struct RoutedPluginManagementTarget {
    external: Option<Arc<dyn PluginManagementTarget>>,
    local: Option<PluginControl>,
}

impl RoutedPluginManagementTarget {
    pub(super) fn new(
        local: Option<PluginControl>,
        external: Option<Arc<dyn PluginManagementTarget>>,
    ) -> Self {
        Self { external, local }
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct PluginMutationCoordinator {
    gate: Arc<tokio::sync::Mutex<()>>,
}

#[derive(Clone, Copy)]
struct ReviewedPluginConfiguration<'a> {
    bytes: &'a [u8],
    expected_proposal_digest: &'a str,
    expected_revision: &'a PluginRootRevision,
    expected_source_digest: &'a str,
    rollback_of_proposal_digest: Option<&'a str>,
}

impl PluginMutationCoordinator {
    pub(super) async fn run<T>(&self, transaction: impl Future<Output = T>) -> T {
        let _guard = self.gate.lock().await;
        transaction.await
    }
}

impl PluginControl {
    fn lifecycle_authority() -> PluginConfigurationAuthorityResponse {
        PluginConfigurationAuthorityResponse {
            kind: "trusted_local_bundle_catalog".to_owned(),
            reference: "agent-host".to_owned(),
            publication_history: false,
            rollback_proposals: false,
        }
    }

    fn trusted_catalog(&self, query: &str) -> Result<TrustedPluginCatalogResponse, String> {
        let state = self
            .configuration_authority
            .inspect()
            .map_err(|error| error.to_string())?;
        let folded = query.to_lowercase();
        let mut entries = Vec::new();
        for (catalog_entry_id, bundle) in &self.trusted_bundles {
            let staged = StagedHome::new(&self.app_root)?;
            let verification_home = staged.root.join("catalog-verification");
            copy_file(
                &staged.home.join(".lenso/host-catalog.json"),
                &verification_home.join(".lenso/host-catalog.json"),
            )?;
            let (package_id, package_revision, _) =
                add_bundle(&verification_home, bundle).map_err(|error| error.to_string())?;
            if folded.is_empty()
                || catalog_entry_id.to_lowercase().contains(&folded)
                || package_id.to_lowercase().contains(&folded)
            {
                entries.push(TrustedPluginCatalogEntry {
                    catalog_entry_id: catalog_entry_id.clone(),
                    package_id,
                    package_revision,
                    source_digest: digest_file(bundle)?,
                });
            }
        }
        Ok(TrustedPluginCatalogResponse {
            authority: Self::lifecycle_authority(),
            entries,
            query: query.to_owned(),
            revision: state.revision().as_str().to_owned(),
            schema: "lenso.agent.trusted-plugin-catalog.v1",
        })
    }

    fn propose_installation(
        &self,
        catalog_entry_id: &str,
        expected_revision: &PluginRootRevision,
    ) -> Result<PluginInstallProposalResponse, String> {
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| "Plugin Root mutation lock is poisoned".to_owned())?;
        self.installation_candidate(catalog_entry_id, expected_revision)
    }

    fn installation_candidate(
        &self,
        catalog_entry_id: &str,
        expected_revision: &PluginRootRevision,
    ) -> Result<PluginInstallProposalResponse, String> {
        if !self.configuration_authority_is_builtin_local {
            return Err(
                "the selected Plugin package authority does not support local Bundle installation"
                    .to_owned(),
            );
        }
        let current = self
            .configuration_authority
            .inspect()
            .map_err(|error| error.to_string())?;
        if current.revision() != expected_revision {
            return Err("Plugin Root revision changed before installation proposal".to_owned());
        }
        let bundle = self
            .trusted_bundles
            .get(catalog_entry_id)
            .ok_or_else(|| "trusted Plugin catalog entry was not found".to_owned())?;
        let staged = StagedHome::new(&self.app_root)?;
        let (package_id, package_revision, _) =
            add_bundle(&staged.home, bundle).map_err(|error| error.to_string())?;
        self.validate_staged(&staged)?;
        let desired = lenso_agent_host::snapshot_desired_plugin_root_for_home(
            &staged.home,
            &self.authority_home,
            self.profile.as_deref(),
        )?;
        let source_digest = digest_file(bundle)?;
        let proposal_digest = lifecycle_proposal_digest(
            "install",
            expected_revision.as_str(),
            desired.plugin_root_revision(),
            &package_id,
            &package_revision,
            Some(catalog_entry_id),
            Some(&source_digest),
        )?;
        Ok(PluginInstallProposalResponse {
            authority: Self::lifecycle_authority(),
            base_revision: expected_revision.as_str().to_owned(),
            candidate_revision: desired.plugin_root_revision().to_owned(),
            catalog_entry_id: catalog_entry_id.to_owned(),
            package_id,
            package_revision,
            proposal_digest,
            schema: "lenso.agent.plugin-install-proposal.v1",
            source_digest,
        })
    }

    fn publish_installation(
        &self,
        catalog_entry_id: &str,
        expected_revision: &PluginRootRevision,
        proposal_digest: &str,
    ) -> Result<PluginInstallPublicationResponse, String> {
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| "Plugin Root mutation lock is poisoned".to_owned())?;
        let proposal = self.installation_candidate(catalog_entry_id, expected_revision)?;
        if proposal.proposal_digest != proposal_digest {
            return Err(
                "Plugin installation proposal no longer matches the reviewed proposal".to_owned(),
            );
        }
        let committed = self.install_unlocked(
            self.trusted_bundles
                .get(catalog_entry_id)
                .expect("candidate checked catalog entry"),
        )?;
        if committed.desired.plugin_root_revision() != proposal.candidate_revision {
            return Err(
                "installed Plugin Root does not match the reviewed candidate revision".to_owned(),
            );
        }
        Ok(PluginInstallPublicationResponse {
            authority: proposal.authority,
            base_revision: proposal.base_revision,
            catalog_entry_id: proposal.catalog_entry_id,
            package_id: proposal.package_id,
            package_revision: proposal.package_revision,
            proposal_digest: proposal.proposal_digest,
            revision: proposal.candidate_revision,
            schema: "lenso.agent.plugin-install-publication.v1",
        })
    }

    fn propose_removal(
        &self,
        plugin_id: &str,
        expected_revision: &PluginRootRevision,
    ) -> Result<PluginRemovalProposalResponse, String> {
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| "Plugin Root mutation lock is poisoned".to_owned())?;
        self.removal_candidate(plugin_id, expected_revision)
    }

    fn removal_candidate(
        &self,
        plugin_id: &str,
        expected_revision: &PluginRootRevision,
    ) -> Result<PluginRemovalProposalResponse, String> {
        if !self.configuration_authority_is_builtin_local {
            return Err(
                "the selected Plugin package authority does not support direct removal".to_owned(),
            );
        }
        let current = self
            .configuration_authority
            .inspect()
            .map_err(|error| error.to_string())?;
        if current.revision() != expected_revision {
            return Err("Plugin Root revision changed before removal proposal".to_owned());
        }
        let plugin = current
            .plugins()
            .iter()
            .find(|plugin| plugin.plugin_id() == plugin_id && plugin.is_root_supplied())
            .ok_or_else(|| "root-supplied Plugin was not found".to_owned())?;
        let package_revision = plugin.release_version().to_owned();
        let staged = StagedHome::new(&self.app_root)?;
        fs::remove_dir_all(staged.home.join("plugins").join(plugin_id))
            .map_err(|error| format!("failed to stage Plugin removal: {error}"))?;
        self.validate_staged(&staged)?;
        let desired = lenso_agent_host::snapshot_desired_plugin_root_for_home(
            &staged.home,
            &self.authority_home,
            self.profile.as_deref(),
        )?;
        let proposal_digest = lifecycle_proposal_digest(
            "remove",
            expected_revision.as_str(),
            desired.plugin_root_revision(),
            plugin_id,
            &package_revision,
            None,
            None,
        )?;
        Ok(PluginRemovalProposalResponse {
            authority: Self::lifecycle_authority(),
            base_revision: expected_revision.as_str().to_owned(),
            candidate_revision: desired.plugin_root_revision().to_owned(),
            package_id: plugin_id.to_owned(),
            package_revision,
            proposal_digest,
            recoverable: true,
            schema: "lenso.agent.plugin-removal-proposal.v1",
        })
    }

    fn publish_removal(
        &self,
        plugin_id: &str,
        expected_revision: &PluginRootRevision,
        proposal_digest: &str,
    ) -> Result<PluginRemovalPublicationResponse, String> {
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| "Plugin Root mutation lock is poisoned".to_owned())?;
        let proposal = self.removal_candidate(plugin_id, expected_revision)?;
        if proposal.proposal_digest != proposal_digest {
            return Err(
                "Plugin removal proposal no longer matches the reviewed proposal".to_owned(),
            );
        }
        let committed = self.remove_unlocked(plugin_id)?;
        if committed.desired.plugin_root_revision() != proposal.candidate_revision {
            return Err(
                "removed Plugin Root does not match the reviewed candidate revision".to_owned(),
            );
        }
        Ok(PluginRemovalPublicationResponse {
            authority: proposal.authority,
            base_revision: proposal.base_revision,
            package_id: proposal.package_id,
            package_revision: proposal.package_revision,
            proposal_digest: proposal.proposal_digest,
            recoverable: true,
            revision: proposal.candidate_revision,
            schema: "lenso.agent.plugin-removal-publication.v1",
        })
    }

    pub(super) fn resolve(
        enabled: bool,
        managed_app_root: Option<&Path>,
        authority_home: &Path,
        profile: Option<String>,
        trusted_bundles: Vec<TrustedPluginBundle>,
        authorities: PluginControlAuthorities,
    ) -> Result<Option<Self>, String> {
        Self::validate_target(managed_app_root, authority_home)?;
        if !enabled {
            return Ok(None);
        }
        let app_root = managed_app_root.unwrap_or(authority_home);
        if profile.is_some() && !authorities.configuration_is_builtin_local {
            return Err(
                "named Profile Plugin control requires the built-in local configuration authority"
                    .to_owned(),
            );
        }
        let trusted_bundle_count = trusted_bundles.len();
        let trusted_bundles = trusted_bundles
            .into_iter()
            .map(|entry| (entry.id, entry.path))
            .collect::<BTreeMap<_, _>>();
        if trusted_bundles.len() != trusted_bundle_count {
            return Err("trusted Plugin Bundle IDs must be unique".to_owned());
        }
        Ok(Some(Self::new(
            app_root,
            authority_home,
            profile,
            trusted_bundles,
            authorities,
        )))
    }

    pub(super) fn validate_target(
        managed_app_root: Option<&Path>,
        authority_home: &Path,
    ) -> Result<(), String> {
        let Some(app_root) = managed_app_root else {
            return Ok(());
        };
        validate_managed_app_root(app_root)?;
        if app_root == authority_home {
            return Ok(());
        }
        let same_root = fs::canonicalize(app_root)
            .and_then(|app_root| {
                fs::canonicalize(authority_home).map(|authority_home| app_root == authority_home)
            })
            .unwrap_or(false);
        if same_root {
            Ok(())
        } else {
            Err(
                "observable Plugin operations require the managed App root to be the Agent Home"
                    .to_owned(),
            )
        }
    }

    pub(super) fn new(
        app_root: &Path,
        authority_home: &Path,
        profile: Option<String>,
        trusted_bundles: BTreeMap<String, PathBuf>,
        authorities: PluginControlAuthorities,
    ) -> Self {
        let PluginControlAuthorities {
            configuration,
            configuration_is_builtin_local,
            history,
            selection,
        } = authorities;
        Self {
            app_root: app_root.to_path_buf(),
            authority_home: authority_home.to_path_buf(),
            configuration_authority: configuration,
            selection_authority: selection,
            configuration_history: history,
            configuration_authority_is_builtin_local: configuration_is_builtin_local,
            mutation: Arc::new(Mutex::new(())),
            profile,
            trusted_bundles,
        }
    }

    pub(super) fn configuration_source(&self) -> PluginConfigurationAuthoritySource {
        self.configuration_authority.source()
    }

    pub(super) fn configuration_authority_response(&self) -> PluginConfigurationAuthorityResponse {
        PluginConfigurationAuthorityResponse::from(self.configuration_source())
            .with_history(self.configuration_history.is_some())
    }

    fn selection_authority_response(&self) -> Option<PluginSelectionAuthorityResponse> {
        self.selection_authority
            .as_ref()
            .map(|authority| PluginSelectionAuthorityResponse::from(authority.source()))
    }

    fn configuration_publication_has_authority_gap(&self) -> bool {
        self.profile.is_none() && !self.configuration_authority_is_builtin_local
    }

    fn install(&self, bundle: &Path) -> Result<CommittedPluginMutation, String> {
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| "Plugin Root mutation lock is poisoned".to_owned())?;
        self.install_unlocked(bundle)
    }

    fn install_unlocked(&self, bundle: &Path) -> Result<CommittedPluginMutation, String> {
        self.mutate_unlocked(|staged| {
            let verification_home = staged.root.join("bundle-verification");
            copy_file(
                &staged.home.join(".lenso/host-catalog.json"),
                &verification_home.join(".lenso/host-catalog.json"),
            )?;
            let (plugin_id, _, _) =
                add_bundle(&verification_home, bundle).map_err(|error| error.to_string())?;
            validate_path_identity(&plugin_id, "Plugin ID")?;
            let candidate = staged.home.join("plugins").join(&plugin_id);
            if candidate.exists() {
                return Err(format!(
                    "Plugin `{plugin_id}` already has a Plugin Root directory"
                ));
            }
            let verified = verification_home.join("plugins").join(&plugin_id);
            fs::create_dir_all(candidate.parent().expect("Plugin candidate has a parent"))
                .map_err(|error| format!("failed to prepare staged Plugin Root: {error}"))?;
            fs::rename(&verified, &candidate).map_err(|error| {
                format!("failed to stage verified Plugin `{plugin_id}`: {error}")
            })?;
            self.validate_staged(staged)?;

            let destination = self.app_root.join("plugins").join(&plugin_id);
            if destination.exists() {
                return Err(format!(
                    "Plugin `{plugin_id}` already has a Plugin Root directory"
                ));
            }
            fs::create_dir_all(
                destination
                    .parent()
                    .expect("Plugin destination has a parent"),
            )
            .map_err(|error| format!("failed to prepare Plugin Root: {error}"))?;
            fs::rename(candidate, &destination)
                .map_err(|error| format!("failed to install Plugin `{plugin_id}`: {error}"))?;
            self.snapshot_committed()
        })
    }

    pub(super) fn inspect(&self) -> Result<PluginManagementResponse, String> {
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| "Plugin Root mutation lock is poisoned".to_owned())?;
        let authority = self.configuration_authority_response();
        if self.profile.is_some() {
            self.ensure_profile_configuration_authority()?;
            let _authoring = lock_plugin_root_authoring(&self.app_root)?;
            return self.inspect_profile(authority).map(|response| {
                response.with_selection_authority(self.selection_authority_response())
            });
        }
        let _authoring = self
            .configuration_authority_is_builtin_local
            .then(|| lock_plugin_root_authoring(&self.app_root))
            .transpose()?;
        let state = self
            .configuration_authority
            .inspect()
            .map_err(|error| error.to_string())?;
        Ok(PluginManagementResponse::from(&state)
            .with_configuration_authority(authority)
            .with_selection_authority(self.selection_authority_response()))
    }

    fn catalog(&self, query: &str) -> Result<PluginCatalogResponse, String> {
        let query_folded = query.to_lowercase();
        let mut plugins = self
            .inspect()?
            .plugins
            .into_iter()
            .filter(|plugin| {
                query_folded.is_empty() || plugin.package_id.to_lowercase().contains(&query_folded)
            })
            .map(|plugin| {
                let active = plugin
                    .instances
                    .iter()
                    .any(|instance| instance.selection == "enabled");
                let mut actions = vec!["configure"];
                if self.selection_authority.is_some()
                    && plugin.instances.iter().any(|instance| instance.disableable)
                {
                    actions.push("set_enabled");
                }
                if plugin.root_supplied {
                    actions.push("remove");
                }
                PluginCatalogItem {
                    actions,
                    instances: plugin.instances,
                    package_id: plugin.package_id,
                    package_revision: plugin.package_revision,
                    source: if plugin.root_supplied {
                        "plugin-root"
                    } else {
                        "host-build"
                    },
                    status: if active { "active" } else { "available" },
                }
            })
            .collect::<Vec<_>>();
        plugins.sort_by(|left, right| left.package_id.cmp(&right.package_id));
        Ok(PluginCatalogResponse {
            installation: PluginCatalogInstallation {
                kind: "local_bundle",
                requires_absolute_path: true,
            },
            plugins,
            query: query.to_owned(),
            schema: "lenso.agent.plugin-catalog.v1",
        })
    }

    fn inspect_profile(
        &self,
        authority: PluginConfigurationAuthorityResponse,
    ) -> Result<PluginManagementResponse, String> {
        let desired = self.snapshot_committed()?;
        ProfileManagementAuthority::load(&self.app_root, &desired)?
            .into_response(desired.plugin_root_revision().to_owned(), authority)
    }

    fn propose_configuration(
        &self,
        plugin_id: &str,
        instance: &str,
        expected_revision: &PluginRootRevision,
        expected_source_digest: &str,
        bytes: &[u8],
    ) -> Result<PluginConfigurationProposalResponse, String> {
        validate_plugin_instance(plugin_id, instance)?;
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| "Plugin Root mutation lock is poisoned".to_owned())?;
        let proposal = self.prepare_configuration_proposal(
            plugin_id,
            instance,
            expected_revision,
            expected_source_digest,
            bytes,
            None,
        )?;
        let authority = self.configuration_authority_response();
        if self.profile.is_some() {
            self.ensure_profile_configuration_authority()?;
            let candidate = self.profile_candidate(plugin_id, instance, bytes, &proposal)?;
            Ok(PluginConfigurationProposalResponse::ready_for_profile(
                &proposal, &candidate, authority,
            ))
        } else {
            Ok(PluginConfigurationProposalResponse::new(
                &proposal, authority,
            ))
        }
    }

    fn prepare_configuration_proposal(
        &self,
        plugin_id: &str,
        instance: &str,
        expected_revision: &PluginRootRevision,
        expected_source_digest: &str,
        bytes: &[u8],
        rollback_of_proposal_digest: Option<&str>,
    ) -> Result<PluginConfigurationProposal, String> {
        ensure_expected_source_current(
            &self.app_root,
            plugin_id,
            instance,
            expected_source_digest,
        )?;
        let proposal = if let Some(publication_proposal_digest) = rollback_of_proposal_digest {
            let history = self.configuration_history.as_ref().ok_or_else(|| {
                "the selected Plugin configuration authority does not expose rollback proposals"
                    .to_owned()
            })?;
            let Some((proposal, configuration_toml)) = history
                .propose_rollback(
                    expected_revision,
                    plugin_id,
                    instance,
                    publication_proposal_digest,
                )
                .map_err(|error| error.to_string())?
            else {
                return Err("Plugin configuration publication was not found".to_owned());
            };
            if configuration_toml.as_bytes() != bytes {
                return Err(
                    "Plugin rollback configuration does not match the reviewed proposal".to_owned(),
                );
            }
            proposal
        } else {
            self.configuration_authority
                .propose(expected_revision, plugin_id, instance, bytes)
                .map_err(|error| error.to_string())?
        };
        ensure_expected_proposal_source(&proposal, expected_source_digest)?;
        Ok(proposal)
    }

    fn publish_configuration(
        &self,
        plugin_id: &str,
        instance: &str,
        reviewed: ReviewedPluginConfiguration<'_>,
    ) -> Result<PublishedPluginConfiguration, String> {
        let ReviewedPluginConfiguration {
            bytes,
            expected_proposal_digest,
            expected_revision,
            expected_source_digest,
            rollback_of_proposal_digest,
        } = reviewed;
        validate_plugin_instance(plugin_id, instance)?;
        if bytes.len() > MAX_PLUGIN_CONFIGURATION_BYTES {
            return Err("Plugin Instance configuration exceeds 256 KiB".to_owned());
        }
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| "Plugin Root mutation lock is poisoned".to_owned())?;
        // Proposal construction owns the authoring lock in lenso-app-authoring.
        // Build it before extending the Host fence, then revalidate its exact
        // source and complete candidate identity under that fence.
        let proposal = self.prepare_configuration_proposal(
            plugin_id,
            instance,
            expected_revision,
            expected_source_digest,
            bytes,
            rollback_of_proposal_digest,
        )?;
        if self.profile.is_some() {
            let linearization =
                PluginMutationLinearization::acquire(&self.app_root, &self.authority_home)?;
            return self.publish_profile_configuration(
                plugin_id,
                instance,
                expected_proposal_digest,
                bytes,
                &proposal,
                linearization,
            );
        }
        if proposal.digest() != expected_proposal_digest {
            return Err(
                "Plugin configuration proposal digest does not match the reviewed proposal"
                    .to_owned(),
            );
        }
        ensure_publishable_proposal(&proposal)?;
        // The built-in authority publishes directly under the shared App lock.
        // Opaque authorities own their own CAS/materialization coordination and
        // must not be recursively locked through a second file descriptor.
        let builtin_linearization = self
            .configuration_authority_is_builtin_local
            .then(|| PluginMutationLinearization::acquire(&self.app_root, &self.authority_home))
            .transpose()?;
        let candidate = self.staged_configuration_snapshot(plugin_id, instance, bytes)?;
        if proposal.candidate_revision().as_str() != candidate.plugin_root_revision() {
            return Err(format!(
                "Plugin configuration proposal resolves {} but Host validation resolves {}",
                proposal.candidate_revision(),
                candidate.plugin_root_revision()
            ));
        }
        if self.configuration_authority_is_builtin_local {
            return self.publish_builtin_configuration(
                plugin_id,
                instance,
                bytes,
                &proposal,
                builtin_linearization.expect("built-in publication acquired linearization guards"),
            );
        }
        let publication = self
            .configuration_authority
            .publish(&proposal)
            .map_err(|error| error.to_string())?;
        // Opaque authorities own publication locking. Fence the Host only after
        // their CAS completes so an authority that wraps the local adapter cannot
        // invert authoring-lock -> Generation-fence ordering and deadlock itself.
        let linearization =
            PluginMutationLinearization::acquire(&self.app_root, &self.authority_home)?;
        let desired = self.snapshot_committed()?;
        if publication.revision().as_str() != desired.plugin_root_revision() {
            return Err(format!(
                "Plugin configuration authority published revision {} but materialized {}",
                publication.revision(),
                desired.plugin_root_revision()
            ));
        }
        Ok(PublishedPluginConfiguration {
            base_revision: publication.base_revision().as_str().to_owned(),
            base_source_digest: publication.base_source_digest().as_str().to_owned(),
            configuration_authority: self.configuration_authority_response(),
            desired,
            proposal_digest: publication.proposal_digest().to_owned(),
            revision: publication.revision().as_str().to_owned(),
            schema: publication.schema().to_owned(),
            linearization,
        })
    }

    fn publish_builtin_configuration(
        &self,
        plugin_id: &str,
        instance: &str,
        bytes: &[u8],
        proposal: &PluginConfigurationProposal,
        linearization: PluginMutationLinearization,
    ) -> Result<PublishedPluginConfiguration, String> {
        ensure_proposal_source_current(&self.app_root, proposal)?;
        atomic_write(
            &self
                .app_root
                .join(plugin_configuration_path(plugin_id, instance)),
            bytes,
        )?;
        let desired = self.snapshot_committed()?;
        if proposal.candidate_revision().as_str() != desired.plugin_root_revision() {
            return Err(format!(
                "Plugin configuration proposal materialized {} but resolved {}",
                proposal.candidate_revision(),
                desired.plugin_root_revision()
            ));
        }
        Ok(PublishedPluginConfiguration {
            base_revision: proposal.base_revision().as_str().to_owned(),
            base_source_digest: proposal.base_source_digest().as_str().to_owned(),
            configuration_authority: self.configuration_authority_response(),
            desired,
            proposal_digest: proposal.digest().to_owned(),
            revision: proposal.candidate_revision().as_str().to_owned(),
            schema: "lenso.plugin-configuration-publication.v1".to_owned(),
            linearization,
        })
    }

    fn publish_profile_configuration(
        &self,
        plugin_id: &str,
        instance: &str,
        expected_proposal_digest: &str,
        bytes: &[u8],
        proposal: &PluginConfigurationProposal,
        linearization: PluginMutationLinearization,
    ) -> Result<PublishedPluginConfiguration, String> {
        let authority = self.configuration_authority_response();
        self.ensure_profile_configuration_authority()?;
        let candidate = self.profile_candidate(plugin_id, instance, bytes, proposal)?;
        if candidate.proposal_digest != expected_proposal_digest {
            return Err(format!(
                "Plugin configuration proposal digest does not match the reviewed proposal: expected {expected_proposal_digest}, recomputed {}",
                candidate.proposal_digest
            ));
        }
        ensure_proposal_source_current(&self.app_root, proposal)?;
        let path = self
            .app_root
            .join(plugin_configuration_path(plugin_id, instance));
        atomic_write(&path, bytes)?;
        let desired = self.snapshot_committed()?;
        if desired.plugin_root_revision() != candidate.desired.plugin_root_revision() {
            return Err(
                "published Plugin Root does not match the reviewed candidate revision".to_owned(),
            );
        }
        Ok(PublishedPluginConfiguration {
            base_revision: proposal.base_revision().as_str().to_owned(),
            base_source_digest: proposal.base_source_digest().as_str().to_owned(),
            configuration_authority: authority,
            desired,
            proposal_digest: candidate.proposal_digest,
            revision: candidate.desired.plugin_root_revision().to_owned(),
            schema: "lenso.plugin-configuration-publication.v1".to_owned(),
            linearization,
        })
    }

    fn configuration_publications(
        &self,
        plugin_id: &str,
        instance: &str,
    ) -> Result<PluginConfigurationHistoryResponse, String> {
        validate_plugin_instance(plugin_id, instance)?;
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| "Plugin Root mutation lock is poisoned".to_owned())?;
        let history = self.configuration_history.as_ref().ok_or_else(|| {
            "the selected Plugin configuration authority does not expose publication history"
                .to_owned()
        })?;
        history
            .publications(plugin_id, instance, 20)
            .map(|publications| PluginConfigurationHistoryResponse {
                configuration_authority: self.configuration_authority_response(),
                instance_key: instance.to_owned(),
                plugin_id: plugin_id.to_owned(),
                publications: publications.into_iter().map(Into::into).collect(),
                schema: "lenso.agent.plugin-configuration-history.v1",
            })
            .map_err(|error| error.to_string())
    }

    fn propose_configuration_rollback(
        &self,
        plugin_id: &str,
        instance: &str,
        expected_revision: &PluginRootRevision,
        expected_source_digest: &str,
        publication_proposal_digest: &str,
    ) -> Result<Option<PluginConfigurationRollbackProposalResponse>, String> {
        validate_plugin_instance(plugin_id, instance)?;
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| "Plugin Root mutation lock is poisoned".to_owned())?;
        ensure_expected_source_current(
            &self.app_root,
            plugin_id,
            instance,
            expected_source_digest,
        )?;
        let history = self.configuration_history.as_ref().ok_or_else(|| {
            "the selected Plugin configuration authority does not expose rollback proposals"
                .to_owned()
        })?;
        let rollback = history
            .propose_rollback(
                expected_revision,
                plugin_id,
                instance,
                publication_proposal_digest,
            )
            .map_err(|error| error.to_string())?;
        let Some((proposal, configuration_toml)) = rollback else {
            return Ok(None);
        };
        ensure_expected_proposal_source(&proposal, expected_source_digest)?;
        Ok(Some(PluginConfigurationRollbackProposalResponse {
            configuration_toml,
            proposal: PluginConfigurationProposalResponse::new(
                &proposal,
                self.configuration_authority_response(),
            ),
            rollback_of_proposal_digest: publication_proposal_digest.to_owned(),
            schema: "lenso.agent.plugin-configuration-rollback-proposal.v1",
        }))
    }

    fn profile_candidate(
        &self,
        plugin_id: &str,
        instance: &str,
        bytes: &[u8],
        proposal: &PluginConfigurationProposal,
    ) -> Result<ProfileConfigurationCandidate, String> {
        let desired = self.staged_configuration_snapshot(plugin_id, instance, bytes)?;
        let catalog_path = self.app_root.join(".lenso/host-catalog.json");
        let catalog_digest = lenso_plugin_control_plane::sha256_digest(
            &fs::read(&catalog_path)
                .map_err(|error| format!("failed to read {}: {error}", catalog_path.display()))?,
        );
        let profile_name = self
            .profile
            .as_deref()
            .expect("profile candidate requires a named Profile");
        let profile_path = self
            .app_root
            .join("profiles")
            .join(format!("{profile_name}.toml"));
        let profile_digest = lenso_plugin_control_plane::sha256_digest(
            &fs::read(&profile_path)
                .map_err(|error| format!("failed to read {}: {error}", profile_path.display()))?,
        );
        let proposal_authority = serde_json::to_vec(&serde_json::json!({
            "baseRevision": proposal.base_revision().as_str(),
            "baseSourceDigest": proposal.base_source_digest().as_str(),
            "candidateRevision": desired.plugin_root_revision(),
            "hostCatalogDigest": catalog_digest,
            "instanceKey": instance,
            "pluginId": plugin_id,
            "profile": profile_name,
            "profileDigest": profile_digest,
            "schema": "lenso.agent.profile-plugin-configuration-proposal@1",
        }))
        .map_err(|error| format!("failed to encode profile proposal authority: {error}"))?;
        Ok(ProfileConfigurationCandidate {
            desired,
            proposal_digest: lenso_plugin_control_plane::sha256_digest(&proposal_authority),
        })
    }

    fn staged_configuration_snapshot(
        &self,
        plugin_id: &str,
        instance: &str,
        bytes: &[u8],
    ) -> Result<lenso_agent_host::DesiredPluginRootSnapshot, String> {
        let staged = StagedHome::new(&self.app_root)?;
        atomic_write(
            &staged
                .home
                .join(plugin_configuration_path(plugin_id, instance)),
            bytes,
        )?;
        lenso_agent_host::snapshot_desired_plugin_root_for_home(
            &staged.home,
            &self.authority_home,
            self.profile.as_deref(),
        )
    }

    fn set_enabled(
        &self,
        expected_revision: &PluginRootRevision,
        plugin_id: &str,
        instance: &str,
        enabled: bool,
    ) -> Result<CommittedPluginMutation, String> {
        validate_plugin_instance(plugin_id, instance)?;
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| "Plugin Root mutation lock is poisoned".to_owned())?;
        let authority = self.selection_authority.as_ref().ok_or_else(|| {
            "the selected Plugin authority does not support selection changes".to_owned()
        })?;
        let publication = authority
            .set_enabled(expected_revision, plugin_id, instance, enabled)
            .map_err(|error| error.to_string())?;
        // Selection authorities own their authoring lock and CAS. Extend the
        // Host fence only after publication to avoid inverting an opaque
        // authority's lock order, matching opaque configuration publication.
        let linearization =
            PluginMutationLinearization::acquire(&self.app_root, &self.authority_home)?;
        let desired = self.snapshot_committed()?;
        if publication.revision().as_str() != desired.plugin_root_revision() {
            return Err(format!(
                "Plugin selection authority published revision {} but materialized {}",
                publication.revision(),
                desired.plugin_root_revision()
            ));
        }
        Ok(CommittedPluginMutation {
            desired,
            linearization,
        })
    }

    fn remove_instance(
        &self,
        plugin_id: &str,
        instance: &str,
    ) -> Result<CommittedPluginMutation, String> {
        validate_plugin_instance(plugin_id, instance)?;
        self.mutate(|staged| {
            let configuration = plugin_configuration_path(plugin_id, instance);
            let disabled = plugin_disabled_path(plugin_id, instance);
            remove_file_if_exists(&staged.home.join(&configuration))?;
            remove_file_if_exists(&staged.home.join(&disabled))?;
            self.validate_staged(staged)?;
            let transaction = FileRemovalTransaction::begin(
                &self.app_root,
                &[configuration, disabled],
                |_, _| Ok(()),
            )?;
            match self.snapshot_committed() {
                Ok(desired) => {
                    transaction.commit();
                    Ok(desired)
                }
                Err(error) => Err(transaction.rollback_after(error)),
            }
        })
    }

    fn remove(&self, plugin_id: &str) -> Result<CommittedPluginMutation, String> {
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| "Plugin Root mutation lock is poisoned".to_owned())?;
        self.remove_unlocked(plugin_id)
    }

    fn remove_unlocked(&self, plugin_id: &str) -> Result<CommittedPluginMutation, String> {
        validate_path_identity(plugin_id, "Plugin ID")?;
        self.mutate_unlocked(|staged| {
            let staged_plugin = staged.home.join("plugins").join(plugin_id);
            if !staged_plugin.is_dir() {
                return Err(format!("Plugin `{plugin_id}` has no Plugin Root directory"));
            }
            fs::remove_dir_all(&staged_plugin)
                .map_err(|error| format!("failed to stage Plugin removal: {error}"))?;
            self.validate_staged(staged)?;

            let plugin = self.app_root.join("plugins").join(plugin_id);
            if !plugin.is_dir() {
                return Err(format!("Plugin `{plugin_id}` has no Plugin Root directory"));
            }
            let trash = self
                .app_root
                .join(".lenso/trash")
                .join(format!("{plugin_id}-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(trash.parent().expect("Plugin trash has a parent"))
                .map_err(|error| format!("failed to prepare Plugin trash: {error}"))?;
            fs::rename(&plugin, &trash)
                .map_err(|error| format!("failed to move Plugin to recoverable trash: {error}"))?;
            self.snapshot_committed()
        })
    }

    fn mutate(
        &self,
        operation: impl FnOnce(
            &StagedHome,
        ) -> Result<lenso_agent_host::DesiredPluginRootSnapshot, String>,
    ) -> Result<CommittedPluginMutation, String> {
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| "Plugin Root mutation lock is poisoned".to_owned())?;
        self.mutate_unlocked(operation)
    }

    fn mutate_unlocked(
        &self,
        operation: impl FnOnce(
            &StagedHome,
        ) -> Result<lenso_agent_host::DesiredPluginRootSnapshot, String>,
    ) -> Result<CommittedPluginMutation, String> {
        if !self.configuration_authority_is_builtin_local {
            return Err(
                "the selected Plugin configuration authority does not permit direct Plugin Root mutation"
                    .to_owned(),
            );
        }
        let authoring = lock_plugin_root_authoring(&self.app_root)?;
        let generation =
            lenso_agent_host::fence_plugin_root_mutation_for_home(&self.authority_home)?;
        let staged = StagedHome::new(&self.app_root)?;
        let desired = operation(&staged)?;
        Ok(CommittedPluginMutation {
            desired,
            linearization: PluginMutationLinearization {
                _authoring: authoring,
                _generation: generation,
            },
        })
    }

    fn validate_staged(&self, staged: &StagedHome) -> Result<(), String> {
        lenso_agent_host::validate_desired_plugin_root_for_home(
            &staged.home,
            self.profile.as_deref(),
        )
    }

    fn snapshot_committed(&self) -> Result<lenso_agent_host::DesiredPluginRootSnapshot, String> {
        lenso_agent_host::snapshot_desired_plugin_root_for_home(
            &self.app_root,
            &self.authority_home,
            self.profile.as_deref(),
        )
    }

    fn ensure_profile_configuration_authority(&self) -> Result<(), String> {
        if self.configuration_authority_is_builtin_local {
            Ok(())
        } else {
            Err(
                "named Profile Plugin control requires the built-in local configuration authority"
                    .to_owned(),
            )
        }
    }
}

fn lifecycle_authority_contract(
    authority: PluginConfigurationAuthorityResponse,
) -> target_contract::AuthoritySource {
    target_contract::AuthoritySource {
        kind: authority.kind,
        reference: authority.reference,
    }
}

macro_rules! forward_external {
    ($self:ident, $request:ident, $method:ident, $error:ident) => {
        match $self.external.as_ref() {
            Some(target) => target.$method($request),
            None => Box::pin(async { Ok(Err(target_contract::$error::TargetNotFound)) }),
        }
    };
}

impl PluginManagementTarget for RoutedPluginManagementTarget {
    fn history(
        &self,
        request: target_contract::HistoryRequest,
    ) -> lenso_kernel::NativeRequestFuture<target_contract::PluginManagementTargetHistory> {
        forward_external!(self, request, history, HistoryError)
    }
    fn inspect(
        &self,
        request: target_contract::InspectRequest,
    ) -> lenso_kernel::NativeRequestFuture<target_contract::PluginManagementTargetInspect> {
        forward_external!(self, request, inspect, InspectError)
    }
    fn propose(
        &self,
        request: target_contract::ProposeRequest,
    ) -> lenso_kernel::NativeRequestFuture<target_contract::PluginManagementTargetPropose> {
        forward_external!(self, request, propose, ProposeError)
    }
    fn propose_rollback(
        &self,
        request: target_contract::ProposeRollbackRequest,
    ) -> lenso_kernel::NativeRequestFuture<target_contract::PluginManagementTargetProposeRollback>
    {
        forward_external!(self, request, propose_rollback, ProposeRollbackError)
    }
    fn publish(
        &self,
        request: target_contract::PublishRequest,
    ) -> lenso_kernel::NativeRequestFuture<target_contract::PluginManagementTargetPublish> {
        forward_external!(self, request, publish, PublishError)
    }
    fn publish_rollback(
        &self,
        request: target_contract::PublishRollbackRequest,
    ) -> lenso_kernel::NativeRequestFuture<target_contract::PluginManagementTargetPublishRollback>
    {
        forward_external!(self, request, publish_rollback, PublishRollbackError)
    }
    fn set_enabled(
        &self,
        request: target_contract::SetEnabledRequest,
    ) -> lenso_kernel::NativeRequestFuture<target_contract::PluginManagementTargetSetEnabled> {
        forward_external!(self, request, set_enabled, SetEnabledError)
    }

    fn catalog(
        &self,
        request: target_contract::CatalogRequest,
    ) -> lenso_kernel::NativeRequestFuture<target_contract::PluginManagementTargetCatalog> {
        if request.agent_id != "console" {
            return forward_external!(self, request, catalog, CatalogError);
        }
        let Some(control) = self.local.as_ref() else {
            return Box::pin(async { Ok(Err(target_contract::CatalogError::Unsupported)) });
        };
        let result = control
            .trusted_catalog(&request.query)
            .map(|catalog| target_contract::CatalogResponse {
                agent_id: request.agent_id,
                authority: lifecycle_authority_contract(catalog.authority),
                entries: catalog
                    .entries
                    .into_iter()
                    .map(|entry| target_contract::CatalogEntry {
                        catalog_entry_id: entry.catalog_entry_id,
                        package_id: entry.package_id,
                        package_revision: entry.package_revision,
                        source_digest: entry.source_digest,
                    })
                    .collect(),
                query: catalog.query,
                revision: catalog.revision,
            })
            .map_err(|_| target_contract::CatalogError::Unsupported);
        Box::pin(async move { Ok(result) })
    }

    fn propose_install(
        &self,
        request: target_contract::ProposeInstallRequest,
    ) -> lenso_kernel::NativeRequestFuture<target_contract::PluginManagementTargetProposeInstall>
    {
        if request.agent_id != "console" {
            return forward_external!(self, request, propose_install, ProposeInstallError);
        }
        let Some(control) = self.local.as_ref() else {
            return Box::pin(async { Ok(Err(target_contract::ProposeInstallError::Unsupported)) });
        };
        let result = request
            .expected_revision
            .parse::<PluginRootRevision>()
            .map_err(|_| target_contract::ProposeInstallError::InvalidRequest)
            .and_then(|revision| {
                control
                    .propose_installation(&request.catalog_entry_id, &revision)
                    .map_err(|error| {
                        if error.contains("revision changed") {
                            target_contract::ProposeInstallError::Conflict
                        } else if error.contains("not found") {
                            target_contract::ProposeInstallError::PluginNotFound
                        } else {
                            target_contract::ProposeInstallError::Unsupported
                        }
                    })
            })
            .map(|proposal| target_contract::InstallProposalResponse {
                agent_id: request.agent_id,
                authority: lifecycle_authority_contract(proposal.authority),
                base_revision: proposal.base_revision,
                catalog_entry_id: proposal.catalog_entry_id,
                candidate_revision: proposal.candidate_revision,
                package_id: proposal.package_id,
                package_revision: proposal.package_revision,
                proposal_digest: proposal.proposal_digest,
                source_digest: proposal.source_digest,
            });
        Box::pin(async move { Ok(result) })
    }

    fn publish_install(
        &self,
        request: target_contract::PublishInstallRequest,
    ) -> lenso_kernel::NativeRequestFuture<target_contract::PluginManagementTargetPublishInstall>
    {
        if request.agent_id != "console" {
            return forward_external!(self, request, publish_install, PublishInstallError);
        }
        let Some(control) = self.local.as_ref() else {
            return Box::pin(async { Ok(Err(target_contract::PublishInstallError::Unsupported)) });
        };
        let result = request
            .expected_revision
            .parse::<PluginRootRevision>()
            .map_err(|_| target_contract::PublishInstallError::InvalidRequest)
            .and_then(|revision| {
                control
                    .publish_installation(
                        &request.catalog_entry_id,
                        &revision,
                        &request.proposal_digest,
                    )
                    .map_err(|error| {
                        if error.contains("revision changed") {
                            target_contract::PublishInstallError::Conflict
                        } else if error.contains("no longer matches") {
                            target_contract::PublishInstallError::ProposalMismatch
                        } else if error.contains("not found") {
                            target_contract::PublishInstallError::PluginNotFound
                        } else {
                            target_contract::PublishInstallError::Unsupported
                        }
                    })
            })
            .map(|publication| target_contract::PublishInstallResponse {
                agent_id: request.agent_id,
                authority: lifecycle_authority_contract(publication.authority),
                base_revision: publication.base_revision,
                catalog_entry_id: publication.catalog_entry_id,
                package_id: publication.package_id,
                package_revision: publication.package_revision,
                proposal_digest: publication.proposal_digest,
                revision: publication.revision,
            });
        Box::pin(async move { Ok(result) })
    }

    fn propose_removal(
        &self,
        request: target_contract::ProposeRemovalRequest,
    ) -> lenso_kernel::NativeRequestFuture<target_contract::PluginManagementTargetProposeRemoval>
    {
        if request.agent_id != "console" {
            return forward_external!(self, request, propose_removal, ProposeRemovalError);
        }
        let Some(control) = self.local.as_ref() else {
            return Box::pin(async { Ok(Err(target_contract::ProposeRemovalError::Unsupported)) });
        };
        let result = request
            .expected_revision
            .parse::<PluginRootRevision>()
            .map_err(|_| target_contract::ProposeRemovalError::InvalidRequest)
            .and_then(|revision| {
                control
                    .propose_removal(&request.plugin_id, &revision)
                    .map_err(|error| {
                        if error.contains("revision changed") {
                            target_contract::ProposeRemovalError::Conflict
                        } else if error.contains("not found") {
                            target_contract::ProposeRemovalError::PluginNotFound
                        } else {
                            target_contract::ProposeRemovalError::Unsupported
                        }
                    })
            })
            .map(|proposal| target_contract::RemovalProposalResponse {
                agent_id: request.agent_id,
                authority: lifecycle_authority_contract(proposal.authority),
                base_revision: proposal.base_revision,
                candidate_revision: proposal.candidate_revision,
                package_id: proposal.package_id,
                package_revision: proposal.package_revision,
                proposal_digest: proposal.proposal_digest,
                recoverable: proposal.recoverable,
            });
        Box::pin(async move { Ok(result) })
    }

    fn publish_removal(
        &self,
        request: target_contract::PublishRemovalRequest,
    ) -> lenso_kernel::NativeRequestFuture<target_contract::PluginManagementTargetPublishRemoval>
    {
        if request.agent_id != "console" {
            return forward_external!(self, request, publish_removal, PublishRemovalError);
        }
        let Some(control) = self.local.as_ref() else {
            return Box::pin(async { Ok(Err(target_contract::PublishRemovalError::Unsupported)) });
        };
        let result = request
            .expected_revision
            .parse::<PluginRootRevision>()
            .map_err(|_| target_contract::PublishRemovalError::InvalidRequest)
            .and_then(|revision| {
                control
                    .publish_removal(&request.plugin_id, &revision, &request.proposal_digest)
                    .map_err(|error| {
                        if error.contains("revision changed") {
                            target_contract::PublishRemovalError::Conflict
                        } else if error.contains("no longer matches") {
                            target_contract::PublishRemovalError::ProposalMismatch
                        } else if error.contains("not found") {
                            target_contract::PublishRemovalError::PluginNotFound
                        } else {
                            target_contract::PublishRemovalError::Unsupported
                        }
                    })
            })
            .map(|publication| target_contract::PublishRemovalResponse {
                agent_id: request.agent_id,
                authority: lifecycle_authority_contract(publication.authority),
                base_revision: publication.base_revision,
                package_id: publication.package_id,
                package_revision: publication.package_revision,
                proposal_digest: publication.proposal_digest,
                recoverable: publication.recoverable,
                revision: publication.revision,
            });
        Box::pin(async move { Ok(result) })
    }
}

fn validate_managed_app_root(app_root: &Path) -> Result<(), String> {
    if !app_root.is_absolute() {
        return Err(format!(
            "managed App root must be an absolute path: {}",
            app_root.display()
        ));
    }
    if app_root.to_str().is_none() {
        return Err(format!(
            "managed App root must be valid UTF-8: {}",
            app_root.display()
        ));
    }
    Ok(())
}

fn digest_file(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|error| {
        format!(
            "failed to read trusted Plugin Bundle {}: {error}",
            path.display()
        )
    })?;
    Ok(lenso_plugin_control_plane::sha256_digest(&bytes))
}

fn lifecycle_proposal_digest(
    operation: &str,
    base_revision: &str,
    candidate_revision: &str,
    package_id: &str,
    package_revision: &str,
    catalog_entry_id: Option<&str>,
    source_digest: Option<&str>,
) -> Result<String, String> {
    let authority = serde_json::to_vec(&serde_json::json!({
        "baseRevision": base_revision,
        "candidateRevision": candidate_revision,
        "catalogEntryId": catalog_entry_id,
        "operation": operation,
        "packageId": package_id,
        "packageRevision": package_revision,
        "schema": "lenso.agent.plugin-lifecycle-proposal@1",
        "sourceDigest": source_digest,
    }))
    .map_err(|error| format!("failed to encode Plugin lifecycle proposal: {error}"))?;
    Ok(lenso_plugin_control_plane::sha256_digest(&authority))
}

fn ensure_publishable_proposal(proposal: &PluginConfigurationProposal) -> Result<(), String> {
    if proposal.status() != PluginConfigurationProposalStatus::Ready
        || proposal.application() == PluginConfigurationApplication::Blocked
    {
        let detail = proposal
            .diagnostics()
            .first()
            .map_or("candidate did not pass the Ready Gate", |diagnostic| {
                diagnostic.detail()
            });
        return Err(format!(
            "Plugin configuration proposal cannot be published: {detail}"
        ));
    }
    Ok(())
}

fn ensure_proposal_source_current(
    app_root: &Path,
    proposal: &PluginConfigurationProposal,
) -> Result<(), String> {
    let current = current_configuration_source_digest(
        app_root,
        proposal.plugin_id(),
        proposal.instance_key(),
    )?;
    if &current != proposal.base_source_digest() {
        return Err(format!(
            "Plugin configuration source conflict: expected {}, current {current}",
            proposal.base_source_digest()
        ));
    }
    Ok(())
}

fn ensure_expected_source_current(
    app_root: &Path,
    plugin_id: &str,
    instance: &str,
    expected_source_digest: &str,
) -> Result<(), String> {
    let current = current_configuration_source_digest(app_root, plugin_id, instance)?;
    if current.as_str() != expected_source_digest {
        return Err(format!(
            "Plugin configuration source conflict: expected {expected_source_digest}, current {current}"
        ));
    }
    Ok(())
}

fn ensure_expected_proposal_source(
    proposal: &PluginConfigurationProposal,
    expected_source_digest: &str,
) -> Result<(), String> {
    if proposal.base_source_digest().as_str() != expected_source_digest {
        return Err(format!(
            "Plugin configuration proposal source does not match reviewed source: expected {expected_source_digest}, proposal {}",
            proposal.base_source_digest()
        ));
    }
    Ok(())
}

fn current_configuration_source_digest(
    app_root: &Path,
    plugin_id: &str,
    instance: &str,
) -> Result<PluginConfigurationSourceDigest, String> {
    validate_plugin_instance(plugin_id, instance)?;
    let path = app_root.join(plugin_configuration_path(plugin_id, instance));
    let bytes = match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            if metadata.len() > MAX_PLUGIN_CONFIGURATION_BYTES as u64 {
                return Err(format!(
                    "Plugin Instance configuration exceeds 256 KiB: {}",
                    path.display()
                ));
            }
            Some(
                fs::read(&path)
                    .map_err(|error| format!("failed to read {}: {error}", path.display()))?,
            )
        }
        Ok(_) => {
            return Err(format!(
                "Plugin configuration source must be a regular file: {}",
                path.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(format!("failed to inspect {}: {error}", path.display()));
        }
    };
    let current =
        PluginConfigurationSourceDigest::for_source(plugin_id, instance, bytes.as_deref())
            .map_err(|error| error.to_string())?;
    Ok(current)
}

#[derive(Debug)]
struct PublishedPluginConfiguration {
    base_revision: String,
    base_source_digest: String,
    configuration_authority: PluginConfigurationAuthorityResponse,
    desired: lenso_agent_host::DesiredPluginRootSnapshot,
    proposal_digest: String,
    revision: String,
    schema: String,
    linearization: PluginMutationLinearization,
}

#[derive(Debug)]
struct CommittedPluginMutation {
    desired: lenso_agent_host::DesiredPluginRootSnapshot,
    linearization: PluginMutationLinearization,
}

#[derive(Debug)]
struct PluginMutationLinearization {
    _authoring: fs::File,
    _generation: lenso_agent_host::PluginRootMutationFence,
}

impl PluginMutationLinearization {
    fn acquire(app_root: &Path, authority_home: &Path) -> Result<Self, String> {
        let authoring = lock_plugin_root_authoring(app_root)?;
        let generation = lenso_agent_host::fence_plugin_root_mutation_for_home(authority_home)?;
        Ok(Self {
            _authoring: authoring,
            _generation: generation,
        })
    }
}

#[derive(Debug)]
struct ProfileConfigurationCandidate {
    desired: lenso_agent_host::DesiredPluginRootSnapshot,
    proposal_digest: String,
}

#[derive(Debug)]
struct StagedHome {
    root: PathBuf,
    home: PathBuf,
}

impl StagedHome {
    fn new(source: &Path) -> Result<Self, String> {
        let root = source
            .join(".lenso/plugin-control-staging")
            .join(uuid::Uuid::new_v4().to_string());
        let staged = Self {
            home: root.join("home"),
            root,
        };
        fs::create_dir_all(&staged.home)
            .map_err(|error| format!("failed to create Plugin mutation staging root: {error}"))?;
        copy_file(
            &source.join(".lenso/host-catalog.json"),
            &staged.home.join(".lenso/host-catalog.json"),
        )?;
        let mut budget = StagingBudget::new(MAX_STAGING_ENTRIES);
        link_directory_if_exists(
            &source.join("plugins"),
            &staged.home.join("plugins"),
            &mut budget,
        )?;
        link_directory_if_exists(
            &source.join("profiles"),
            &staged.home.join("profiles"),
            &mut budget,
        )?;
        Ok(staged)
    }
}

impl Drop for StagedHome {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[derive(Debug)]
struct FileRemovalTransaction {
    active: bool,
    directory: PathBuf,
    moved: Vec<(PathBuf, PathBuf)>,
}

impl FileRemovalTransaction {
    fn begin(
        root: &Path,
        relative_paths: &[PathBuf],
        mut before_move: impl FnMut(usize, &Path) -> Result<(), String>,
    ) -> Result<Self, String> {
        let directory = root
            .join(".lenso/plugin-control-transactions")
            .join(uuid::Uuid::new_v4().to_string());
        fs::create_dir_all(&directory).map_err(|error| {
            format!(
                "failed to prepare Plugin removal transaction {}: {error}",
                directory.display()
            )
        })?;
        let mut transaction = Self {
            active: true,
            directory,
            moved: Vec::new(),
        };
        for (index, relative) in relative_paths.iter().enumerate() {
            let source = root.join(relative);
            if !source.exists() {
                continue;
            }
            let destination = transaction.directory.join(index.to_string());
            let step = before_move(index, &source).and_then(|()| {
                fs::rename(&source, &destination).map_err(|error| {
                    format!("failed to stage removal of {}: {error}", source.display())
                })
            });
            if let Err(error) = step {
                return Err(transaction.rollback_after(error));
            }
            transaction.moved.push((source, destination));
        }
        Ok(transaction)
    }

    fn commit(mut self) {
        self.active = false;
        let _ = fs::remove_dir_all(&self.directory);
    }

    fn rollback_after(mut self, error: String) -> String {
        match self.rollback() {
            Ok(()) => error,
            Err(rollback) => format!("{error}; Plugin removal rollback failed: {rollback}"),
        }
    }

    fn rollback(&mut self) -> Result<(), String> {
        self.active = false;
        for (source, destination) in self.moved.iter().rev() {
            fs::rename(destination, source).map_err(|error| {
                format!(
                    "failed to restore {} from {}: {error}",
                    source.display(),
                    destination.display()
                )
            })?;
        }
        let _ = fs::remove_dir_all(&self.directory);
        Ok(())
    }
}

impl Drop for FileRemovalTransaction {
    fn drop(&mut self) {
        if self.active {
            let _ = self.rollback();
        }
    }
}

fn validate_plugin_instance(plugin_id: &str, instance: &str) -> Result<(), String> {
    validate_path_identity(plugin_id, "Plugin ID")?;
    validate_path_identity(instance, "Instance key")?;
    if instance.starts_with('.') || instance == "plugin" {
        return Err(format!("reserved Plugin Instance key `{instance}`"));
    }
    Ok(())
}

fn validate_path_identity(value: &str, label: &str) -> Result<(), String> {
    if value.trim() != value
        || value.is_empty()
        || value == "."
        || value == ".."
        || value.contains(['/', '\0', '\\'])
    {
        return Err(format!("invalid {label} `{value}`"));
    }
    Ok(())
}

fn plugin_configuration_path(plugin_id: &str, instance: &str) -> PathBuf {
    Path::new("plugins")
        .join(plugin_id)
        .join(format!("{instance}.toml"))
}

fn plugin_disabled_path(plugin_id: &str, instance: &str) -> PathBuf {
    Path::new("plugins")
        .join(plugin_id)
        .join(format!("{instance}.disabled"))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "Plugin file has no parent".to_owned())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    let temporary = parent.join(format!(".plugin-control-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| format!("failed to create {}: {error}", temporary.display()))?;
        file.write_all(bytes)
            .map_err(|error| format!("failed to write {}: {error}", temporary.display()))?;
        file.sync_all()
            .map_err(|error| format!("failed to sync {}: {error}", temporary.display()))?;
        fs::rename(&temporary, path)
            .map_err(|error| format!("failed to publish {}: {error}", path.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn lock_plugin_root_authoring(root: &Path) -> Result<fs::File, String> {
    let path = root.join(".lenso/plugin-root-authoring.lock");
    fs::create_dir_all(
        path.parent()
            .expect("Plugin Root authoring lock has a parent"),
    )
    .map_err(|error| format!("failed to prepare Plugin Root authoring lock: {error}"))?;
    let file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| format!("failed to open {}: {error}", path.display()))?;
    file.lock()
        .map_err(|error| format!("failed to lock {}: {error}", path.display()))?;
    Ok(file)
}

fn remove_file_if_exists(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("failed to remove {}: {error}", path.display())),
    }
}

#[derive(Debug)]
struct StagingBudget {
    remaining_entries: usize,
}

impl StagingBudget {
    const fn new(remaining_entries: usize) -> Self {
        Self { remaining_entries }
    }

    fn consume(&mut self, path: &Path) -> Result<(), String> {
        self.remaining_entries = self.remaining_entries.checked_sub(1).ok_or_else(|| {
            format!(
                "Plugin mutation staging exceeds {MAX_STAGING_ENTRIES} filesystem entries at {}",
                path.display()
            )
        })?;
        Ok(())
    }
}

fn link_directory_if_exists(
    source: &Path,
    destination: &Path,
    budget: &mut StagingBudget,
) -> Result<(), String> {
    if !source.exists() {
        return Ok(());
    }
    link_directory(source, destination, budget)
}

fn link_directory(
    source: &Path,
    destination: &Path,
    budget: &mut StagingBudget,
) -> Result<(), String> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|error| format!("failed to inspect {}: {error}", source.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "Plugin mutation staging source must be a regular directory: {}",
            source.display()
        ));
    }
    fs::create_dir_all(destination)
        .map_err(|error| format!("failed to create {}: {error}", destination.display()))?;
    let entries = fs::read_dir(source)
        .map_err(|error| format!("failed to read {}: {error}", source.display()))?;
    for entry in entries {
        let entry = entry
            .map_err(|error| format!("failed to read an entry in {}: {error}", source.display()))?;
        budget.consume(&entry.path())?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("failed to inspect {}: {error}", entry.path().display()))?;
        let target = destination.join(entry.file_name());
        if file_type.is_dir() {
            link_directory(&entry.path(), &target, budget)?;
        } else if file_type.is_file() {
            fs::hard_link(entry.path(), &target).map_err(|error| {
                format!(
                    "failed to link {} into bounded Plugin staging as {}: {error}",
                    entry.path().display(),
                    target.display()
                )
            })?;
        } else {
            return Err(format!(
                "Plugin mutation staging rejects non-regular path {}",
                entry.path().display()
            ));
        }
    }
    Ok(())
}

fn copy_file(source: &Path, destination: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(source)
        .map_err(|error| format!("failed to inspect {}: {error}", source.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "Plugin mutation staging source must be a regular file: {}",
            source.display()
        ));
    }
    let parent = destination
        .parent()
        .ok_or_else(|| "staged file has no parent".to_owned())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    fs::copy(source, destination).map_err(|error| {
        format!(
            "failed to stage {} as {}: {error}",
            source.display(),
            destination.display()
        )
    })?;
    Ok(())
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PluginManagementResponse {
    configuration_authority: PluginConfigurationAuthorityResponse,
    plugins: Vec<ManagedPlugin>,
    revision: String,
    schema: &'static str,
    selection_authority: Option<PluginSelectionAuthorityResponse>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginSelectionAuthorityResponse {
    kind: String,
    reference: String,
}

impl From<PluginConfigurationAuthoritySource> for PluginSelectionAuthorityResponse {
    fn from(source: PluginConfigurationAuthoritySource) -> Self {
        Self {
            kind: source.kind().to_owned(),
            reference: source.reference().to_owned(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginCatalogResponse {
    installation: PluginCatalogInstallation,
    plugins: Vec<PluginCatalogItem>,
    query: String,
    schema: &'static str,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginCatalogInstallation {
    kind: &'static str,
    requires_absolute_path: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginCatalogItem {
    actions: Vec<&'static str>,
    instances: Vec<ManagedPluginInstance>,
    package_id: String,
    package_revision: String,
    source: &'static str,
    status: &'static str,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TrustedPluginCatalogResponse {
    authority: PluginConfigurationAuthorityResponse,
    entries: Vec<TrustedPluginCatalogEntry>,
    query: String,
    revision: String,
    schema: &'static str,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct TrustedPluginCatalogEntry {
    catalog_entry_id: String,
    package_id: String,
    package_revision: String,
    source_digest: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginInstallProposalResponse {
    authority: PluginConfigurationAuthorityResponse,
    base_revision: String,
    candidate_revision: String,
    catalog_entry_id: String,
    package_id: String,
    package_revision: String,
    proposal_digest: String,
    schema: &'static str,
    source_digest: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginInstallPublicationResponse {
    authority: PluginConfigurationAuthorityResponse,
    base_revision: String,
    catalog_entry_id: String,
    package_id: String,
    package_revision: String,
    proposal_digest: String,
    revision: String,
    schema: &'static str,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginRemovalProposalResponse {
    authority: PluginConfigurationAuthorityResponse,
    base_revision: String,
    candidate_revision: String,
    package_id: String,
    package_revision: String,
    proposal_digest: String,
    recoverable: bool,
    schema: &'static str,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginRemovalPublicationResponse {
    authority: PluginConfigurationAuthorityResponse,
    base_revision: String,
    package_id: String,
    package_revision: String,
    proposal_digest: String,
    recoverable: bool,
    revision: String,
    schema: &'static str,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ManagedPlugin {
    configuration_defaults: Value,
    configuration_schema: Option<Value>,
    instances: Vec<ManagedPluginInstance>,
    package_id: String,
    package_revision: String,
    root_supplied: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ManagedPluginInstance {
    disableable: bool,
    has_root_difference: bool,
    instance_key: String,
    origin: &'static str,
    root_configuration_toml: Option<String>,
    selection: &'static str,
    source_digest: String,
}

#[derive(Debug)]
struct ProfileManagementAuthority {
    app_root: PathBuf,
    disabled: BTreeSet<String>,
    enabled: BTreeSet<String>,
    host_defaults: BTreeMap<String, bool>,
    ids: BTreeSet<String>,
    releases: BTreeMap<String, ManagedPluginRelease>,
    root_instances: BTreeSet<String>,
    root_releases: BTreeSet<String>,
}

#[derive(Debug)]
struct ManagedPluginRelease {
    configuration_defaults: Value,
    configuration_schema: Option<Value>,
    release_version: String,
}

impl ProfileManagementAuthority {
    fn load(
        app_root: &Path,
        desired: &lenso_agent_host::DesiredPluginRootSnapshot,
    ) -> Result<Self, String> {
        let catalog_path = app_root.join(".lenso/host-catalog.json");
        let catalog: lenso_app_plan::authoring::HostCatalog = serde_json::from_slice(
            &fs::read(&catalog_path)
                .map_err(|error| format!("failed to read {}: {error}", catalog_path.display()))?,
        )
        .map_err(|error| format!("Host Catalog is invalid: {error}"))?;
        let enabled = desired
            .plan()
            .plugin_instances()
            .iter()
            .map(|instance| instance.instance_key().to_owned())
            .collect::<BTreeSet<_>>();
        let root_instances = desired
            .plugin_root()
            .instances()
            .iter()
            .map(|instance| instance.id().to_string())
            .collect::<BTreeSet<_>>();
        let disabled = desired
            .plugin_root()
            .disabled()
            .iter()
            .map(ToString::to_string)
            .collect::<BTreeSet<_>>();
        let host_defaults = catalog
            .defaults()
            .iter()
            .map(|instance| (instance.id().to_string(), instance.is_disableable()))
            .collect::<BTreeMap<_, _>>();
        let ids = root_instances
            .iter()
            .chain(disabled.iter())
            .chain(host_defaults.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        let root_releases = desired
            .plugin_root()
            .releases()
            .iter()
            .map(|release| release.plugin_id().to_owned())
            .collect::<BTreeSet<_>>();
        let mut releases = catalog
            .plugins()
            .iter()
            .map(|release| {
                let descriptor = release.descriptor();
                (
                    descriptor.plugin_id().to_owned(),
                    ManagedPluginRelease {
                        configuration_defaults: descriptor.configuration_defaults().clone(),
                        configuration_schema: descriptor.configuration_schema().cloned(),
                        release_version: descriptor.release_version().to_owned(),
                    },
                )
            })
            .chain(desired.plugin_root().releases().iter().map(|release| {
                (
                    release.plugin_id().to_owned(),
                    ManagedPluginRelease {
                        configuration_defaults: release.configuration_defaults().clone(),
                        configuration_schema: release.configuration_schema().cloned(),
                        release_version: release.release_version().to_owned(),
                    },
                )
            }))
            .collect::<BTreeMap<_, _>>();
        for id in &ids {
            let (plugin_id, _) = split_plugin_instance_id(id);
            releases
                .entry(plugin_id.to_owned())
                .or_insert_with(|| ManagedPluginRelease {
                    configuration_defaults: Value::Object(Map::default()),
                    configuration_schema: None,
                    release_version: String::new(),
                });
        }
        Ok(Self {
            app_root: app_root.to_path_buf(),
            disabled,
            enabled,
            host_defaults,
            ids,
            releases,
            root_instances,
            root_releases,
        })
    }

    fn into_response(
        self,
        revision: String,
        configuration_authority: PluginConfigurationAuthorityResponse,
    ) -> Result<PluginManagementResponse, String> {
        let mut instances = self
            .releases
            .keys()
            .map(|plugin_id| (plugin_id.clone(), Vec::new()))
            .collect::<BTreeMap<_, Vec<ManagedPluginInstance>>>();
        for id in &self.ids {
            let (plugin_id, instance) = self.project_instance(id)?;
            instances.entry(plugin_id).or_default().push(instance);
        }
        Ok(PluginManagementResponse {
            configuration_authority,
            plugins: self
                .releases
                .into_iter()
                .map(|(plugin_id, release)| {
                    let mut plugin_instances = instances.remove(&plugin_id).unwrap_or_default();
                    plugin_instances
                        .sort_by(|left, right| left.instance_key.cmp(&right.instance_key));
                    ManagedPlugin {
                        configuration_defaults: release.configuration_defaults,
                        configuration_schema: release.configuration_schema,
                        instances: plugin_instances,
                        package_id: plugin_id.clone(),
                        package_revision: release.release_version,
                        root_supplied: self.root_releases.contains(&plugin_id),
                    }
                })
                .collect(),
            revision,
            schema: "lenso.agent.plugin-management.v1",
            selection_authority: None,
        })
    }

    fn project_instance(&self, id: &str) -> Result<(String, ManagedPluginInstance), String> {
        let (plugin_id, instance_key) = split_plugin_instance_id(id);
        let root_configuration_toml = self
            .root_instances
            .contains(id)
            .then(|| {
                let path = self
                    .app_root
                    .join("plugins")
                    .join(plugin_id)
                    .join(format!("{instance_key}.toml"));
                fs::read_to_string(&path)
                    .map_err(|error| format!("failed to read {}: {error}", path.display()))
            })
            .transpose()?;
        let disabled_by_root = self.disabled.contains(id);
        let source_digest = PluginConfigurationSourceDigest::for_source(
            plugin_id,
            instance_key,
            root_configuration_toml.as_deref().map(str::as_bytes),
        )
        .map_err(|error| error.to_string())?
        .as_str()
        .to_owned();
        Ok((
            plugin_id.to_owned(),
            ManagedPluginInstance {
                disableable: self.host_defaults.get(id).copied().unwrap_or(true),
                has_root_difference: root_configuration_toml.is_some() || disabled_by_root,
                instance_key: instance_key.to_owned(),
                origin: if self.host_defaults.contains_key(id) {
                    "host-default"
                } else {
                    "plugin-root"
                },
                root_configuration_toml,
                selection: if self.enabled.contains(id) {
                    "enabled"
                } else if disabled_by_root {
                    "disabled-by-root"
                } else {
                    "excluded-by-profile"
                },
                source_digest,
            },
        ))
    }
}

fn split_plugin_instance_id(id: &str) -> (&str, &str) {
    id.split_once('/')
        .expect("validated Plugin Instance IDs contain one separator")
}

impl From<&PluginRootAuthoringState> for PluginManagementResponse {
    fn from(state: &PluginRootAuthoringState) -> Self {
        Self {
            configuration_authority: PluginConfigurationAuthoritySource::new(
                "local_plugin_root",
                "app",
            )
            .expect("the built-in Plugin configuration authority identity is valid")
            .into(),
            plugins: state
                .plugins()
                .iter()
                .map(|plugin| ManagedPlugin {
                    configuration_defaults: plugin.configuration_defaults().clone(),
                    configuration_schema: plugin.configuration_schema().cloned(),
                    instances: plugin
                        .instances()
                        .iter()
                        .map(|instance| ManagedPluginInstance {
                            disableable: instance.is_disableable(),
                            has_root_difference: instance.has_root_difference(),
                            instance_key: instance.id().instance_key().to_owned(),
                            origin: if instance.is_host_default() {
                                "host-default"
                            } else {
                                "plugin-root"
                            },
                            root_configuration_toml: instance
                                .root_configuration_toml()
                                .map(str::to_owned),
                            selection: if instance.is_enabled() {
                                "enabled"
                            } else {
                                "disabled-by-root"
                            },
                            source_digest: instance.source_digest().as_str().to_owned(),
                        })
                        .collect(),
                    package_id: plugin.plugin_id().to_owned(),
                    package_revision: plugin.release_version().to_owned(),
                    root_supplied: plugin.is_root_supplied(),
                })
                .collect(),
            revision: state.revision().as_str().to_owned(),
            schema: "lenso.agent.plugin-management.v1",
            selection_authority: None,
        }
    }
}

impl PluginManagementResponse {
    fn with_configuration_authority(
        mut self,
        authority: PluginConfigurationAuthorityResponse,
    ) -> Self {
        self.configuration_authority = authority;
        self
    }

    fn with_selection_authority(
        mut self,
        authority: Option<PluginSelectionAuthorityResponse>,
    ) -> Self {
        self.selection_authority = authority;
        self
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginConfigurationProposalResponse {
    application: &'static str,
    base_revision: String,
    base_source_digest: String,
    candidate_revision: String,
    configuration_authority: PluginConfigurationAuthorityResponse,
    diagnostics: Vec<PluginConfigurationDiagnosticResponse>,
    instance_key: String,
    plugin_id: String,
    proposal_digest: String,
    schema: String,
    status: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginConfigurationDiagnosticResponse {
    code: String,
    detail: String,
}

impl PluginConfigurationProposalResponse {
    fn new(
        proposal: &PluginConfigurationProposal,
        configuration_authority: PluginConfigurationAuthorityResponse,
    ) -> Self {
        Self {
            application: proposal_application(proposal.application()),
            base_revision: proposal.base_revision().as_str().to_owned(),
            base_source_digest: proposal.base_source_digest().as_str().to_owned(),
            candidate_revision: proposal.candidate_revision().as_str().to_owned(),
            configuration_authority,
            diagnostics: proposal
                .diagnostics()
                .iter()
                .map(PluginConfigurationDiagnosticResponse::from)
                .collect(),
            instance_key: proposal.instance_key().to_owned(),
            plugin_id: proposal.plugin_id().to_owned(),
            proposal_digest: proposal.digest().to_owned(),
            schema: proposal.schema().to_owned(),
            status: proposal_status(proposal.status()),
        }
    }

    fn ready_for_profile(
        proposal: &PluginConfigurationProposal,
        candidate: &ProfileConfigurationCandidate,
        configuration_authority: PluginConfigurationAuthorityResponse,
    ) -> Self {
        let mut response = Self::new(proposal, configuration_authority);
        response.application =
            if proposal.base_revision().as_str() == candidate.desired.plugin_root_revision() {
                "noop"
            } else {
                "app_generation"
            };
        candidate
            .desired
            .plugin_root_revision()
            .clone_into(&mut response.candidate_revision);
        response.diagnostics.clear();
        response
            .proposal_digest
            .clone_from(&candidate.proposal_digest);
        response.status = "ready";
        response
    }
}

impl From<&PluginConfigurationDiagnostic> for PluginConfigurationDiagnosticResponse {
    fn from(diagnostic: &PluginConfigurationDiagnostic) -> Self {
        Self {
            code: diagnostic.code().to_owned(),
            detail: diagnostic.detail().to_owned(),
        }
    }
}

fn proposal_status(status: PluginConfigurationProposalStatus) -> &'static str {
    match status {
        PluginConfigurationProposalStatus::Ready => "ready",
        PluginConfigurationProposalStatus::NeedsDecision => "needs_decision",
        PluginConfigurationProposalStatus::Rejected => "rejected",
    }
}

fn proposal_application(application: PluginConfigurationApplication) -> &'static str {
    match application {
        PluginConfigurationApplication::Noop => "noop",
        PluginConfigurationApplication::AppGeneration => "app_generation",
        PluginConfigurationApplication::Blocked => "blocked",
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginConfigurationHistoryResponse {
    configuration_authority: PluginConfigurationAuthorityResponse,
    instance_key: String,
    plugin_id: String,
    publications: Vec<PluginConfigurationPublicationRecordResponse>,
    schema: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginConfigurationPublicationRecordResponse {
    base_revision: String,
    base_source_digest: Option<String>,
    configuration_toml: String,
    proposal_digest: String,
    published_at_unix_ms: i64,
    revision: String,
    rollback_of_proposal_digest: Option<String>,
}

impl From<PluginConfigurationPublicationRecord> for PluginConfigurationPublicationRecordResponse {
    fn from(record: PluginConfigurationPublicationRecord) -> Self {
        Self {
            base_revision: record.base_revision,
            base_source_digest: record.base_source_digest,
            configuration_toml: record.configuration_toml,
            proposal_digest: record.proposal_digest,
            published_at_unix_ms: record.published_at_unix_ms,
            revision: record.revision,
            rollback_of_proposal_digest: record.rollback_of_proposal_digest,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginConfigurationRollbackProposalResponse {
    configuration_toml: String,
    proposal: PluginConfigurationProposalResponse,
    rollback_of_proposal_digest: String,
    schema: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginConfigurationPublicationResponse {
    base_revision: String,
    base_source_digest: String,
    configuration_authority: PluginConfigurationAuthorityResponse,
    desired: PublishedDesiredPluginSelection,
    operation: PluginOperation,
    publication_schema: String,
    publication_status: &'static str,
    proposal_digest: String,
    revision: String,
    schema: &'static str,
    stream_id: String,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum PluginConfigurationMutationResponse {
    Published(Box<PluginConfigurationPublicationResponse>),
    Rejected(Box<PluginMutationResponse>),
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PublishedDesiredPluginSelection {
    configuration_status: &'static str,
    desired_revision: String,
    #[serde(flatten)]
    selection: DesiredPluginSelection,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PluginInventoryQuery {
    after: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct InstallPluginRequest {
    bundle_path: PathBuf,
    expected_stream_id: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginCatalogQuery {
    #[serde(default)]
    query: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProposePluginInstallRequest {
    catalog_entry_id: String,
    expected_revision: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PublishPluginInstallRequest {
    catalog_entry_id: String,
    expected_revision: String,
    proposal_digest: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProposePluginRemovalRequest {
    expected_revision: String,
    plugin_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PublishPluginRemovalRequest {
    expected_revision: String,
    plugin_id: String,
    proposal_digest: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProposePluginConfigurationRequest {
    expected_revision: String,
    expected_source_digest: String,
    expected_stream_id: String,
    toml: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PublishPluginConfigurationRequest {
    expected_revision: String,
    expected_source_digest: String,
    expected_stream_id: String,
    proposal_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rollback_of_proposal_digest: Option<String>,
    toml: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProposePluginConfigurationRollbackRequest {
    expected_revision: String,
    expected_source_digest: String,
    expected_stream_id: String,
    publication_proposal_digest: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SetPluginEnabledRequest {
    enabled: bool,
    expected_revision: String,
    expected_stream_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ExpectedPluginSelectionRequest {
    expected_revision: String,
    expected_stream_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ExpectedPluginStreamRequest {
    expected_stream_id: String,
}

pub(super) fn routes() -> Router<WebRuntime> {
    Router::new()
        .route("/api/console/v1/agent/plugins", get(plugin_inventory))
        .route(
            "/api/console/v1/agent/control/plugins",
            get(plugin_management),
        )
        .route(
            "/api/console/v1/agent/control/plugins/catalog",
            get(plugin_catalog),
        )
        .route(
            "/api/console/v1/agent/control/plugins/trusted-catalog",
            get(trusted_plugin_catalog),
        )
        .route(
            "/api/console/v1/agent/control/plugin-installations/proposals",
            post(propose_plugin_installation),
        )
        .route(
            "/api/console/v1/agent/control/plugin-installations/publications",
            post(publish_plugin_installation),
        )
        .route(
            "/api/console/v1/agent/control/plugin-removals/proposals",
            post(propose_plugin_removal),
        )
        .route(
            "/api/console/v1/agent/control/plugin-removals/publications",
            post(publish_plugin_removal),
        )
        .route(
            "/api/console/v1/agent/control/plugin-operations/{operation_id}",
            get(plugin_operation),
        )
        .route(
            "/api/console/v1/agent/control/plugins/install",
            post(install_plugin),
        )
        .route(
            "/api/console/v1/agent/control/plugins/{plugin_id}",
            delete(remove_controlled_plugin),
        )
        .route(
            "/api/console/v1/agent/control/plugins/{plugin_id}/{instance}",
            delete(remove_plugin_instance),
        )
        .route(
            "/api/console/v1/agent/control/plugins/{plugin_id}/{instance}/configuration",
            put(publish_plugin_instance_configuration).layer(
                DefaultBodyLimit::max(MAX_PLUGIN_CONFIGURATION_REQUEST_BYTES),
            ),
        )
        .route(
            "/api/console/v1/agent/control/plugins/{plugin_id}/{instance}/configuration/proposals",
            post(propose_plugin_instance_configuration).layer(
                DefaultBodyLimit::max(MAX_PLUGIN_CONFIGURATION_REQUEST_BYTES),
            ),
        )
        .route(
            "/api/console/v1/agent/control/plugins/{plugin_id}/{instance}/configuration/publications",
            get(plugin_instance_configuration_publications),
        )
        .route(
            "/api/console/v1/agent/control/plugins/{plugin_id}/{instance}/configuration/rollback-proposals",
            post(propose_plugin_instance_configuration_rollback),
        )
        .route(
            "/api/console/v1/agent/control/plugins/{plugin_id}/{instance}/enabled",
            put(set_plugin_instance_enabled),
        )
        .route(
            "/api/console/v1/agent/control/plugins/{plugin_id}/{instance}/disable",
            post(disable_plugin_instance),
        )
        .route(
            "/api/console/v1/agent/control/plugins/{plugin_id}/{instance}/enable",
            post(enable_plugin_instance),
        )
}

pub(super) async fn plugin_inventory(
    State(runtime): State<WebRuntime>,
    Query(query): Query<PluginInventoryQuery>,
) -> Result<Json<PluginInventoryResponse>, ApiProblem> {
    let after = query
        .after
        .as_deref()
        .map(parse_plugin_cursor)
        .transpose()?;
    let (reply, response) = oneshot::channel();
    runtime
        .commands
        .send(RuntimeCommand::Plugin(PluginRuntimeCommand::Inventory {
            after,
            reply,
        }))
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime already stopped"))?;
    response
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime stopped without an inventory"))?
        .map(Json)
        .map_err(ApiProblem::unavailable)
}

async fn plugin_operation(
    State(runtime): State<WebRuntime>,
    headers: HeaderMap,
    AxumPath(operation_id): AxumPath<String>,
) -> Result<Json<PluginOperationResponse>, ApiProblem> {
    runtime.authorize_control(&headers)?;
    if uuid::Uuid::parse_str(&operation_id).is_err() {
        return Err(ApiProblem::bad_request("Plugin operation ID is invalid"));
    }
    let (reply, response) = oneshot::channel();
    runtime
        .commands
        .send(RuntimeCommand::Plugin(PluginRuntimeCommand::Operation {
            operation_id,
            reply,
        }))
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime already stopped"))?;
    response
        .await
        .map_err(|_| ApiProblem::unavailable("Agent runtime stopped without an operation"))?
        .map(Json)
        .ok_or_else(|| ApiProblem::not_found("Plugin operation is unknown or expired"))
}

fn parse_plugin_cursor(value: &str) -> Result<u64, ApiProblem> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ApiProblem::bad_request(
            "Plugin event cursor must be an unsigned decimal integer",
        ));
    }
    value
        .parse()
        .map_err(|_| ApiProblem::bad_request("Plugin event cursor is out of range"))
}

async fn plugin_management(
    State(runtime): State<WebRuntime>,
    headers: HeaderMap,
) -> Result<Response, ApiProblem> {
    runtime.authorize_control(&headers)?;
    let control = runtime.plugin_control()?;
    let management = tokio::task::spawn_blocking(move || control.inspect())
        .await
        .map_err(|error| {
            ApiProblem::unavailable(format!("Plugin inspection task failed: {error}"))
        })?
        .map_err(ApiProblem::conflict)?;
    let body = serde_json::to_vec(&management).map_err(|error| {
        ApiProblem::unavailable(format!("Plugin management serialization failed: {error}"))
    })?;
    let etag = format!("\"{}\"", lenso_plugin_control_plane::sha256_digest(&body));
    let etag = HeaderValue::from_str(&etag).map_err(|error| {
        ApiProblem::unavailable(format!("Plugin management ETag failed: {error}"))
    })?;
    if headers
        .get(header::IF_NONE_MATCH)
        .is_some_and(|candidate| etag_matches(candidate, &etag))
    {
        let mut response = StatusCode::NOT_MODIFIED.into_response();
        response.headers_mut().insert(header::ETAG, etag);
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-cache"),
        );
        return Ok(response);
    }
    let mut response = body.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response.headers_mut().insert(header::ETAG, etag);
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-cache"),
    );
    Ok(response)
}

async fn plugin_catalog(
    State(runtime): State<WebRuntime>,
    headers: HeaderMap,
    Query(request): Query<PluginCatalogQuery>,
) -> Result<Json<PluginCatalogResponse>, ApiProblem> {
    runtime.authorize_control(&headers)?;
    let query = request.query.trim();
    if query.len() > 128 {
        return Err(ApiProblem::bad_request(
            "Plugin catalog query exceeds 128 bytes",
        ));
    }
    runtime
        .plugin_control()?
        .catalog(query)
        .map(Json)
        .map_err(ApiProblem::unavailable)
}

async fn trusted_plugin_catalog(
    State(runtime): State<WebRuntime>,
    headers: HeaderMap,
    Query(request): Query<PluginCatalogQuery>,
) -> Result<Json<TrustedPluginCatalogResponse>, ApiProblem> {
    runtime.authorize_control(&headers)?;
    let query = request.query.trim();
    if query.len() > 128 {
        return Err(ApiProblem::bad_request(
            "Plugin catalog query exceeds 128 bytes",
        ));
    }
    runtime
        .plugin_control()?
        .trusted_catalog(query)
        .map(Json)
        .map_err(ApiProblem::unavailable)
}

async fn propose_plugin_installation(
    State(runtime): State<WebRuntime>,
    headers: HeaderMap,
    Json(request): Json<ProposePluginInstallRequest>,
) -> Result<Json<PluginInstallProposalResponse>, ApiProblem> {
    runtime.authorize_control(&headers)?;
    let expected = request
        .expected_revision
        .parse::<PluginRootRevision>()
        .map_err(|error| ApiProblem::bad_request(error.to_string()))?;
    let control = runtime.plugin_control()?;
    tokio::task::spawn_blocking(move || {
        control.propose_installation(&request.catalog_entry_id, &expected)
    })
    .await
    .map_err(|error| {
        ApiProblem::unavailable(format!("Plugin installation proposal task failed: {error}"))
    })?
    .map(Json)
    .map_err(ApiProblem::conflict)
}

async fn publish_plugin_installation(
    State(runtime): State<WebRuntime>,
    headers: HeaderMap,
    Json(request): Json<PublishPluginInstallRequest>,
) -> Result<Json<PluginInstallPublicationResponse>, ApiProblem> {
    runtime.authorize_control(&headers)?;
    let expected = request
        .expected_revision
        .parse::<PluginRootRevision>()
        .map_err(|error| ApiProblem::bad_request(error.to_string()))?;
    let control = runtime.plugin_control()?;
    tokio::task::spawn_blocking(move || {
        control.publish_installation(
            &request.catalog_entry_id,
            &expected,
            &request.proposal_digest,
        )
    })
    .await
    .map_err(|error| {
        ApiProblem::unavailable(format!(
            "Plugin installation publication task failed: {error}"
        ))
    })?
    .map(Json)
    .map_err(ApiProblem::conflict)
}

async fn propose_plugin_removal(
    State(runtime): State<WebRuntime>,
    headers: HeaderMap,
    Json(request): Json<ProposePluginRemovalRequest>,
) -> Result<Json<PluginRemovalProposalResponse>, ApiProblem> {
    runtime.authorize_control(&headers)?;
    let expected = request
        .expected_revision
        .parse::<PluginRootRevision>()
        .map_err(|error| ApiProblem::bad_request(error.to_string()))?;
    let control = runtime.plugin_control()?;
    tokio::task::spawn_blocking(move || control.propose_removal(&request.plugin_id, &expected))
        .await
        .map_err(|error| {
            ApiProblem::unavailable(format!("Plugin removal proposal task failed: {error}"))
        })?
        .map(Json)
        .map_err(ApiProblem::conflict)
}

async fn publish_plugin_removal(
    State(runtime): State<WebRuntime>,
    headers: HeaderMap,
    Json(request): Json<PublishPluginRemovalRequest>,
) -> Result<Json<PluginRemovalPublicationResponse>, ApiProblem> {
    runtime.authorize_control(&headers)?;
    let expected = request
        .expected_revision
        .parse::<PluginRootRevision>()
        .map_err(|error| ApiProblem::bad_request(error.to_string()))?;
    let control = runtime.plugin_control()?;
    tokio::task::spawn_blocking(move || {
        control.publish_removal(&request.plugin_id, &expected, &request.proposal_digest)
    })
    .await
    .map_err(|error| {
        ApiProblem::unavailable(format!("Plugin removal publication task failed: {error}"))
    })?
    .map(Json)
    .map_err(ApiProblem::conflict)
}

fn etag_matches(candidate: &HeaderValue, current: &HeaderValue) -> bool {
    let Ok(candidate) = candidate.to_str() else {
        return false;
    };
    let current = current
        .to_str()
        .expect("generated Plugin management ETag is visible ASCII");
    candidate
        .split(',')
        .map(str::trim)
        .any(|candidate| candidate == "*" || candidate == current)
}

async fn install_plugin(
    State(runtime): State<WebRuntime>,
    headers: HeaderMap,
    Json(request): Json<InstallPluginRequest>,
) -> Result<(StatusCode, Json<PluginMutationResponse>), ApiProblem> {
    runtime.authorize_control(&headers)?;
    if !request.bundle_path.is_absolute() {
        return Err(ApiProblem::bad_request(
            "Plugin Bundle path must be absolute",
        ));
    }
    mutate_plugin_root(
        &runtime,
        request.expected_stream_id,
        "install",
        move |control| control.install(&request.bundle_path),
    )
    .await
}

async fn propose_plugin_instance_configuration(
    State(runtime): State<WebRuntime>,
    headers: HeaderMap,
    AxumPath((plugin_id, instance)): AxumPath<(String, String)>,
    Json(request): Json<ProposePluginConfigurationRequest>,
) -> Result<Json<PluginConfigurationProposalResponse>, ApiProblem> {
    runtime.authorize_control(&headers)?;
    let expected_revision = request
        .expected_revision
        .parse::<PluginRootRevision>()
        .map_err(|error| ApiProblem::bad_request(error.to_string()))?;
    runtime
        .validate_plugin_stream(&request.expected_stream_id)
        .await?;
    let control = runtime.plugin_control()?;
    tokio::task::spawn_blocking(move || {
        control.propose_configuration(
            &plugin_id,
            &instance,
            &expected_revision,
            &request.expected_source_digest,
            request.toml.as_bytes(),
        )
    })
    .await
    .map_err(|error| ApiProblem::unavailable(format!("Plugin proposal task failed: {error}")))?
    .map(Json)
    .map_err(ApiProblem::conflict)
}

async fn plugin_instance_configuration_publications(
    State(runtime): State<WebRuntime>,
    headers: HeaderMap,
    AxumPath((plugin_id, instance)): AxumPath<(String, String)>,
) -> Result<Json<PluginConfigurationHistoryResponse>, ApiProblem> {
    runtime.authorize_control(&headers)?;
    let control = runtime.plugin_control()?;
    tokio::task::spawn_blocking(move || control.configuration_publications(&plugin_id, &instance))
        .await
        .map_err(|error| {
            ApiProblem::unavailable(format!("Plugin publication history task failed: {error}"))
        })?
        .map(Json)
        .map_err(ApiProblem::conflict)
}

async fn propose_plugin_instance_configuration_rollback(
    State(runtime): State<WebRuntime>,
    headers: HeaderMap,
    AxumPath((plugin_id, instance)): AxumPath<(String, String)>,
    Json(request): Json<ProposePluginConfigurationRollbackRequest>,
) -> Result<Json<PluginConfigurationRollbackProposalResponse>, ApiProblem> {
    runtime.authorize_control(&headers)?;
    let expected_revision = request
        .expected_revision
        .parse::<PluginRootRevision>()
        .map_err(|error| ApiProblem::bad_request(error.to_string()))?;
    runtime
        .validate_plugin_stream(&request.expected_stream_id)
        .await?;
    let control = runtime.plugin_control()?;
    let proposal = tokio::task::spawn_blocking(move || {
        control.propose_configuration_rollback(
            &plugin_id,
            &instance,
            &expected_revision,
            &request.expected_source_digest,
            &request.publication_proposal_digest,
        )
    })
    .await
    .map_err(|error| {
        ApiProblem::unavailable(format!("Plugin rollback proposal task failed: {error}"))
    })?
    .map_err(ApiProblem::conflict)?
    .ok_or_else(|| ApiProblem::not_found("Plugin configuration publication was not found"))?;
    Ok(Json(proposal))
}

async fn publish_plugin_instance_configuration(
    State(runtime): State<WebRuntime>,
    headers: HeaderMap,
    AxumPath((plugin_id, instance)): AxumPath<(String, String)>,
    Json(request): Json<PublishPluginConfigurationRequest>,
) -> Result<(StatusCode, Json<PluginConfigurationMutationResponse>), ApiProblem> {
    runtime.authorize_control(&headers)?;
    let expected_revision = request
        .expected_revision
        .parse::<PluginRootRevision>()
        .map_err(|error| ApiProblem::bad_request(error.to_string()))?;
    runtime
        .plugin_mutations
        .run(async {
            runtime
                .validate_plugin_stream(&request.expected_stream_id)
                .await?;
            let publication_observe_fence = runtime.plugin_observation_fence().await?;
            let control = runtime.plugin_control()?;
            let observe_after_cursor = if control.configuration_publication_has_authority_gap() {
                Some(publication_observe_fence.cursor)
            } else {
                None
            };
            let published = tokio::task::spawn_blocking(move || {
                control.publish_configuration(
                    &plugin_id,
                    &instance,
                    ReviewedPluginConfiguration {
                        bytes: request.toml.as_bytes(),
                        expected_proposal_digest: &request.proposal_digest,
                        expected_revision: &expected_revision,
                        expected_source_digest: &request.expected_source_digest,
                        rollback_of_proposal_digest: request.rollback_of_proposal_digest.as_deref(),
                    },
                )
            })
            .await
            .map_err(|error| {
                ApiProblem::unavailable(format!("Plugin publication task failed: {error}"))
            })?;
            let published = match published {
                Ok(published) => published,
                Err(detail) => return rejected_configuration_mutation(&runtime, detail).await,
            };
            let PublishedPluginConfiguration {
                base_revision,
                base_source_digest,
                configuration_authority,
                desired: committed_desired,
                proposal_digest,
                revision,
                schema: publication_schema,
                linearization,
            } = published;
            if committed_desired.plugin_root_revision() != revision {
                return rejected_configuration_mutation(
                    &runtime,
                    "published Plugin Root revision does not match its operation receipt"
                        .to_owned(),
                )
                .await;
            }
            let accepted_fence = runtime.plugin_observation_fence().await?;
            let receipt = runtime
                .register_plugin_mutation(
                    accepted_fence.cursor,
                    observe_after_cursor.unwrap_or(accepted_fence.cursor),
                    accepted_fence.desired_epoch,
                    Ok(committed_desired),
                )
                .await?;
            drop(linearization);
            let stream_id = receipt.stream_id.clone();
            let desired = receipt.desired.ok_or_else(|| {
                ApiProblem::unavailable("published Plugin configuration has no Desired selection")
            })?;
            let configuration_status = receipt.operation.configuration_status();
            Ok((
                StatusCode::ACCEPTED,
                Json(PluginConfigurationMutationResponse::Published(Box::new(
                    PluginConfigurationPublicationResponse {
                        base_revision,
                        base_source_digest,
                        configuration_authority,
                        desired: PublishedDesiredPluginSelection {
                            configuration_status,
                            desired_revision: revision.clone(),
                            selection: desired,
                        },
                        operation: receipt.operation,
                        publication_schema,
                        publication_status: "published",
                        proposal_digest,
                        revision,
                        schema: "lenso.agent.plugin-operation.v1",
                        stream_id,
                    },
                ))),
            ))
        })
        .await
}

async fn rejected_configuration_mutation(
    runtime: &WebRuntime,
    detail: String,
) -> Result<(StatusCode, Json<PluginConfigurationMutationResponse>), ApiProblem> {
    let accepted_fence = runtime.plugin_observation_fence().await?;
    let receipt = runtime
        .register_plugin_mutation(
            accepted_fence.cursor,
            accepted_fence.cursor,
            accepted_fence.desired_epoch,
            Err(detail),
        )
        .await?;
    Ok((
        StatusCode::CONFLICT,
        Json(PluginConfigurationMutationResponse::Rejected(Box::new(
            receipt,
        ))),
    ))
}

async fn set_plugin_instance_enabled(
    State(runtime): State<WebRuntime>,
    headers: HeaderMap,
    AxumPath((plugin_id, instance)): AxumPath<(String, String)>,
    Json(request): Json<SetPluginEnabledRequest>,
) -> Result<(StatusCode, Json<PluginMutationResponse>), ApiProblem> {
    runtime.authorize_control(&headers)?;
    let expected_revision = PluginRootRevision::from_str(&request.expected_revision)
        .map_err(|error| ApiProblem::bad_request(error.to_string()))?;
    mutate_plugin_root(
        &runtime,
        request.expected_stream_id,
        "selection",
        move |control| {
            control.set_enabled(&expected_revision, &plugin_id, &instance, request.enabled)
        },
    )
    .await
}

async fn disable_plugin_instance(
    State(runtime): State<WebRuntime>,
    headers: HeaderMap,
    path: AxumPath<(String, String)>,
    Json(request): Json<ExpectedPluginSelectionRequest>,
) -> Result<(StatusCode, Json<PluginMutationResponse>), ApiProblem> {
    set_plugin_enabled_alias(
        runtime,
        headers,
        path,
        request.expected_revision,
        request.expected_stream_id,
        false,
    )
    .await
}

async fn enable_plugin_instance(
    State(runtime): State<WebRuntime>,
    headers: HeaderMap,
    path: AxumPath<(String, String)>,
    Json(request): Json<ExpectedPluginSelectionRequest>,
) -> Result<(StatusCode, Json<PluginMutationResponse>), ApiProblem> {
    set_plugin_enabled_alias(
        runtime,
        headers,
        path,
        request.expected_revision,
        request.expected_stream_id,
        true,
    )
    .await
}

async fn set_plugin_enabled_alias(
    runtime: WebRuntime,
    headers: HeaderMap,
    AxumPath((plugin_id, instance)): AxumPath<(String, String)>,
    expected_revision: String,
    expected_stream_id: String,
    enabled: bool,
) -> Result<(StatusCode, Json<PluginMutationResponse>), ApiProblem> {
    runtime.authorize_control(&headers)?;
    let expected_revision = PluginRootRevision::from_str(&expected_revision)
        .map_err(|error| ApiProblem::bad_request(error.to_string()))?;
    mutate_plugin_root(&runtime, expected_stream_id, "selection", move |control| {
        control.set_enabled(&expected_revision, &plugin_id, &instance, enabled)
    })
    .await
}

async fn remove_plugin_instance(
    State(runtime): State<WebRuntime>,
    headers: HeaderMap,
    AxumPath((plugin_id, instance)): AxumPath<(String, String)>,
    Json(request): Json<ExpectedPluginStreamRequest>,
) -> Result<(StatusCode, Json<PluginMutationResponse>), ApiProblem> {
    runtime.authorize_control(&headers)?;
    mutate_plugin_root(
        &runtime,
        request.expected_stream_id,
        "removal",
        move |control| control.remove_instance(&plugin_id, &instance),
    )
    .await
}

async fn remove_controlled_plugin(
    State(runtime): State<WebRuntime>,
    headers: HeaderMap,
    AxumPath(plugin_id): AxumPath<String>,
    Json(request): Json<ExpectedPluginStreamRequest>,
) -> Result<(StatusCode, Json<PluginMutationResponse>), ApiProblem> {
    runtime.authorize_control(&headers)?;
    mutate_plugin_root(
        &runtime,
        request.expected_stream_id,
        "removal",
        move |control| control.remove(&plugin_id),
    )
    .await
}

async fn mutate_plugin_root(
    runtime: &WebRuntime,
    expected_stream_id: String,
    operation_name: &'static str,
    operation: impl FnOnce(PluginControl) -> Result<CommittedPluginMutation, String> + Send + 'static,
) -> Result<(StatusCode, Json<PluginMutationResponse>), ApiProblem> {
    runtime
        .plugin_mutations
        .run(async {
            runtime.validate_plugin_stream(&expected_stream_id).await?;
            let control = runtime.plugin_control()?;
            let committed = tokio::task::spawn_blocking(move || operation(control))
                .await
                .map_err(|error| {
                    ApiProblem::unavailable(format!("Plugin {operation_name} task failed: {error}"))
                })?;
            let (desired, linearization) = match committed {
                Ok(committed) => (Ok(committed.desired), Some(committed.linearization)),
                Err(detail) => (Err(detail), None),
            };
            let accepted_fence = runtime.plugin_observation_fence().await?;
            let receipt = runtime
                .register_plugin_mutation(
                    accepted_fence.cursor,
                    accepted_fence.cursor,
                    accepted_fence.desired_epoch,
                    desired,
                )
                .await?;
            drop(linearization);
            let status = if receipt.operation.is_rejected() {
                StatusCode::CONFLICT
            } else {
                StatusCode::ACCEPTED
            };
            Ok((status, Json(receipt)))
        })
        .await
}

impl WebRuntime {
    fn plugin_control(&self) -> Result<PluginControl, ApiProblem> {
        self.plugin_control
            .clone()
            .ok_or_else(|| ApiProblem::not_found("Agent Plugin Root control is not configured"))
    }

    async fn plugin_observation_fence(&self) -> Result<PluginObservationFence, ApiProblem> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(RuntimeCommand::Plugin(
                PluginRuntimeCommand::ObservationFence { reply },
            ))
            .await
            .map_err(|_| ApiProblem::unavailable("Agent runtime already stopped"))?;
        response.await.map_err(|_| {
            ApiProblem::unavailable("Agent runtime stopped without an observation fence")
        })
    }

    async fn validate_plugin_stream(&self, expected_stream_id: &str) -> Result<(), ApiProblem> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(RuntimeCommand::Plugin(
                PluginRuntimeCommand::ValidateStream {
                    expected_stream_id: expected_stream_id.to_owned(),
                    reply,
                },
            ))
            .await
            .map_err(|_| ApiProblem::unavailable("Agent runtime already stopped"))?;
        response
            .await
            .map_err(|_| {
                ApiProblem::unavailable("Agent runtime stopped without stream validation")
            })?
            .map_err(ApiProblem::conflict)
    }

    async fn register_plugin_mutation(
        &self,
        accepted_after_cursor: u64,
        observe_after_cursor: u64,
        accepted_after_desired_epoch: u64,
        desired: Result<lenso_agent_host::DesiredPluginRootSnapshot, String>,
    ) -> Result<PluginMutationResponse, ApiProblem> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(RuntimeCommand::Plugin(
                PluginRuntimeCommand::RegisterMutation {
                    accepted_after_cursor,
                    observe_after_cursor,
                    accepted_after_desired_epoch,
                    desired: Box::new(desired),
                    reply,
                },
            ))
            .await
            .map_err(|_| ApiProblem::unavailable("Agent runtime already stopped"))?;
        response
            .await
            .map_err(|_| ApiProblem::unavailable("Agent runtime stopped without a receipt"))?
            .map_err(ApiProblem::conflict)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        process::{Command, Stdio},
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
            mpsc,
        },
        time::{Duration, Instant},
    };

    use super::*;
    use crate::plugin_control_api::fixture_desired_selection;
    use lenso_app_authoring::LocalPluginRootAuthority;

    #[test]
    #[allow(clippy::too_many_lines)] // One golden keeps the whole HTTP contract visually adjacent.
    fn serialized_management_and_publication_contract_matches_the_consumer_fixture() {
        let authority = PluginConfigurationAuthorityResponse {
            kind: "sqlite_configuration_store".to_owned(),
            publication_history: true,
            reference: "agent".to_owned(),
            rollback_proposals: true,
        };
        let management = PluginManagementResponse {
            configuration_authority: authority.clone(),
            plugins: vec![ManagedPlugin {
                configuration_defaults: serde_json::json!({ "enabled": false }),
                configuration_schema: Some(serde_json::json!({
                    "additionalProperties": false,
                    "properties": {
                        "enabled": { "type": "boolean" }
                    },
                    "type": "object"
                })),
                instances: vec![ManagedPluginInstance {
                    disableable: true,
                    has_root_difference: true,
                    instance_key: "default".to_owned(),
                    origin: "host-default",
                    root_configuration_toml: Some("enabled = true\n".to_owned()),
                    selection: "enabled",
                    source_digest:
                        "sha256:7505392d43ab29321a37810d9f6bd18c5d45a44c9b4dfe7c99a10acacd1dbe72"
                            .to_owned(),
                }],
                package_id: "example.echo".to_owned(),
                package_revision: "1.0.0".to_owned(),
                root_supplied: false,
            }],
            revision: "sha256:root-next".to_owned(),
            schema: "lenso.agent.plugin-management.v1",
            selection_authority: Some(PluginSelectionAuthorityResponse {
                kind: "sqlite_configuration_store".to_owned(),
                reference: "agent".to_owned(),
            }),
        };
        let proposal = PluginConfigurationProposalResponse {
            application: "app_generation",
            base_revision: "sha256:root-active".to_owned(),
            base_source_digest:
                "sha256:6250680ceebbc342f2610f53dc5e9d7c6f0c3637b6ad1202d207733d0249dfc3".to_owned(),
            candidate_revision: "sha256:root-next".to_owned(),
            configuration_authority: authority.clone(),
            diagnostics: Vec::new(),
            instance_key: "default".to_owned(),
            plugin_id: "example.echo".to_owned(),
            proposal_digest: "sha256:proposal-next".to_owned(),
            schema: "lenso.plugin-configuration-proposal.v1".to_owned(),
            status: "ready",
        };
        let switched = PluginOperation::fixture_switched(
            "018f0f5f-8b8a-7c3e-9b34-7f7f8d3f6b23",
            9_007_199_254_740_994,
        );
        let applied_status = switched.configuration_status();
        let publication_applied = PluginConfigurationPublicationResponse {
            base_revision: "sha256:root-active".to_owned(),
            base_source_digest:
                "sha256:6250680ceebbc342f2610f53dc5e9d7c6f0c3637b6ad1202d207733d0249dfc3".to_owned(),
            configuration_authority: authority.clone(),
            desired: PublishedDesiredPluginSelection {
                configuration_status: applied_status,
                desired_revision: "sha256:root-next".to_owned(),
                selection: fixture_desired_selection(),
            },
            operation: switched,
            publication_schema: "lenso.plugin-configuration-publication.v1".to_owned(),
            publication_status: "published",
            proposal_digest: "sha256:proposal-next".to_owned(),
            revision: "sha256:root-next".to_owned(),
            schema: "lenso.agent.plugin-operation.v1",
            stream_id: "018f0f5f-8b8a-7c3e-9b34-7f7f8d3f6b20".to_owned(),
        };
        let pending = PluginOperation::fixture_accepted(
            "018f0f5f-8b8a-7c3e-9b34-7f7f8d3f6b24",
            9_007_199_254_740_995,
        );
        let pending_status = pending.configuration_status();
        let publication_pending = PluginConfigurationPublicationResponse {
            base_revision: "sha256:root-active".to_owned(),
            base_source_digest:
                "sha256:6250680ceebbc342f2610f53dc5e9d7c6f0c3637b6ad1202d207733d0249dfc3".to_owned(),
            configuration_authority: authority,
            desired: PublishedDesiredPluginSelection {
                configuration_status: pending_status,
                desired_revision: "sha256:root-next".to_owned(),
                selection: fixture_desired_selection(),
            },
            operation: pending,
            publication_schema: "lenso.plugin-configuration-publication.v1".to_owned(),
            publication_status: "published",
            proposal_digest: "sha256:proposal-next".to_owned(),
            revision: "sha256:root-next".to_owned(),
            schema: "lenso.agent.plugin-operation.v1",
            stream_id: "018f0f5f-8b8a-7c3e-9b34-7f7f8d3f6b20".to_owned(),
        };
        let proposal_request = ProposePluginConfigurationRequest {
            expected_revision: "sha256:root-active".to_owned(),
            expected_source_digest:
                "sha256:6250680ceebbc342f2610f53dc5e9d7c6f0c3637b6ad1202d207733d0249dfc3".to_owned(),
            expected_stream_id: "018f0f5f-8b8a-7c3e-9b34-7f7f8d3f6b20".to_owned(),
            toml: "enabled = false\n".to_owned(),
        };
        let publication_request = PublishPluginConfigurationRequest {
            expected_revision: "sha256:root-active".to_owned(),
            expected_source_digest:
                "sha256:6250680ceebbc342f2610f53dc5e9d7c6f0c3637b6ad1202d207733d0249dfc3".to_owned(),
            expected_stream_id: "018f0f5f-8b8a-7c3e-9b34-7f7f8d3f6b20".to_owned(),
            proposal_digest: "sha256:proposal-next".to_owned(),
            rollback_of_proposal_digest: None,
            toml: "enabled = false\n".to_owned(),
        };
        let rollback_proposal_request = ProposePluginConfigurationRollbackRequest {
            expected_revision: "sha256:root-next".to_owned(),
            expected_source_digest: "sha256:source-next".to_owned(),
            expected_stream_id: "018f0f5f-8b8a-7c3e-9b34-7f7f8d3f6b20".to_owned(),
            publication_proposal_digest: "sha256:proposal-active".to_owned(),
        };
        let rollback_publication_request = PublishPluginConfigurationRequest {
            expected_revision: "sha256:root-next".to_owned(),
            expected_source_digest: "sha256:source-next".to_owned(),
            expected_stream_id: "018f0f5f-8b8a-7c3e-9b34-7f7f8d3f6b20".to_owned(),
            proposal_digest: "sha256:proposal-rollback".to_owned(),
            rollback_of_proposal_digest: Some("sha256:proposal-active".to_owned()),
            toml: "enabled = true\n".to_owned(),
        };
        let actual = serde_json::json!({
            "management": management,
            "proposal": proposal,
            "proposalRequest": proposal_request,
            "publicationApplied": publication_applied,
            "publicationPending": publication_pending,
            "publicationRequest": publication_request,
            "rollbackPublicationRequest": rollback_publication_request,
            "rollbackProposalRequest": rollback_proposal_request,
        });
        let expected: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/plugin-control-contract.json"
        ))
        .unwrap();

        for key in [
            "management",
            "proposal",
            "proposalRequest",
            "publicationApplied",
            "publicationPending",
            "publicationRequest",
            "rollbackPublicationRequest",
            "rollbackProposalRequest",
        ] {
            assert_eq!(actual[key], expected[key], "contract fixture key {key}");
        }
    }

    #[derive(Debug)]
    struct WrappedLocalConfigurationAuthority {
        local: LocalPluginRootAuthority,
        published: Arc<AtomicBool>,
    }

    #[derive(Debug)]
    struct PausingWrappedLocalConfigurationAuthority {
        local: LocalPluginRootAuthority,
        materialized: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }

    impl PluginConfigurationAuthority for PausingWrappedLocalConfigurationAuthority {
        fn source(&self) -> PluginConfigurationAuthoritySource {
            PluginConfigurationAuthoritySource::new("pausing_wrapped_local", "fixture").unwrap()
        }

        fn inspect(&self) -> anyhow::Result<PluginRootAuthoringState> {
            self.local.inspect()
        }

        fn propose(
            &self,
            expected_revision: &PluginRootRevision,
            plugin_id: &str,
            instance: &str,
            bytes: &[u8],
        ) -> anyhow::Result<PluginConfigurationProposal> {
            self.local
                .propose(expected_revision, plugin_id, instance, bytes)
        }

        fn publish(
            &self,
            proposal: &PluginConfigurationProposal,
        ) -> anyhow::Result<lenso_app_authoring::PluginConfigurationPublication> {
            let publication = self.local.publish(proposal)?;
            self.materialized
                .send(())
                .map_err(|_| anyhow::anyhow!("materialization observer stopped"))?;
            self.release
                .lock()
                .map_err(|_| anyhow::anyhow!("publication release lock is poisoned"))?
                .recv_timeout(Duration::from_secs(5))
                .map_err(|error| anyhow::anyhow!("publication was not released: {error}"))?;
            Ok(publication)
        }
    }

    impl PluginConfigurationAuthority for WrappedLocalConfigurationAuthority {
        fn source(&self) -> PluginConfigurationAuthoritySource {
            PluginConfigurationAuthoritySource::new("wrapped_local", "fixture").unwrap()
        }

        fn inspect(&self) -> anyhow::Result<PluginRootAuthoringState> {
            self.local.inspect()
        }

        fn propose(
            &self,
            expected_revision: &PluginRootRevision,
            plugin_id: &str,
            instance: &str,
            bytes: &[u8],
        ) -> anyhow::Result<PluginConfigurationProposal> {
            self.local
                .propose(expected_revision, plugin_id, instance, bytes)
        }

        fn publish(
            &self,
            proposal: &PluginConfigurationProposal,
        ) -> anyhow::Result<lenso_app_authoring::PluginConfigurationPublication> {
            self.published.store(true, Ordering::SeqCst);
            self.local.publish(proposal)
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn opaque_authority_that_wraps_local_publication_does_not_self_deadlock() {
        let root = tempfile::tempdir().unwrap();
        crate::configure_test_fixture_model(root.path());
        let published = Arc::new(AtomicBool::new(false));
        let authority = WrappedLocalConfigurationAuthority {
            local: LocalPluginRootAuthority::new(root.path()),
            published: Arc::clone(&published),
        };
        let mut config = crate::AgentWebConfig::new(lenso_agent_console_plugins::link);
        config.agent_home = Some(root.path().to_path_buf());
        config.control = crate::AgentWebControl::HostAuthorized;
        config.plugin_control = true;
        config.plugin_configuration_authority = Some(Arc::new(authority));
        let local = tokio::task::LocalSet::new();

        local
            .run_until(async {
                let surface = crate::AgentWebSurface::start(config).await.unwrap();
                let control = surface.runtime.plugin_control.clone().unwrap();
                assert!(control.configuration_publication_has_authority_gap());
                let revision = control.inspect().unwrap().revision.parse().unwrap();
                let toml = concat!(
                    "model = \"fixture/readme-summary-v1\"\n",
                    "allowed_models = [\"fixture/alternate-v1\", \"fixture/alternate-v2\"]\n",
                );
                let source_digest = current_configuration_source_digest(
                    root.path(),
                    "lenso.agent.model.fixture",
                    "model",
                )
                .unwrap()
                .to_string();
                let proposal = control
                    .propose_configuration(
                        "lenso.agent.model.fixture",
                        "model",
                        &revision,
                        &source_digest,
                        toml.as_bytes(),
                    )
                    .unwrap();
                let proposal_digest = proposal.proposal_digest;
                let task = tokio::task::spawn_blocking(move || {
                    control.publish_configuration(
                        "lenso.agent.model.fixture",
                        "model",
                        ReviewedPluginConfiguration {
                            bytes: toml.as_bytes(),
                            expected_proposal_digest: &proposal_digest,
                            expected_revision: &revision,
                            expected_source_digest: &source_digest,
                            rollback_of_proposal_digest: None,
                        },
                    )
                });

                let publication = tokio::time::timeout(Duration::from_secs(2), task)
                    .await
                    .expect("wrapped local publication self-deadlocked")
                    .unwrap()
                    .unwrap();

                assert_eq!(publication.configuration_authority.kind, "wrapped_local");
                assert!(published.load(Ordering::SeqCst));
                drop(publication);
                surface.shutdown().await.unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    #[allow(clippy::large_futures, clippy::too_many_lines)]
    async fn opaque_publication_receipt_replays_switch_before_handler_registration() {
        let root = tempfile::tempdir().unwrap();
        crate::configure_test_fixture_model(root.path());
        let (materialized, materialization) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let authority = PausingWrappedLocalConfigurationAuthority {
            local: LocalPluginRootAuthority::new(root.path()),
            materialized,
            release: Mutex::new(released),
        };
        let mut config = crate::AgentWebConfig::new(lenso_agent_console_plugins::link);
        config.agent_home = Some(root.path().to_path_buf());
        config.control = crate::AgentWebControl::HostAuthorized;
        config.plugin_control = true;
        config.plugin_configuration_authority = Some(Arc::new(authority));
        let local = tokio::task::LocalSet::new();

        local
            .run_until(async {
                let surface = crate::AgentWebSurface::start(config).await.unwrap();
                let runtime = surface.runtime.clone();
                let inventory = plugin_inventory(
                    State(runtime.clone()),
                    Query(PluginInventoryQuery::default()),
                )
                .await
                .unwrap()
                .0;
                let baseline_cursor = inventory.cursor.parse::<u64>().unwrap();
                let stream_id = inventory.stream_id;
                let control = runtime.plugin_control.clone().unwrap();
                let revision = control.inspect().unwrap().revision.parse().unwrap();
                let toml = concat!(
                    "model = \"fixture/readme-summary-v1\"\n",
                    "allowed_models = [\"fixture/alternate-v1\", \"fixture/alternate-v2\"]\n",
                );
                let source_digest = current_configuration_source_digest(
                    root.path(),
                    "lenso.agent.model.fixture",
                    "model",
                )
                .unwrap()
                .to_string();
                let proposal = control
                    .propose_configuration(
                        "lenso.agent.model.fixture",
                        "model",
                        &revision,
                        &source_digest,
                        toml.as_bytes(),
                    )
                    .unwrap();
                let request = PublishPluginConfigurationRequest {
                    expected_revision: revision.to_string(),
                    expected_source_digest: source_digest,
                    expected_stream_id: stream_id,
                    proposal_digest: proposal.proposal_digest,
                    rollback_of_proposal_digest: None,
                    toml: toml.to_owned(),
                };
                let handler_runtime = runtime.clone();
                let handler = tokio::task::spawn_local(async move {
                    publish_plugin_instance_configuration(
                        State(handler_runtime),
                        HeaderMap::new(),
                        AxumPath(("lenso.agent.model.fixture".to_owned(), "model".to_owned())),
                        Json(request),
                    )
                    .await
                });

                tokio::task::spawn_blocking(move || {
                    materialization
                        .recv_timeout(Duration::from_secs(5))
                        .expect("opaque authority did not materialize");
                })
                .await
                .unwrap();

                let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
                loop {
                    let current = plugin_inventory(
                        State(runtime.clone()),
                        Query(PluginInventoryQuery::default()),
                    )
                    .await
                    .unwrap()
                    .0;
                    let switched = current.cursor.parse::<u64>().unwrap() > baseline_cursor
                        && serde_json::to_value(&current).unwrap()["events"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|event| event["status"] == "switched");
                    if switched {
                        break;
                    }
                    assert!(
                        tokio::time::Instant::now() < deadline,
                        "Host did not switch before opaque publication returned"
                    );
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                release.send(()).unwrap();

                let (status, Json(response)) = handler.await.unwrap().unwrap();
                let response = serde_json::to_value(response).unwrap();
                assert_eq!(status, StatusCode::ACCEPTED);
                assert_eq!(response["operation"]["status"], "switched");
                assert_eq!(response["desired"]["configurationStatus"], "applied");
                surface.shutdown().await.unwrap();
            })
            .await;
    }

    fn add_host_global_resource_overflow(root: &Path) {
        let loop_plugin = root.join("plugins/lenso.agent.loop");
        fs::create_dir_all(loop_plugin.join("agent")).unwrap();
        fs::write(
            loop_plugin.join("agent.toml"),
            concat!(
                "model = \"fixture/readme-summary-v1\"\n",
                "max_steps = 8\n",
                "max_tool_calls = 4\n",
                "max_parallel_tool_calls = 4\n",
                "max_output_tokens = 1024\n",
                "max_history_events = 200\n",
                "max_compaction_summary_characters = 8192\n",
                "max_memory_items = 8\n",
                "max_memory_characters = 16384\n",
            ),
        )
        .unwrap();
        let prompt_plugin = root.join("plugins/lenso.agent.prompt");
        fs::create_dir_all(prompt_plugin.join("prompt")).unwrap();
        fs::write(
            prompt_plugin.join("prompt.toml"),
            "max_contributions = 256\nmax_total_bytes = 262144\n",
        )
        .unwrap();
        for resources in [loop_plugin.join("agent"), prompt_plugin.join("prompt")] {
            for index in 0..9 {
                fs::write(
                    resources.join(format!("resource-{index}.bin")),
                    vec![0_u8; 1024 * 1024],
                )
                .unwrap();
            }
        }
    }

    fn assert_global_budget_rejection_preserves_target(
        control: &PluginControl,
        root: &Path,
    ) -> String {
        let target = root.join("plugins/lenso.agent.loop/agent.toml");
        let before = fs::read(&target).ok();
        let revision = control.inspect().unwrap().revision.parse().unwrap();
        let toml = concat!(
            "model = \"fixture/readme-summary-v1\"\n",
            "max_steps = 9\n",
            "max_tool_calls = 4\n",
            "max_parallel_tool_calls = 4\n",
            "max_output_tokens = 1024\n",
            "max_history_events = 200\n",
            "max_compaction_summary_characters = 8192\n",
            "max_memory_items = 8\n",
            "max_memory_characters = 16384\n",
        );
        let source_digest = current_configuration_source_digest(root, "lenso.agent.loop", "agent")
            .unwrap()
            .to_string();
        let proposal = control
            .propose_configuration(
                "lenso.agent.loop",
                "agent",
                &revision,
                &source_digest,
                toml.as_bytes(),
            )
            .unwrap();
        assert_eq!(proposal.status, "ready");
        let error = control
            .publish_configuration(
                "lenso.agent.loop",
                "agent",
                ReviewedPluginConfiguration {
                    bytes: toml.as_bytes(),
                    expected_proposal_digest: &proposal.proposal_digest,
                    expected_revision: &revision,
                    expected_source_digest: &source_digest,
                    rollback_of_proposal_digest: None,
                },
            )
            .unwrap_err();

        assert!(error.contains("16 MiB in aggregate"), "{error}");
        assert_eq!(fs::read(&target).ok(), before);
        error
    }

    #[tokio::test(flavor = "current_thread")]
    async fn host_global_budget_rejection_precedes_local_publication() {
        let root = tempfile::tempdir().unwrap();
        crate::configure_test_fixture_model(root.path());
        let mut config = crate::AgentWebConfig::new(lenso_agent_console_plugins::link);
        config.agent_home = Some(root.path().to_path_buf());
        config.control = crate::AgentWebControl::HostAuthorized;
        config.plugin_control = true;
        let local = tokio::task::LocalSet::new();

        local
            .run_until(async {
                let surface = crate::AgentWebSurface::start(config).await.unwrap();
                let control = surface.runtime.plugin_control.clone().unwrap();
                add_host_global_resource_overflow(root.path());
                assert_global_budget_rejection_preserves_target(&control, root.path());
                surface.shutdown().await.unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn host_global_budget_rejection_precedes_sqlite_publication() {
        let root = tempfile::tempdir().unwrap();
        crate::configure_test_fixture_model(root.path());
        let database = root.path().join("configuration.sqlite3");
        let mut config = crate::AgentWebConfig::new(lenso_agent_console_plugins::link);
        config.agent_home = Some(root.path().to_path_buf());
        let local = tokio::task::LocalSet::new();

        local
            .run_until(async {
                let surface = crate::AgentWebSurface::start(config).await.unwrap();
                surface.shutdown().await.unwrap();
                add_host_global_resource_overflow(root.path());
                let authority = Arc::new(
                    crate::SqlitePluginConfigurationAuthority::open(
                        root.path(),
                        crate::PluginConfigurationStoreConfig::new(&database, "fixture"),
                    )
                    .unwrap(),
                );
                let control = PluginControl::new(
                    root.path(),
                    root.path(),
                    None,
                    BTreeMap::new(),
                    PluginControlAuthorities {
                        configuration: Arc::clone(&authority)
                            as Arc<dyn PluginConfigurationAuthority>,
                        configuration_is_builtin_local: false,
                        selection: Some(
                            Arc::clone(&authority) as Arc<dyn PluginSelectionAuthority>
                        ),
                        history: Some(authority as Arc<dyn PluginConfigurationHistoryAuthority>),
                    },
                );
                assert_global_budget_rejection_preserves_target(&control, root.path());
                let connection = rusqlite::Connection::open(&database).unwrap();
                let publications: i64 = connection
                    .query_row(
                        "SELECT COUNT(*) FROM configuration_publications",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(publications, 0);
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn host_global_budget_rejection_never_calls_an_opaque_authority_publish() {
        let root = tempfile::tempdir().unwrap();
        crate::configure_test_fixture_model(root.path());
        let published = Arc::new(AtomicBool::new(false));
        let authority = WrappedLocalConfigurationAuthority {
            local: LocalPluginRootAuthority::new(root.path()),
            published: Arc::clone(&published),
        };
        let mut config = crate::AgentWebConfig::new(lenso_agent_console_plugins::link);
        config.agent_home = Some(root.path().to_path_buf());
        config.control = crate::AgentWebControl::HostAuthorized;
        config.plugin_control = true;
        config.plugin_configuration_authority = Some(Arc::new(authority));
        let local = tokio::task::LocalSet::new();

        local
            .run_until(async {
                let surface = crate::AgentWebSurface::start(config).await.unwrap();
                let control = surface.runtime.plugin_control.clone().unwrap();
                add_host_global_resource_overflow(root.path());
                assert_global_budget_rejection_preserves_target(&control, root.path());
                assert!(!published.load(Ordering::SeqCst));
                surface.shutdown().await.unwrap();
            })
            .await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn mutation_coordinator_keeps_cursor_commit_and_registration_together() {
        let coordinator = PluginMutationCoordinator::default();
        let rendezvous = Arc::new(tokio::sync::Barrier::new(2));
        let steps = Arc::new(Mutex::new(Vec::new()));

        let first = coordinator.run({
            let rendezvous = Arc::clone(&rendezvous);
            let steps = Arc::clone(&steps);
            async move {
                steps.lock().unwrap().push("r1.cursor");
                rendezvous.wait().await;
                tokio::task::yield_now().await;
                steps.lock().unwrap().push("r1.commit");
                steps.lock().unwrap().push("r1.register");
            }
        });
        let second = {
            let coordinator = coordinator.clone();
            let rendezvous = Arc::clone(&rendezvous);
            let steps = Arc::clone(&steps);
            async move {
                rendezvous.wait().await;
                coordinator
                    .run(async move {
                        steps.lock().unwrap().push("r2.cursor");
                        steps.lock().unwrap().push("r2.commit");
                        steps.lock().unwrap().push("r2.register");
                    })
                    .await;
            }
        };

        tokio::join!(first, second);

        assert_eq!(
            *steps.lock().unwrap(),
            [
                "r1.cursor",
                "r1.commit",
                "r1.register",
                "r2.cursor",
                "r2.commit",
                "r2.register"
            ]
        );
    }

    #[test]
    fn authoring_lock_child_process() {
        let Ok(root) = std::env::var("LENSO_WEB_LOCK_CHILD_ROOT") else {
            return;
        };
        let mode = std::env::var("LENSO_WEB_LOCK_CHILD_MODE").unwrap();
        if mode == "try" {
            let path = Path::new(&root).join(".lenso/plugin-root-authoring.lock");
            let file = fs::OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .truncate(false)
                .open(path)
                .unwrap();
            println!(
                "authoring-lock={}",
                if file.try_lock().is_ok() {
                    "acquired"
                } else {
                    "blocked"
                }
            );
            return;
        }

        if mode == "wait" {
            let _lock = lock_plugin_root_authoring(Path::new(&root)).unwrap();
            let ready = PathBuf::from(std::env::var("LENSO_WEB_LOCK_CHILD_READY").unwrap());
            fs::write(ready, "acquired").unwrap();
            return;
        }

        let _lock = lock_plugin_root_authoring(Path::new(&root)).unwrap();
        let ready = PathBuf::from(std::env::var("LENSO_WEB_LOCK_CHILD_READY").unwrap());
        let release = PathBuf::from(std::env::var("LENSO_WEB_LOCK_CHILD_RELEASE").unwrap());
        fs::write(&ready, "ready").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !release.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(release.exists(), "parent did not release child lock");
    }

    #[test]
    fn instance_removal_restores_both_files_when_the_second_move_fails() {
        let root = tempfile::tempdir().unwrap();
        let plugin = root.path().join("plugins/example.echo");
        fs::create_dir_all(&plugin).unwrap();
        let configuration = PathBuf::from("plugins/example.echo/default.toml");
        let disabled = PathBuf::from("plugins/example.echo/default.disabled");
        let configuration_bytes = b"message = \"preserve me\"\n";
        let disabled_bytes = b"disabled-marker";
        fs::write(root.path().join(&configuration), configuration_bytes).unwrap();
        fs::write(root.path().join(&disabled), disabled_bytes).unwrap();

        let error = FileRemovalTransaction::begin(
            root.path(),
            &[configuration.clone(), disabled.clone()],
            |index, _| {
                if index == 1 {
                    Err("injected second removal failure".to_owned())
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();

        assert!(error.contains("injected second removal failure"));
        assert_eq!(
            fs::read(root.path().join(configuration)).unwrap(),
            configuration_bytes
        );
        assert_eq!(
            fs::read(root.path().join(disabled)).unwrap(),
            disabled_bytes
        );
    }

    #[test]
    fn staging_links_unchanged_bytes_and_replaces_only_the_overlay_target() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join(".lenso")).unwrap();
        fs::create_dir_all(root.path().join("plugins/example.echo")).unwrap();
        fs::write(root.path().join(".lenso/host-catalog.json"), b"{}").unwrap();
        let source = root.path().join("plugins/example.echo/default.toml");
        fs::write(&source, b"message = \"original\"\n").unwrap();

        let staged = StagedHome::new(root.path()).unwrap();
        let overlay = staged.home.join("plugins/example.echo/default.toml");
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(
                fs::metadata(&source).unwrap().ino(),
                fs::metadata(&overlay).unwrap().ino(),
                "unchanged Plugin bytes should be linked instead of copied"
            );
        }
        atomic_write(&overlay, b"message = \"candidate\"\n").unwrap();

        assert_eq!(fs::read(&source).unwrap(), b"message = \"original\"\n");
        assert_eq!(fs::read(&overlay).unwrap(), b"message = \"candidate\"\n");
    }

    #[test]
    fn staging_budget_fails_closed_before_an_unbounded_walk() {
        let mut budget = StagingBudget::new(1);
        budget.consume(Path::new("first")).unwrap();
        let error = budget.consume(Path::new("second")).unwrap_err();
        assert!(error.contains("exceeds 16384 filesystem entries"));
    }

    #[test]
    fn configuration_source_validation_precedes_path_read_and_bounds_current_bytes() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("outside.toml"), b"secret = true\n").unwrap();

        let traversal =
            current_configuration_source_digest(root.path(), "..", "outside").unwrap_err();
        assert_eq!(traversal, "invalid Plugin ID `..`");
        assert!(!traversal.contains("sha256:"));

        let configuration = root.path().join("plugins/example.echo/default.toml");
        fs::create_dir_all(configuration.parent().unwrap()).unwrap();
        let file = fs::File::create(&configuration).unwrap();
        file.set_len(MAX_PLUGIN_CONFIGURATION_BYTES as u64 + 1)
            .unwrap();
        let oversized = current_configuration_source_digest(root.path(), "example.echo", "default")
            .unwrap_err();
        assert!(oversized.contains("exceeds 256 KiB"), "{oversized}");
    }

    #[test]
    fn post_commit_linearization_blocks_a_child_writer_until_receipt_registration() {
        let root = tempfile::tempdir().unwrap();
        let ready = root.path().join("child-acquired-after-receipt");
        let linearization = PluginMutationLinearization::acquire(root.path(), root.path()).unwrap();
        let executable = std::env::current_exe().unwrap();
        let filter = "plugin_control::tests::authoring_lock_child_process";
        let mut child = Command::new(executable)
            .args(["--exact", filter, "--nocapture"])
            .env("LENSO_WEB_LOCK_CHILD_ROOT", root.path())
            .env("LENSO_WEB_LOCK_CHILD_MODE", "wait")
            .env("LENSO_WEB_LOCK_CHILD_READY", &ready)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();

        std::thread::sleep(Duration::from_millis(100));
        assert!(
            !ready.exists(),
            "child writer acquired the authoring lock before receipt registration"
        );
        fs::write(root.path().join("receipt-registered"), "registered").unwrap();
        drop(linearization);

        let deadline = Instant::now() + Duration::from_secs(2);
        while !ready.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            ready.exists(),
            "child writer did not resume after registration"
        );
        assert!(child.wait().unwrap().success());
    }

    #[test]
    fn authoring_lock_is_shared_bidirectionally_with_other_processes() {
        let root = tempfile::tempdir().unwrap();
        let executable = std::env::current_exe().unwrap();
        let filter = "plugin_control::tests::authoring_lock_child_process";

        let parent_lock = lock_plugin_root_authoring(root.path()).unwrap();
        let output = Command::new(&executable)
            .args(["--exact", filter, "--nocapture"])
            .env("LENSO_WEB_LOCK_CHILD_ROOT", root.path())
            .env("LENSO_WEB_LOCK_CHILD_MODE", "try")
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("authoring-lock=blocked"),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        drop(parent_lock);

        let ready = root.path().join("child-ready");
        let release = root.path().join("child-release");
        let mut child = Command::new(&executable)
            .args(["--exact", filter, "--nocapture"])
            .env("LENSO_WEB_LOCK_CHILD_ROOT", root.path())
            .env("LENSO_WEB_LOCK_CHILD_MODE", "hold")
            .env("LENSO_WEB_LOCK_CHILD_READY", &ready)
            .env("LENSO_WEB_LOCK_CHILD_RELEASE", &release)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !ready.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(ready.exists(), "child did not acquire the authoring lock");
        let lock_path = root.path().join(".lenso/plugin-root-authoring.lock");
        let competing = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(lock_path)
            .unwrap();
        assert!(competing.try_lock().is_err());
        fs::write(&release, "release").unwrap();
        assert!(child.wait().unwrap().success());

        // The released child lock must not leave stale ownership behind.
        let mut final_lock = lock_plugin_root_authoring(root.path()).unwrap();
        final_lock.flush().unwrap();
    }
}
