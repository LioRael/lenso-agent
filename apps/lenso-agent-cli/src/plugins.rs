use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use fs2::FileExt;
use lenso_agent_text_tools_module::{
    FACTORY_IDENTITY as TEXT_TOOLS_FACTORY_IDENTITY, PACKAGE_ID as TEXT_TOOLS_PACKAGE_ID,
};
use lenso_app_plan::{CapabilityBinding, ResolvedAppPlan};
use lenso_capability_agent_tool_provider::{
    CAPABILITY_ID as TOOL_PROVIDER_CAPABILITY_ID,
    CATALOG_OPERATION as TOOL_PROVIDER_CATALOG_OPERATION,
    DESCRIPTOR_VERSION as TOOL_PROVIDER_DESCRIPTOR_VERSION,
    EXECUTE_OPERATION as TOOL_PROVIDER_EXECUTE_OPERATION,
};
use lenso_plugin_control_plane::{
    AdmissionPolicy, CanonicalDocument, ControlPlaneError, LockedInstance, LockedPlugin,
    ModuleContribution, PluginBundle, PluginManifest, PluginSetLock, PluginStore, SupportChannel,
    TrustLevel, sha256_digest,
};
use serde::{Deserialize, Serialize};

const APP_ID: &str = "lenso.agent.harness";
const ACTIVE_SET_FILE: &str = "active-set.json";
const LOCK_FILE: &str = "active-set.lock";
const MANIFEST_FILE: &str = "lenso-plugin.json";
const LOCAL_REVIEW_PROVENANCE: &str = "local-review";
const LOCAL_REVIEW_POLICY: &str = "lenso.agent.local-review@1";
const MAX_BUNDLE_FILES: usize = 4_096;
const MAX_BUNDLE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_BUNDLE_DEPTH: usize = 32;
const MAX_EVIDENCE_BYTES: usize = 4_096;
const NATIVE_EXECUTION_CLASS: &str = "lenso.native-rust@1";
const NATIVE_TOOL_PROFILE: &str = "agent-tool-provider-v1";
const TOOL_AGGREGATOR_INSTANCE: &str = "tools";
const EMPTY_CONFIGURATION_SCHEMA: &[u8] = br#"{"additionalProperties":false,"type":"object"}"#;
const TOOL_PROVIDER_DESCRIPTOR: &[u8] =
    include_bytes!("../../../crates/lenso-capability-agent-tool-provider/capability.json");

#[derive(Debug)]
pub enum PluginCommand {
    Install {
        bundle: PathBuf,
        evidence: String,
        features: Vec<String>,
        root: PathBuf,
    },
    Remove {
        plugin_id: String,
        root: PathBuf,
    },
    Status {
        root: PathBuf,
    },
}

pub fn parse_command(arguments: &[String]) -> Result<PluginCommand, String> {
    let Some(command) = arguments.first() else {
        return Err(usage());
    };
    match command.as_str() {
        "install" => parse_install(&arguments[1..]),
        "remove" => parse_remove(&arguments[1..]),
        "status" => parse_status(&arguments[1..]),
        _ => Err(usage()),
    }
}

pub fn run(command: PluginCommand) -> Result<(), String> {
    match command {
        PluginCommand::Install {
            bundle,
            evidence,
            features,
            root,
        } => {
            let outcome = install(&root, &bundle, &evidence, features)?;
            println!(
                "installed: {}@{}",
                outcome.plugin_id, outcome.release_version
            );
            println!("manifest: {}", outcome.manifest_digest);
            println!("receipt: {}", outcome.receipt_digest);
            println!("plugin-set: {}", outcome.plugin_set_digest);
            Ok(())
        }
        PluginCommand::Status { root } => {
            let authority = load_generation_authority(&root)?;
            if authority.lock.value().plugins.is_empty() {
                println!("No active Plugin releases.");
            } else {
                for plugin in &authority.lock.value().plugins {
                    println!(
                        "{}@{} {}",
                        plugin.plugin_id, plugin.release_version, plugin.manifest_digest
                    );
                }
            }
            println!("plugin-set: {}", authority.lock.digest());
            Ok(())
        }
        PluginCommand::Remove { plugin_id, root } => {
            let digest = remove(&root, &plugin_id)?;
            println!("removed: {plugin_id}");
            println!("plugin-set: {digest}");
            Ok(())
        }
    }
}

fn parse_install(arguments: &[String]) -> Result<PluginCommand, String> {
    let mut bundle = None;
    let mut evidence = None;
    let mut features = Vec::new();
    let mut root = default_root();
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--bundle" => {
                bundle = Some(PathBuf::from(arguments.next().ok_or_else(usage)?.as_str()));
            }
            "--evidence" => {
                evidence = Some(arguments.next().ok_or_else(usage)?.clone());
            }
            "--feature" => features.push(arguments.next().ok_or_else(usage)?.clone()),
            "--root" => {
                root = PathBuf::from(arguments.next().ok_or_else(usage)?.as_str());
            }
            _ => return Err(usage()),
        }
    }
    Ok(PluginCommand::Install {
        bundle: bundle.ok_or_else(usage)?,
        evidence: evidence.ok_or_else(usage)?,
        features,
        root,
    })
}

fn parse_status(arguments: &[String]) -> Result<PluginCommand, String> {
    let root = match arguments {
        [] => default_root(),
        [flag, root] if flag == "--root" => PathBuf::from(root),
        _ => return Err(usage()),
    };
    Ok(PluginCommand::Status { root })
}

fn parse_remove(arguments: &[String]) -> Result<PluginCommand, String> {
    let mut plugin_id = None;
    let mut root = default_root();
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--plugin" => plugin_id = Some(arguments.next().ok_or_else(usage)?.clone()),
            "--root" => root = PathBuf::from(arguments.next().ok_or_else(usage)?.as_str()),
            _ => return Err(usage()),
        }
    }
    Ok(PluginCommand::Remove {
        plugin_id: plugin_id.ok_or_else(usage)?,
        root,
    })
}

fn usage() -> String {
    "usage: lenso-agent-cli plugins <install --bundle <directory> --evidence <review> [--feature <id>]... [--root <directory>]|remove --plugin <id> [--root <directory>]|status [--root <directory>]>".to_owned()
}

fn default_root() -> PathBuf {
    PathBuf::from(".lenso/plugins")
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ActivePluginSet {
    schema_version: u32,
    lock: PluginSetLock,
    releases: Vec<ActiveRelease>,
}

impl ActivePluginSet {
    fn empty() -> Self {
        Self {
            schema_version: 1,
            lock: PluginSetLock {
                schema_version: 1,
                app_id: APP_ID.to_owned(),
                plugins: Vec::new(),
                instances: Vec::new(),
                data_mounts: Vec::new(),
                approved_grants: Vec::new(),
            },
            releases: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ActiveRelease {
    plugin_id: String,
    manifest: PluginManifest,
    admission_receipt_digest: String,
}

#[derive(Debug)]
pub(crate) struct GenerationPluginAuthority {
    pub(crate) store: PluginStore,
    pub(crate) lock: CanonicalDocument<PluginSetLock>,
    pub(crate) manifests: BTreeMap<String, CanonicalDocument<PluginManifest>>,
    pub(crate) admission_receipts: BTreeMap<String, String>,
}

#[derive(Debug, Eq, PartialEq)]
struct InstallOutcome {
    plugin_id: String,
    release_version: String,
    manifest_digest: String,
    receipt_digest: String,
    plugin_set_digest: String,
}

#[derive(Debug)]
struct LoadedBundle {
    manifest: Vec<u8>,
    files: BTreeMap<String, Vec<u8>>,
}

#[derive(Debug, Eq, PartialEq)]
struct SelectedContent {
    module_contribution_ids: Vec<String>,
    product_metadata_digests: Vec<String>,
}

#[derive(Debug)]
struct LocalReviewPolicy<'a> {
    evidence: &'a str,
}

impl AdmissionPolicy for LocalReviewPolicy<'_> {
    fn admit(
        &self,
        manifest: &PluginManifest,
        _manifest_digest: &str,
        _artifact_digests: &[String],
        _product_metadata_digests: &[String],
        provenance: &str,
    ) -> Result<String, ControlPlaneError> {
        if provenance != LOCAL_REVIEW_PROVENANCE {
            return rejected("Plugin Bundle provenance is not local review");
        }
        validate_supported_manifest(manifest)?;
        let evidence = self.evidence.trim();
        if evidence.is_empty() || evidence.len() > MAX_EVIDENCE_BYTES {
            return rejected("review evidence must be non-empty and bounded");
        }
        Ok(evidence.to_owned())
    }

    fn identity(&self) -> &'static str {
        LOCAL_REVIEW_POLICY
    }
}

fn install(
    root: &Path,
    bundle_root: &Path,
    evidence: &str,
    mut features: Vec<String>,
) -> Result<InstallOutcome, String> {
    prepare_root(root)?;
    let lock_file = exclusive_lock(root)?;
    let store = PluginStore::open(root.join("store")).map_err(control_error)?;
    let mut active = validate_active_set(load_active_set(root)?, &store)?.into_value();
    let bundle = load_bundle(bundle_root)?;
    let manifest = CanonicalDocument::<PluginManifest>::parse(MANIFEST_FILE, &bundle.manifest)
        .map_err(control_error)?;
    validate_supported_manifest(manifest.value()).map_err(control_error)?;
    let feature_count = features.len();
    features.sort();
    features.dedup();
    if features.len() != feature_count {
        return Err("selected Plugin Features contain a duplicate".to_owned());
    }

    if let Some(existing) = active
        .lock
        .plugins
        .iter()
        .find(|plugin| plugin.plugin_id == manifest.value().plugin_id)
        && existing.manifest_digest != manifest.digest()
    {
        return Err(format!(
            "Plugin `{}` is already locked to another immutable Release",
            manifest.value().plugin_id
        ));
    }

    let receipt = store
        .admit(
            &PluginBundle::new(bundle.manifest, bundle.files, LOCAL_REVIEW_PROVENANCE),
            &LocalReviewPolicy { evidence },
        )
        .map_err(control_error)?;
    let selected = validate_selection(manifest.value(), &features).map_err(control_error)?;
    let locked = LockedPlugin {
        plugin_id: manifest.value().plugin_id.clone(),
        release_version: manifest.value().release_version.clone(),
        manifest_digest: manifest.digest().to_owned(),
        selected_features: features,
        product_metadata_digests: selected.product_metadata_digests,
    };
    active
        .lock
        .plugins
        .retain(|plugin| plugin.plugin_id != locked.plugin_id);
    active.lock.plugins.push(locked);
    active
        .lock
        .plugins
        .sort_by(|left, right| left.plugin_id.cmp(&right.plugin_id));
    active
        .lock
        .instances
        .retain(|instance| instance.plugin_id != manifest.value().plugin_id);
    active.lock.instances.extend(locked_instances(
        manifest.value(),
        &selected.module_contribution_ids,
    ));
    active
        .lock
        .instances
        .sort_by(|left, right| left.instance_key.cmp(&right.instance_key));
    active
        .releases
        .retain(|release| release.plugin_id != manifest.value().plugin_id);
    active.releases.push(ActiveRelease {
        plugin_id: manifest.value().plugin_id.clone(),
        manifest: manifest.value().clone(),
        admission_receipt_digest: receipt.digest().to_owned(),
    });
    active
        .releases
        .sort_by(|left, right| left.plugin_id.cmp(&right.plugin_id));
    let active_document = validate_active_set(active, &store)?;
    write_active_set(root, &active_document)?;
    drop(lock_file);
    let lock = CanonicalDocument::from_value(
        "lenso-plugins.lock.json",
        active_document.value().lock.clone(),
    )
    .map_err(control_error)?;
    Ok(InstallOutcome {
        plugin_id: manifest.value().plugin_id.clone(),
        release_version: manifest.value().release_version.clone(),
        manifest_digest: manifest.digest().to_owned(),
        receipt_digest: receipt.digest().to_owned(),
        plugin_set_digest: lock.digest().to_owned(),
    })
}

fn remove(root: &Path, plugin_id: &str) -> Result<String, String> {
    prepare_root(root)?;
    let lock_file = exclusive_lock(root)?;
    let store = PluginStore::open(root.join("store")).map_err(control_error)?;
    let mut active = validate_active_set(load_active_set(root)?, &store)?.into_value();
    if !active
        .lock
        .plugins
        .iter()
        .any(|plugin| plugin.plugin_id == plugin_id)
    {
        return Err(format!("Plugin `{plugin_id}` is not active"));
    }
    active
        .lock
        .plugins
        .retain(|plugin| plugin.plugin_id != plugin_id);
    active
        .lock
        .instances
        .retain(|instance| instance.plugin_id != plugin_id);
    active
        .releases
        .retain(|release| release.plugin_id != plugin_id);
    let active = validate_active_set(active, &store)?;
    write_active_set(root, &active)?;
    drop(lock_file);
    let lock =
        CanonicalDocument::from_value("lenso-plugins.lock.json", active.value().lock.clone())
            .map_err(control_error)?;
    Ok(lock.digest().to_owned())
}

pub(crate) fn load_generation_authority(root: &Path) -> Result<GenerationPluginAuthority, String> {
    prepare_root(root)?;
    let store = PluginStore::open(root.join("store")).map_err(control_error)?;
    let active = load_active_set(root)?;
    let active = validate_active_set(active, &store)?;
    let lock =
        CanonicalDocument::from_value("lenso-plugins.lock.json", active.value().lock.clone())
            .map_err(control_error)?;
    let mut manifests = BTreeMap::new();
    let mut admission_receipts = BTreeMap::new();
    for release in &active.value().releases {
        let manifest = CanonicalDocument::from_value("lenso-plugin.json", release.manifest.clone())
            .map_err(control_error)?;
        manifests.insert(release.plugin_id.clone(), manifest.clone());
        admission_receipts.insert(
            manifest.digest().to_owned(),
            release.admission_receipt_digest.clone(),
        );
    }
    Ok(GenerationPluginAuthority {
        store,
        lock,
        manifests,
        admission_receipts,
    })
}

pub(crate) fn generation_bindings(
    authority: &GenerationPluginAuthority,
    base_plan: &ResolvedAppPlan,
) -> Result<Vec<CapabilityBinding>, String> {
    if authority.lock.value().instances.is_empty() {
        return Ok(base_plan.capability_bindings().to_vec());
    }
    let aggregator = base_plan
        .module_instance(TOOL_AGGREGATOR_INSTANCE)
        .ok_or_else(|| "active Tool Plugins require the `tools` aggregator Instance".to_owned())?;
    if !aggregator
        .required_capabilities()
        .iter()
        .any(|requirement| {
            requirement.capability_id() == TOOL_PROVIDER_CAPABILITY_ID
                && requirement.descriptor_version() == TOOL_PROVIDER_DESCRIPTOR_VERSION
        })
    {
        return Err("the `tools` Instance does not accept Tool Provider Plugins".to_owned());
    }
    let mut bindings = base_plan.capability_bindings().to_vec();
    bindings.extend(authority.lock.value().instances.iter().map(|instance| {
        CapabilityBinding::new(
            TOOL_AGGREGATOR_INSTANCE,
            TOOL_PROVIDER_CAPABILITY_ID,
            TOOL_PROVIDER_DESCRIPTOR_VERSION,
            &instance.instance_key,
        )
    }));
    Ok(bindings)
}

fn validate_active_set(
    active: ActivePluginSet,
    store: &PluginStore,
) -> Result<CanonicalDocument<ActivePluginSet>, String> {
    if active.schema_version != 1 || active.lock.schema_version != 1 || active.lock.app_id != APP_ID
    {
        return Err("active Plugin Set schema or App identity is invalid".to_owned());
    }
    if !active.lock.data_mounts.is_empty() || !active.lock.approved_grants.is_empty() {
        return Err("this Host does not accept Plugin Data mounts or permission grants".to_owned());
    }
    ensure_sorted_unique(
        active.lock.plugins.iter().map(|plugin| &plugin.plugin_id),
        "locked Plugin",
    )?;
    ensure_sorted_unique(
        active.releases.iter().map(|release| &release.plugin_id),
        "active Release",
    )?;
    ensure_sorted_unique(
        active
            .lock
            .instances
            .iter()
            .map(|instance| &instance.instance_key),
        "locked Plugin Instance",
    )?;
    if active.lock.plugins.len() != active.releases.len() {
        return Err("active Releases do not exactly close the Plugin lock".to_owned());
    }
    let mut expected_instances = Vec::new();
    for locked in &active.lock.plugins {
        let release = active
            .releases
            .iter()
            .find(|release| release.plugin_id == locked.plugin_id)
            .ok_or_else(|| format!("Plugin `{}` has no active Release", locked.plugin_id))?;
        let manifest = CanonicalDocument::from_value("lenso-plugin.json", release.manifest.clone())
            .map_err(control_error)?;
        validate_supported_manifest(manifest.value()).map_err(control_error)?;
        if manifest.digest() != locked.manifest_digest
            || manifest.value().plugin_id != locked.plugin_id
            || manifest.value().release_version != locked.release_version
        {
            return Err(format!(
                "Plugin `{}` Manifest does not close its lock",
                locked.plugin_id
            ));
        }
        let selected = validate_selection(manifest.value(), &locked.selected_features)
            .map_err(control_error)?;
        if selected.product_metadata_digests != locked.product_metadata_digests {
            return Err(format!(
                "Plugin `{}` Product Metadata selection is not exact",
                locked.plugin_id
            ));
        }
        expected_instances.extend(locked_instances(
            manifest.value(),
            &selected.module_contribution_ids,
        ));
        validate_receipt_closure(locked, release, manifest.value(), store)?;
    }
    expected_instances.sort_by(|left, right| left.instance_key.cmp(&right.instance_key));
    if active.lock.instances != expected_instances {
        return Err(
            "active Plugin Instances do not exactly close selected contributions".to_owned(),
        );
    }
    CanonicalDocument::from_value("active-set.json", active).map_err(control_error)
}

fn validate_receipt_closure(
    locked: &LockedPlugin,
    release: &ActiveRelease,
    manifest: &PluginManifest,
    store: &PluginStore,
) -> Result<(), String> {
    let receipt = store
        .admission_receipt(&release.admission_receipt_digest)
        .map_err(control_error)?;
    if receipt.value().policy_identity != LOCAL_REVIEW_POLICY
        || receipt.value().provenance != LOCAL_REVIEW_PROVENANCE
        || receipt.value().plugin_id != locked.plugin_id
        || receipt.value().release_version != locked.release_version
        || receipt.value().manifest_digest != locked.manifest_digest
    {
        return Err(format!(
            "Plugin `{}` Admission Receipt does not close its lock",
            locked.plugin_id
        ));
    }
    let artifact_digests = manifest
        .artifacts
        .iter()
        .map(|artifact| artifact.digest.clone())
        .collect::<BTreeSet<_>>();
    let metadata_digests = manifest
        .product_metadata
        .iter()
        .map(|metadata| metadata.digest.clone())
        .collect::<BTreeSet<_>>();
    if receipt
        .value()
        .artifact_digests
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        != artifact_digests
        || receipt
            .value()
            .product_metadata_digests
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            != metadata_digests
        || receipt.value().decision_evidence.trim().is_empty()
        || receipt.value().decision_evidence.len() > MAX_EVIDENCE_BYTES
    {
        return Err(format!(
            "Plugin `{}` Admission Receipt does not close its artifacts or review evidence",
            locked.plugin_id
        ));
    }
    Ok(())
}

fn validate_supported_manifest(manifest: &PluginManifest) -> Result<(), ControlPlaneError> {
    if !manifest.data_contributions.is_empty()
        || !manifest.permission_requests.is_empty()
        || !manifest.binding_templates.is_empty()
    {
        return rejected(
            "this Host rejects Plugin Data mounts, permission requests, and binding templates",
        );
    }
    for contribution in &manifest.module_contributions {
        validate_tool_contribution(contribution)?;
    }
    Ok(())
}

fn validate_tool_contribution(contribution: &ModuleContribution) -> Result<(), ControlPlaneError> {
    let target = host_target();
    let exact_endpoint = if let [provided] = contribution.provides.as_slice() {
        provided.capability_id == TOOL_PROVIDER_CAPABILITY_ID
            && provided.descriptor_version == TOOL_PROVIDER_DESCRIPTOR_VERSION
            && provided.descriptor_digest == sha256_digest(TOOL_PROVIDER_DESCRIPTOR)
            && provided.request_operations
                == [
                    TOOL_PROVIDER_CATALOG_OPERATION,
                    TOOL_PROVIDER_EXECUTE_OPERATION,
                ]
    } else {
        false
    };
    if !exact_endpoint
        || contribution.package_id != TEXT_TOOLS_PACKAGE_ID
        || contribution.configuration_schema_digest != sha256_digest(EMPTY_CONFIGURATION_SCHEMA)
        || !contribution.requires.is_empty()
        || !contribution.permission_request_ids.is_empty()
        || contribution.state.is_some()
        || contribution.implementations.is_empty()
        || !contribution
            .implementations
            .iter()
            .any(|implementation| implementation.targets.contains(&target))
        || contribution.implementations.iter().any(|implementation| {
            implementation.artifact.is_some()
                || implementation.built_in_factory.as_deref() != Some(TEXT_TOOLS_FACTORY_IDENTITY)
                || implementation.entrypoint != "default"
                || implementation.execution_class != NATIVE_EXECUTION_CLASS
                || !implementation
                    .profiles
                    .iter()
                    .any(|profile| profile == NATIVE_TOOL_PROFILE)
                || implementation.support_channel != SupportChannel::Stable
                || implementation.trust != TrustLevel::Trusted
        })
    {
        return rejected(
            "executable Plugins must be stateless, permission-free native Tool Providers",
        );
    }
    Ok(())
}

fn validate_selection(
    manifest: &PluginManifest,
    features: &[String],
) -> Result<SelectedContent, ControlPlaneError> {
    if features.windows(2).any(|pair| pair[0] >= pair[1]) {
        return rejected("selected Plugin Features must be sorted and unique");
    }
    let selected = features.iter().cloned().collect::<BTreeSet<_>>();
    if selected.iter().any(|feature| {
        !manifest
            .features
            .iter()
            .any(|candidate| &candidate.id == feature)
    }) {
        return rejected("selected Plugin Feature is unknown");
    }
    let featured_artifacts = manifest
        .features
        .iter()
        .flat_map(|feature| feature.artifact_ids.iter())
        .collect::<BTreeSet<_>>();
    let featured_modules = manifest
        .features
        .iter()
        .flat_map(|feature| feature.module_contribution_ids.iter())
        .collect::<BTreeSet<_>>();
    let mut module_contribution_ids = manifest
        .module_contributions
        .iter()
        .filter(|contribution| !featured_modules.contains(&contribution.id))
        .map(|contribution| contribution.id.clone())
        .collect::<BTreeSet<_>>();
    let mut selected_artifacts = manifest
        .artifacts
        .iter()
        .filter(|artifact| !featured_artifacts.contains(&artifact.id))
        .map(|artifact| &artifact.id)
        .collect::<BTreeSet<_>>();
    let featured_metadata = manifest
        .features
        .iter()
        .flat_map(|feature| feature.product_metadata_ids.iter())
        .collect::<BTreeSet<_>>();
    let mut metadata_digests = manifest
        .product_metadata
        .iter()
        .filter(|metadata| !featured_metadata.contains(&metadata.id))
        .map(|metadata| metadata.digest.clone())
        .collect::<BTreeSet<_>>();
    for feature in manifest
        .features
        .iter()
        .filter(|feature| selected.contains(&feature.id))
    {
        module_contribution_ids.extend(feature.module_contribution_ids.iter().cloned());
        selected_artifacts.extend(feature.artifact_ids.iter());
        for metadata_id in &feature.product_metadata_ids {
            let metadata = manifest
                .product_metadata
                .iter()
                .find(|metadata| &metadata.id == metadata_id)
                .expect("Store admission validates Feature references");
            metadata_digests.insert(metadata.digest.clone());
        }
    }
    let target = host_target();
    if selected_artifacts.iter().any(|artifact_id| {
        manifest
            .artifacts
            .iter()
            .find(|artifact| &artifact.id == *artifact_id)
            .is_none_or(|artifact| !artifact.targets.contains(&target))
    }) {
        return rejected("selected Plugin Artifact does not support this Host target");
    }
    Ok(SelectedContent {
        module_contribution_ids: module_contribution_ids.into_iter().collect(),
        product_metadata_digests: metadata_digests.into_iter().collect(),
    })
}

fn locked_instances(
    manifest: &PluginManifest,
    selected_contributions: &[String],
) -> Vec<LockedInstance> {
    selected_contributions
        .iter()
        .map(|contribution_id| LockedInstance {
            plugin_id: manifest.plugin_id.clone(),
            contribution_id: contribution_id.clone(),
            instance_key: format!(
                "plugin:{}:{}:{contribution_id}",
                manifest.plugin_id.len(),
                manifest.plugin_id
            ),
            implementation_variant: None,
            configuration: "{}".to_owned(),
            execution_lane: "main".to_owned(),
        })
        .collect()
}

pub(crate) fn host_target() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

fn load_bundle(root: &Path) -> Result<LoadedBundle, String> {
    let input_metadata = fs::symlink_metadata(root)
        .map_err(|error| format!("failed to open Plugin Bundle {}: {error}", root.display()))?;
    if input_metadata.file_type().is_symlink() {
        return Err("Plugin Bundle root is a symlink".to_owned());
    }
    let root = fs::canonicalize(root)
        .map_err(|error| format!("failed to open Plugin Bundle {}: {error}", root.display()))?;
    if !fs::metadata(&root)
        .map_err(|error| format!("failed to inspect Plugin Bundle: {error}"))?
        .is_dir()
    {
        return Err("Plugin Bundle path is not a directory".to_owned());
    }
    let mut paths = Vec::new();
    let mut entries = 0_usize;
    collect_bundle_paths(&root, &root, 0, &mut entries, &mut paths)?;
    paths.sort();
    if paths.len() > MAX_BUNDLE_FILES {
        return Err("Plugin Bundle contains too many files".to_owned());
    }
    let mut total = 0_u64;
    let mut manifest = None;
    let mut files = BTreeMap::new();
    for (relative, path) in paths {
        let bytes = fs::read(&path)
            .map_err(|error| format!("failed to read Bundle file `{relative}`: {error}"))?;
        total = total
            .checked_add(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
            .ok_or_else(|| "Plugin Bundle byte count overflowed".to_owned())?;
        if total > MAX_BUNDLE_BYTES {
            return Err("Plugin Bundle exceeds the total byte limit".to_owned());
        }
        if relative == MANIFEST_FILE {
            manifest = Some(bytes);
        } else {
            files.insert(relative, bytes);
        }
    }
    Ok(LoadedBundle {
        manifest: manifest.ok_or_else(|| format!("Plugin Bundle is missing `{MANIFEST_FILE}`"))?,
        files,
    })
}

fn collect_bundle_paths(
    root: &Path,
    directory: &Path,
    depth: usize,
    entry_count: &mut usize,
    paths: &mut Vec<(String, PathBuf)>,
) -> Result<(), String> {
    if depth > MAX_BUNDLE_DEPTH {
        return Err("Plugin Bundle directory depth exceeds its limit".to_owned());
    }
    let mut entries = fs::read_dir(directory)
        .map_err(|error| format!("failed to enumerate Plugin Bundle: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("failed to enumerate Plugin Bundle: {error}"))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        *entry_count = entry_count
            .checked_add(1)
            .ok_or_else(|| "Plugin Bundle entry count overflowed".to_owned())?;
        if *entry_count > MAX_BUNDLE_FILES {
            return Err("Plugin Bundle contains too many entries".to_owned());
        }
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("failed to inspect Bundle entry: {error}"))?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "Plugin Bundle entry `{}` is a symlink",
                path.display()
            ));
        }
        if metadata.is_dir() {
            collect_bundle_paths(root, &path, depth + 1, entry_count, paths)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .expect("Bundle traversal remains below its root")
                .components()
                .map(|component| component.as_os_str().to_str())
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| "Plugin Bundle path is not UTF-8".to_owned())?
                .join("/");
            paths.push((relative, path));
        } else {
            return Err("Plugin Bundle contains a non-file entry".to_owned());
        }
        if paths.len() > MAX_BUNDLE_FILES {
            return Err("Plugin Bundle contains too many files".to_owned());
        }
    }
    Ok(())
}

fn prepare_root(root: &Path) -> Result<(), String> {
    fs::create_dir_all(root)
        .map_err(|error| format!("failed to create Plugin authority root: {error}"))?;
    let metadata = fs::symlink_metadata(root)
        .map_err(|error| format!("failed to inspect Plugin authority root: {error}"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Plugin authority root is not a regular directory".to_owned());
    }
    Ok(())
}

fn exclusive_lock(root: &Path) -> Result<File, String> {
    let path = root.join(LOCK_FILE);
    if fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err("Plugin authority lock is a symlink".to_owned());
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|error| format!("failed to open Plugin authority lock: {error}"))?;
    file.lock_exclusive()
        .map_err(|error| format!("failed to lock Plugin authority: {error}"))?;
    Ok(file)
}

fn load_active_set(root: &Path) -> Result<ActivePluginSet, String> {
    let path = root.join(ACTIVE_SET_FILE);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ActivePluginSet::empty());
        }
        Err(error) => return Err(format!("failed to inspect active Plugin Set: {error}")),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("active Plugin Set is not a regular file".to_owned());
    }
    let bytes =
        fs::read(path).map_err(|error| format!("failed to read active Plugin Set: {error}"))?;
    CanonicalDocument::<ActivePluginSet>::parse("active-set.json", &bytes)
        .map(CanonicalDocument::into_value)
        .map_err(control_error)
}

fn write_active_set(
    root: &Path,
    active: &CanonicalDocument<ActivePluginSet>,
) -> Result<(), String> {
    let destination = root.join(ACTIVE_SET_FILE);
    let (temporary, mut file) = create_transaction(root)?;
    let result = (|| {
        file.write_all(active.bytes())
            .map_err(|error| format!("failed to write Plugin Set transaction: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("failed to sync Plugin Set transaction: {error}"))?;
        fs::rename(&temporary, &destination)
            .map_err(|error| format!("failed to commit Plugin Set transaction: {error}"))?;
        File::open(root)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("failed to sync Plugin authority root: {error}"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn create_transaction(root: &Path) -> Result<(PathBuf, File), String> {
    for attempt in 0_u16..=u16::MAX {
        let path = root.join(format!(
            "{ACTIVE_SET_FILE}.{}-{attempt}.tmp",
            std::process::id()
        ));
        match OpenOptions::new().create_new(true).write(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(format!("failed to create Plugin Set transaction: {error}"));
            }
        }
    }
    Err("failed to allocate Plugin Set transaction".to_owned())
}

fn ensure_sorted_unique<'a>(
    values: impl Iterator<Item = &'a String>,
    kind: &str,
) -> Result<(), String> {
    let values = values.collect::<Vec<_>>();
    if values.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(format!("{kind} entries must be sorted and unique"));
    }
    Ok(())
}

fn rejected<T>(detail: impl Into<String>) -> Result<T, ControlPlaneError> {
    Err(ControlPlaneError::AdmissionRejected {
        detail: detail.into(),
    })
}

#[allow(clippy::needless_pass_by_value)]
fn control_error(error: ControlPlaneError) -> String {
    format!("Plugin control plane failed: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use lenso_app_plan::ResolvedAppPlan;
    use lenso_plugin_control_plane::{
        ArtifactDeclaration, ArtifactKind, CapabilityDeclaration, ImplementationVariant,
        ModuleContribution, PermissionRequest, PluginFeature, ProductMetadataDeclaration,
        sha256_digest,
    };

    const PLAN: &[u8] = include_bytes!("../../../composition/headless-readonly/resolved-plan.json");

    #[test]
    fn reviewed_passive_release_is_locked_and_closed_into_a_generation() {
        let root = tempfile::tempdir().unwrap();
        let bundle = tempfile::tempdir().unwrap();
        let manifest = write_passive_bundle(bundle.path());
        let outcome = install(
            root.path(),
            bundle.path(),
            "review-ticket-42",
            vec!["extras".to_owned()],
        )
        .unwrap();
        assert_eq!(outcome.plugin_id, "example.passive");
        assert_eq!(outcome.manifest_digest, manifest.digest());

        let authority = load_generation_authority(root.path()).unwrap();
        assert_eq!(authority.lock.value().plugins.len(), 1);
        assert_eq!(
            authority.lock.value().plugins[0].selected_features,
            ["extras"]
        );
        let generation = crate::generation::resolve_initial_generation(PLAN, root.path()).unwrap();
        let approved: ResolvedAppPlan = serde_json::from_slice(PLAN).unwrap();
        assert_eq!(generation.plan, approved);
        assert_eq!(generation.artifact_set.value().releases.len(), 1);
        assert_eq!(generation.artifact_set.value().artifacts.len(), 1);
        assert_eq!(
            generation.artifact_set.value().artifacts[0].artifact_id,
            "extra"
        );
    }

    #[test]
    fn executable_or_permission_bearing_release_is_rejected_before_activation() {
        let root = tempfile::tempdir().unwrap();
        let bundle = tempfile::tempdir().unwrap();
        let mut manifest = passive_manifest(b"extra", b"{\"kind\":\"fixture\"}");
        manifest.permission_requests.push(PermissionRequest {
            id: "network".to_owned(),
            resource_kind: "network".to_owned(),
            required: true,
            scope: serde_json::json!({"hosts": ["example.com"]}),
            explanation_key: "network.required".to_owned(),
        });
        write_bundle(
            bundle.path(),
            &manifest,
            b"extra",
            b"{\"kind\":\"fixture\"}",
        );
        let error = install(root.path(), bundle.path(), "review", Vec::new()).unwrap_err();
        assert!(error.contains("permission requests"));
        assert!(!root.path().join(ACTIVE_SET_FILE).exists());
    }

    #[test]
    fn unregistered_native_tool_factory_is_rejected_before_activation() {
        let root = tempfile::tempdir().unwrap();
        let bundle = tempfile::tempdir().unwrap();
        write_tool_bundle(bundle.path());
        let manifest_path = bundle.path().join(MANIFEST_FILE);
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["module_contributions"][0]["implementations"][0]["built_in_factory"] =
            "unregistered@1".into();
        fs::write(manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();

        let error = install(root.path(), bundle.path(), "review", Vec::new()).unwrap_err();
        assert!(error.contains("stateless, permission-free native Tool Providers"));
        assert!(!root.path().join(ACTIVE_SET_FILE).exists());
    }

    #[test]
    fn reviewed_native_tool_plugin_is_composed_and_removed_exactly() {
        let root = tempfile::tempdir().unwrap();
        let bundle = tempfile::tempdir().unwrap();
        write_tool_bundle(bundle.path());
        install(root.path(), bundle.path(), "review-ticket-77", Vec::new()).unwrap();

        let authority = load_generation_authority(root.path()).unwrap();
        assert_eq!(authority.lock.value().instances.len(), 1);
        let instance_key = authority.lock.value().instances[0].instance_key.clone();
        let generation = crate::generation::resolve_initial_generation(PLAN, root.path()).unwrap();
        assert_eq!(
            generation
                .plan
                .module_instance(&instance_key)
                .unwrap()
                .package_id(),
            "lenso.agent.text-tools"
        );
        let binding = generation
            .plan
            .capability_bindings()
            .iter()
            .find(|binding| {
                binding.consumer_instance() == TOOL_AGGREGATOR_INSTANCE
                    && binding.provider_instance() == instance_key
            })
            .unwrap();
        assert_eq!(binding.provider_order(), 1);
        assert_eq!(generation.artifact_set.value().instances.len(), 1);

        remove(root.path(), "example.text-tools").unwrap();
        let generation = crate::generation::resolve_initial_generation(PLAN, root.path()).unwrap();
        let approved: ResolvedAppPlan = serde_json::from_slice(PLAN).unwrap();
        assert_eq!(generation.plan, approved);
        assert!(generation.artifact_set.value().instances.is_empty());
    }

    #[test]
    fn undeclared_bundle_file_and_tampered_active_authority_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let bundle = tempfile::tempdir().unwrap();
        write_passive_bundle(bundle.path());
        fs::write(bundle.path().join("undeclared.txt"), "not declared").unwrap();
        let error = install(
            root.path(),
            bundle.path(),
            "review",
            vec!["extras".to_owned()],
        )
        .unwrap_err();
        assert!(error.contains("undeclared file"));

        fs::remove_file(bundle.path().join("undeclared.txt")).unwrap();
        install(
            root.path(),
            bundle.path(),
            "review",
            vec!["extras".to_owned()],
        )
        .unwrap();
        let active_path = root.path().join(ACTIVE_SET_FILE);
        let mut active: serde_json::Value =
            serde_json::from_slice(&fs::read(&active_path).unwrap()).unwrap();
        active["lock"]["plugins"][0]["release_version"] = "9.9.9".into();
        fs::write(active_path, serde_json::to_vec(&active).unwrap()).unwrap();
        let error = load_generation_authority(root.path()).unwrap_err();
        assert!(error.contains("does not close its lock"));
        let error = remove(root.path(), "example.passive").unwrap_err();
        assert!(error.contains("does not close its lock"));
    }

    fn write_passive_bundle(root: &Path) -> CanonicalDocument<PluginManifest> {
        let artifact = b"extra";
        let metadata = b"{\"kind\":\"fixture\"}";
        let manifest = passive_manifest(artifact, metadata);
        write_bundle(root, &manifest, artifact, metadata);
        CanonicalDocument::from_value("lenso-plugin.json", manifest).unwrap()
    }

    fn write_tool_bundle(root: &Path) {
        let manifest = PluginManifest {
            schema_version: 1,
            plugin_id: "example.text-tools".to_owned(),
            release_version: "1.0.0".to_owned(),
            artifacts: Vec::new(),
            module_contributions: vec![ModuleContribution {
                id: "text-tools".to_owned(),
                package_id: "lenso.agent.text-tools".to_owned(),
                configuration_schema_digest: sha256_digest(EMPTY_CONFIGURATION_SCHEMA),
                provides: vec![CapabilityDeclaration {
                    capability_id: TOOL_PROVIDER_CAPABILITY_ID.to_owned(),
                    descriptor_version: TOOL_PROVIDER_DESCRIPTOR_VERSION.to_owned(),
                    descriptor_digest: sha256_digest(TOOL_PROVIDER_DESCRIPTOR),
                    request_operations: vec![
                        TOOL_PROVIDER_CATALOG_OPERATION.to_owned(),
                        TOOL_PROVIDER_EXECUTE_OPERATION.to_owned(),
                    ],
                }],
                requires: Vec::new(),
                implementations: vec![ImplementationVariant {
                    id: "native".to_owned(),
                    artifact: None,
                    built_in_factory: Some(TEXT_TOOLS_FACTORY_IDENTITY.to_owned()),
                    entrypoint: "default".to_owned(),
                    execution_class: NATIVE_EXECUTION_CLASS.to_owned(),
                    targets: vec![host_target()],
                    profiles: vec![NATIVE_TOOL_PROFILE.to_owned()],
                    support_channel: SupportChannel::Stable,
                    trust: TrustLevel::Trusted,
                }],
                permission_request_ids: Vec::new(),
                state: None,
            }],
            data_contributions: Vec::new(),
            permission_requests: Vec::new(),
            features: Vec::new(),
            binding_templates: Vec::new(),
            product_metadata: Vec::new(),
        };
        fs::write(
            root.join(MANIFEST_FILE),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
    }

    fn write_bundle(root: &Path, manifest: &PluginManifest, artifact: &[u8], metadata: &[u8]) {
        fs::write(
            root.join(MANIFEST_FILE),
            serde_json::to_vec(manifest).unwrap(),
        )
        .unwrap();
        fs::write(root.join("extra.bin"), artifact).unwrap();
        fs::write(root.join("extra.json"), metadata).unwrap();
    }

    fn passive_manifest(artifact: &[u8], metadata: &[u8]) -> PluginManifest {
        PluginManifest {
            schema_version: 1,
            plugin_id: "example.passive".to_owned(),
            release_version: "1.0.0".to_owned(),
            artifacts: vec![ArtifactDeclaration {
                id: "extra".to_owned(),
                kind: ArtifactKind::Data,
                digest: sha256_digest(artifact),
                size: u64::try_from(artifact.len()).unwrap(),
                media_type: "application/octet-stream".to_owned(),
                path: "extra.bin".to_owned(),
                targets: vec![host_target()],
            }],
            module_contributions: Vec::new(),
            data_contributions: Vec::new(),
            permission_requests: Vec::new(),
            features: vec![PluginFeature {
                id: "extras".to_owned(),
                module_contribution_ids: Vec::new(),
                data_contribution_ids: Vec::new(),
                artifact_ids: vec!["extra".to_owned()],
                permission_request_ids: Vec::new(),
                product_metadata_ids: vec!["extra-meta".to_owned()],
            }],
            binding_templates: Vec::new(),
            product_metadata: vec![ProductMetadataDeclaration {
                id: "extra-meta".to_owned(),
                namespace: "example.passive".to_owned(),
                schema_id: "example.passive.metadata@1".to_owned(),
                path: "extra.json".to_owned(),
                digest: sha256_digest(metadata),
            }],
        }
    }
}
