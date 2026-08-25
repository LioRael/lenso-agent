use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use lenso_app_plan::{CapabilityBinding, ModuleInstancePlan, ResolvedAppPlan};
use lenso_plugin_control_plane::{
    AdmissionPolicy, CanonicalDocument, ControlPlaneError, LockedInstance, LockedPlugin,
    PluginBundle, PluginManifest, PluginSetLock, PluginStore,
};
use serde::{Deserialize, Serialize};

use crate::{
    authority::AuthorityCoordinator,
    plugin_profiles::{PluginProfileCatalog, ResolvedAttachment, harness_plugin_profiles},
};

const APP_ID: &str = "lenso.agent.harness";
const ACTIVE_SET_FILE: &str = "active-set.json";
const ACTIVE_SET_DIRECTORY: &str = "active-sets";
const RECOVERY_AUTHORITY_DIRECTORY: &str = "generation-authorities";
const MANIFEST_FILE: &str = "lenso-plugin.json";
const LOCAL_REVIEW_PROVENANCE: &str = "local-review";
const LOCAL_REVIEW_POLICY: &str = "lenso.agent.local-review@1";
const MAX_BUNDLE_FILES: usize = 4_096;
const MAX_BUNDLE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_BUNDLE_DEPTH: usize = 32;
const MAX_EVIDENCE_BYTES: usize = 4_096;
#[cfg(test)]
const EMPTY_CONFIGURATION_SCHEMA: &[u8] = br#"{"additionalProperties":false,"type":"object"}"#;
#[cfg(test)]
const TOOL_PROVIDER_DESCRIPTOR: &[u8] =
    include_bytes!("../../../crates/lenso-capability-agent-tool-provider/capability.json");
#[cfg(test)]
const MODEL_DESCRIPTOR: &[u8] =
    include_bytes!("../../../crates/lenso-capability-agent-model/capability.json");
#[cfg(test)]
const FIXTURE_MODEL_CONFIGURATION_SCHEMA: &[u8] =
    include_bytes!("../../../crates/lenso-agent-model-fixture-module/config.schema.json");
#[cfg(test)]
const CODEX_MODEL_CONFIGURATION_SCHEMA: &[u8] = include_bytes!(
    "../../../crates/lenso-agent-model-openai-codex-direct-module/config.schema.json"
);
#[cfg(test)]
const CODEX_AUTH_CONFIGURATION_SCHEMA: &[u8] =
    include_bytes!("../../../crates/lenso-agent-auth-openai-codex-module/config.schema.json");
#[cfg(test)]
const CODEX_AUTH_DESCRIPTOR: &[u8] =
    include_bytes!("../../../crates/lenso-capability-agent-auth-openai-codex/capability.json");

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
    Upgrade {
        bundle: PathBuf,
        evidence: String,
        features: Vec<String>,
        expected_manifest: String,
        plan: PathBuf,
        root: PathBuf,
    },
    Rollback {
        to: String,
        plan: PathBuf,
        root: PathBuf,
    },
    Status {
        root: PathBuf,
    },
    History {
        root: PathBuf,
    },
    Inspect {
        active_set_digest: String,
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
        "upgrade" => parse_upgrade(&arguments[1..]),
        "rollback" => parse_rollback(&arguments[1..]),
        "status" => parse_status(&arguments[1..]),
        "history" => parse_history(&arguments[1..]),
        "inspect" => parse_inspect(&arguments[1..]),
        _ => Err(usage()),
    }
}

pub async fn run(command: PluginCommand) -> Result<(), String> {
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
        PluginCommand::Upgrade {
            bundle,
            evidence,
            features,
            expected_manifest,
            plan,
            root,
        } => {
            let outcome = upgrade(
                &root,
                &bundle,
                &evidence,
                features,
                &expected_manifest,
                &plan,
            )
            .await?;
            println!(
                "upgraded: {}@{}",
                outcome.plugin_id, outcome.release_version
            );
            println!("manifest: {}", outcome.manifest_digest);
            println!(
                "previous-active-set: {}",
                outcome.previous_active_set_digest
            );
            println!("active-set: {}", outcome.active_set_digest);
            println!("generation: {}", outcome.generation_spec_digest);
            Ok(())
        }
        PluginCommand::Rollback { to, plan, root } => {
            let outcome = rollback(&root, &to, &plan).await?;
            println!("rolled-back-to: {}", outcome.active_set);
            println!("previous-active-set: {}", outcome.previous_active_set);
            println!("generation: {}", outcome.generation_spec);
            Ok(())
        }
        PluginCommand::History { root } => print_active_set_history(&root),
        PluginCommand::Inspect {
            active_set_digest,
            root,
        } => print_active_set(&root, &active_set_digest),
    }
}

fn print_active_set_history(root: &Path) -> Result<(), String> {
    let (current, history) = active_set_history(root)?;
    for (digest, active) in history {
        let state = if digest == current {
            "current"
        } else {
            "retained"
        };
        let lock =
            CanonicalDocument::from_value("lenso-plugins.lock.json", active.value().lock.clone())
                .map_err(control_error)?;
        println!(
            "{state}: {digest} plugin-set={} releases={}",
            lock.digest(),
            active.value().releases.len()
        );
    }
    Ok(())
}

fn print_active_set(root: &Path, digest: &str) -> Result<(), String> {
    let (active, current) = active_set_by_digest(root, digest)?;
    let lock =
        CanonicalDocument::from_value("lenso-plugins.lock.json", active.value().lock.clone())
            .map_err(control_error)?;
    println!("active-set: {}", active.digest());
    println!("current: {current}");
    println!("plugin-set: {}", lock.digest());
    for release in &active.value().releases {
        let manifest = CanonicalDocument::from_value(MANIFEST_FILE, release.manifest.clone())
            .map_err(control_error)?;
        println!(
            "release: {}@{} manifest={} receipt={}",
            release.plugin_id,
            release.manifest.release_version,
            manifest.digest(),
            release.admission_receipt_digest
        );
    }
    for instance in &active.value().lock.instances {
        println!(
            "instance: {} plugin={} contribution={}",
            instance.instance_key, instance.plugin_id, instance.contribution_id
        );
    }
    Ok(())
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

fn parse_history(arguments: &[String]) -> Result<PluginCommand, String> {
    parse_root_only(arguments).map(|root| PluginCommand::History { root })
}

fn parse_inspect(arguments: &[String]) -> Result<PluginCommand, String> {
    let mut active_set_digest = None;
    let mut root = default_root();
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--active-set" => active_set_digest = Some(arguments.next().ok_or_else(usage)?.clone()),
            "--root" => root = PathBuf::from(arguments.next().ok_or_else(usage)?),
            _ => return Err(usage()),
        }
    }
    Ok(PluginCommand::Inspect {
        active_set_digest: active_set_digest.ok_or_else(usage)?,
        root,
    })
}

fn parse_root_only(arguments: &[String]) -> Result<PathBuf, String> {
    match arguments {
        [] => Ok(default_root()),
        [flag, root] if flag == "--root" => Ok(PathBuf::from(root)),
        _ => Err(usage()),
    }
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

fn parse_upgrade(arguments: &[String]) -> Result<PluginCommand, String> {
    let mut bundle = None;
    let mut evidence = None;
    let mut features = Vec::new();
    let mut expected_manifest = None;
    let mut plan = None;
    let mut root = default_root();
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--bundle" => bundle = Some(PathBuf::from(arguments.next().ok_or_else(usage)?)),
            "--evidence" => evidence = Some(arguments.next().ok_or_else(usage)?.clone()),
            "--feature" => features.push(arguments.next().ok_or_else(usage)?.clone()),
            "--expected-manifest" => {
                expected_manifest = Some(arguments.next().ok_or_else(usage)?.clone());
            }
            "--plan" => plan = Some(PathBuf::from(arguments.next().ok_or_else(usage)?)),
            "--root" => root = PathBuf::from(arguments.next().ok_or_else(usage)?),
            _ => return Err(usage()),
        }
    }
    Ok(PluginCommand::Upgrade {
        bundle: bundle.ok_or_else(usage)?,
        evidence: evidence.ok_or_else(usage)?,
        features,
        expected_manifest: expected_manifest.ok_or_else(usage)?,
        plan: plan.ok_or_else(usage)?,
        root,
    })
}

fn parse_rollback(arguments: &[String]) -> Result<PluginCommand, String> {
    let mut to = None;
    let mut plan = None;
    let mut root = default_root();
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--to" => to = Some(arguments.next().ok_or_else(usage)?.clone()),
            "--plan" => plan = Some(PathBuf::from(arguments.next().ok_or_else(usage)?)),
            "--root" => root = PathBuf::from(arguments.next().ok_or_else(usage)?),
            _ => return Err(usage()),
        }
    }
    Ok(PluginCommand::Rollback {
        to: to.ok_or_else(usage)?,
        plan: plan.ok_or_else(usage)?,
        root,
    })
}

fn usage() -> String {
    "usage: lenso-agent-cli plugins <install --bundle <directory> --evidence <review> [--feature <id>]... [--root <directory>]|upgrade --bundle <directory> --evidence <review> --expected-manifest <sha256:digest> --plan <path> [--feature <id>]... [--root <directory>]|rollback --to <sha256:active-set-digest> --plan <path> [--root <directory>]|remove --plugin <id> [--root <directory>]|status [--root <directory>]|history [--root <directory>]|inspect --active-set <sha256:digest> [--root <directory>]>".to_owned()
}

fn default_root() -> PathBuf {
    PathBuf::from(".lenso/plugins")
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActivePluginSet {
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

#[derive(Debug)]
pub(crate) struct GenerationComposition {
    pub(crate) base_instances: Vec<ModuleInstancePlan>,
    pub(crate) bindings: Vec<CapabilityBinding>,
    pub(crate) preserved_base_bindings: Vec<CapabilityBinding>,
}

#[derive(Debug, Eq, PartialEq)]
struct InstallOutcome {
    plugin_id: String,
    release_version: String,
    manifest_digest: String,
    receipt_digest: String,
    plugin_set_digest: String,
}

#[derive(Debug, Eq, PartialEq)]
struct UpgradeOutcome {
    plugin_id: String,
    release_version: String,
    manifest_digest: String,
    previous_active_set_digest: String,
    active_set_digest: String,
    generation_spec_digest: String,
}

#[derive(Debug, Eq, PartialEq)]
struct RollbackOutcome {
    previous_active_set: String,
    active_set: String,
    generation_spec: String,
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
    profiles: &'a PluginProfileCatalog,
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
        validate_supported_manifest(manifest, self.profiles)?;
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
    let profiles = harness_plugin_profiles()?;
    let coordinator = AuthorityCoordinator::prepare(root)?;
    let _fence = coordinator.transition()?;
    let store = PluginStore::open(root.join("store")).map_err(control_error)?;
    let mut active = validate_active_set(load_active_set(root)?, &store, &profiles)?.into_value();
    let bundle = load_bundle(bundle_root)?;
    let manifest = CanonicalDocument::<PluginManifest>::parse(MANIFEST_FILE, &bundle.manifest)
        .map_err(control_error)?;
    validate_supported_manifest(manifest.value(), &profiles).map_err(control_error)?;
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
            &LocalReviewPolicy {
                evidence,
                profiles: &profiles,
            },
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
        &profiles,
    )?);
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
    let active_document = validate_active_set(active, &store, &profiles)?;
    write_active_set(root, &active_document)?;
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
    let profiles = harness_plugin_profiles()?;
    let coordinator = AuthorityCoordinator::prepare(root)?;
    let _fence = coordinator.transition()?;
    let store = PluginStore::open(root.join("store")).map_err(control_error)?;
    let mut active = validate_active_set(load_active_set(root)?, &store, &profiles)?.into_value();
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
    let active = validate_active_set(active, &store, &profiles)?;
    write_active_set(root, &active)?;
    let lock =
        CanonicalDocument::from_value("lenso-plugins.lock.json", active.value().lock.clone())
            .map_err(control_error)?;
    Ok(lock.digest().to_owned())
}

async fn upgrade(
    root: &Path,
    bundle_root: &Path,
    evidence: &str,
    mut features: Vec<String>,
    expected_manifest: &str,
    plan_path: &Path,
) -> Result<UpgradeOutcome, String> {
    let profiles = harness_plugin_profiles()?;
    let coordinator = AuthorityCoordinator::prepare(root)?;
    let _fence = coordinator.transition()?;
    let store = PluginStore::open(root.join("store")).map_err(control_error)?;
    let current = validate_active_set(load_active_set(root)?, &store, &profiles)?;
    let bundle = load_bundle(bundle_root)?;
    let manifest = CanonicalDocument::<PluginManifest>::parse(MANIFEST_FILE, &bundle.manifest)
        .map_err(control_error)?;
    validate_supported_manifest(manifest.value(), &profiles).map_err(control_error)?;
    let existing = current
        .value()
        .lock
        .plugins
        .iter()
        .find(|plugin| plugin.plugin_id == manifest.value().plugin_id)
        .ok_or_else(|| {
            format!(
                "Plugin `{}` is not active; install its first Release instead",
                manifest.value().plugin_id
            )
        })?;
    if existing.manifest_digest != expected_manifest {
        return Err(format!(
            "Plugin `{}` Manifest compare-and-swap failed: expected `{expected_manifest}`, active `{}`",
            existing.plugin_id, existing.manifest_digest
        ));
    }
    if existing.manifest_digest == manifest.digest() {
        return Err(format!(
            "Plugin `{}` candidate is already active",
            existing.plugin_id
        ));
    }
    let feature_count = features.len();
    features.sort();
    features.dedup();
    if features.len() != feature_count {
        return Err("selected Plugin Features contain a duplicate".to_owned());
    }
    let receipt = store
        .admit(
            &PluginBundle::new(bundle.manifest, bundle.files, LOCAL_REVIEW_PROVENANCE),
            &LocalReviewPolicy {
                evidence,
                profiles: &profiles,
            },
        )
        .map_err(control_error)?;
    let candidate = replacement_active_set(
        &current,
        &manifest,
        receipt.digest(),
        features,
        &store,
        &profiles,
    )?;
    let plan = fs::read(plan_path)
        .map_err(|error| format!("failed to read {}: {error}", plan_path.display()))?;
    let current_authority = generation_authority_from_active(root, current.value().clone())?;
    let candidate_authority = generation_authority_from_active(root, candidate.value().clone())?;
    let generation_spec_digest = crate::generation::ready_check_maintenance_transition(
        &plan,
        current_authority,
        candidate_authority,
        root,
    )
    .await?;
    record_active_set(root, &current)?;
    record_active_set(root, &candidate)?;
    write_active_set(root, &candidate)?;
    Ok(UpgradeOutcome {
        plugin_id: manifest.value().plugin_id.clone(),
        release_version: manifest.value().release_version.clone(),
        manifest_digest: manifest.digest().to_owned(),
        previous_active_set_digest: current.digest().to_owned(),
        active_set_digest: candidate.digest().to_owned(),
        generation_spec_digest,
    })
}

fn replacement_active_set(
    current: &CanonicalDocument<ActivePluginSet>,
    manifest: &CanonicalDocument<PluginManifest>,
    receipt_digest: &str,
    features: Vec<String>,
    store: &PluginStore,
    profiles: &PluginProfileCatalog,
) -> Result<CanonicalDocument<ActivePluginSet>, String> {
    let selected = validate_selection(manifest.value(), &features).map_err(control_error)?;
    let mut candidate = current.value().clone();
    let locked = LockedPlugin {
        plugin_id: manifest.value().plugin_id.clone(),
        release_version: manifest.value().release_version.clone(),
        manifest_digest: manifest.digest().to_owned(),
        selected_features: features,
        product_metadata_digests: selected.product_metadata_digests,
    };
    candidate
        .lock
        .plugins
        .retain(|plugin| plugin.plugin_id != locked.plugin_id);
    candidate.lock.plugins.push(locked);
    candidate
        .lock
        .plugins
        .sort_by(|left, right| left.plugin_id.cmp(&right.plugin_id));
    candidate
        .lock
        .instances
        .retain(|instance| instance.plugin_id != manifest.value().plugin_id);
    candidate.lock.instances.extend(locked_instances(
        manifest.value(),
        &selected.module_contribution_ids,
        profiles,
    )?);
    candidate
        .lock
        .instances
        .sort_by(|left, right| left.instance_key.cmp(&right.instance_key));
    candidate
        .releases
        .retain(|release| release.plugin_id != manifest.value().plugin_id);
    candidate.releases.push(ActiveRelease {
        plugin_id: manifest.value().plugin_id.clone(),
        manifest: manifest.value().clone(),
        admission_receipt_digest: receipt_digest.to_owned(),
    });
    candidate
        .releases
        .sort_by(|left, right| left.plugin_id.cmp(&right.plugin_id));
    validate_active_set(candidate, store, profiles)
}

async fn rollback(root: &Path, to: &str, plan_path: &Path) -> Result<RollbackOutcome, String> {
    let profiles = harness_plugin_profiles()?;
    let coordinator = AuthorityCoordinator::prepare(root)?;
    let _fence = coordinator.transition()?;
    let store = PluginStore::open(root.join("store")).map_err(control_error)?;
    let current = validate_active_set(load_active_set(root)?, &store, &profiles)?;
    if current.digest() == to {
        return Err(format!("active Plugin Set `{to}` is already current"));
    }
    let target = validate_active_set(load_active_set_record(root, to)?, &store, &profiles)?;
    if target.digest() != to {
        return Err("rollback Plugin Set does not match its requested digest".to_owned());
    }
    let plan = fs::read(plan_path)
        .map_err(|error| format!("failed to read {}: {error}", plan_path.display()))?;
    let current_authority = generation_authority_from_active(root, current.value().clone())?;
    let target_authority = generation_authority_from_active(root, target.value().clone())?;
    let generation_spec_digest = crate::generation::ready_check_maintenance_transition(
        &plan,
        current_authority,
        target_authority,
        root,
    )
    .await?;
    record_active_set(root, &current)?;
    record_active_set(root, &target)?;
    write_active_set(root, &target)?;
    Ok(RollbackOutcome {
        previous_active_set: current.digest().to_owned(),
        active_set: target.digest().to_owned(),
        generation_spec: generation_spec_digest,
    })
}

pub(crate) fn load_generation_authority(root: &Path) -> Result<GenerationPluginAuthority, String> {
    let coordinator = AuthorityCoordinator::prepare(root)?;
    let _fence = coordinator.snapshot()?;
    load_generation_authority_unfenced(root)
}

pub(crate) fn record_current_generation_authority(root: &Path) -> Result<(), String> {
    let profiles = harness_plugin_profiles()?;
    let coordinator = AuthorityCoordinator::prepare(root)?;
    let _fence = coordinator.snapshot()?;
    let store = PluginStore::open(root.join("store")).map_err(control_error)?;
    let active = validate_active_set(load_active_set(root)?, &store, &profiles)?;
    record_active_set_in(root, RECOVERY_AUTHORITY_DIRECTORY, &active)
}

pub(crate) fn recovery_generation_authorities(
    root: &Path,
) -> Result<Vec<GenerationPluginAuthority>, String> {
    let profiles = harness_plugin_profiles()?;
    let coordinator = AuthorityCoordinator::prepare(root)?;
    let _fence = coordinator.snapshot()?;
    let store = PluginStore::open(root.join("store")).map_err(control_error)?;
    let directory = root.join(RECOVERY_AUTHORITY_DIRECTORY);
    let metadata = fs::symlink_metadata(&directory)
        .map_err(|error| format!("failed to inspect Generation recovery authority: {error}"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Generation recovery authority is not a regular directory".to_owned());
    }
    let mut entries = fs::read_dir(&directory)
        .map_err(|error| format!("failed to enumerate Generation recovery authority: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("failed to enumerate Generation recovery authority: {error}"))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    entries
        .into_iter()
        .map(|entry| {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|error| {
                format!("failed to inspect Generation recovery authority: {error}")
            })?;
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err("Generation recovery authority record is not a regular file".to_owned());
            }
            let bytes = fs::read(&path).map_err(|error| {
                format!("failed to read Generation recovery authority: {error}")
            })?;
            let active = CanonicalDocument::<ActivePluginSet>::parse("active-set.json", &bytes)
                .map_err(control_error)?;
            let expected =
                active_set_record_path_in(root, RECOVERY_AUTHORITY_DIRECTORY, active.digest())?;
            if path != expected {
                return Err(
                    "Generation recovery authority record is not content-addressed".to_owned(),
                );
            }
            let active = validate_active_set(active.into_value(), &store, &profiles)?;
            let authority_store = PluginStore::open(root.join("store")).map_err(control_error)?;
            generation_authority_from_document(authority_store, &active)
        })
        .collect()
}

fn load_generation_authority_unfenced(root: &Path) -> Result<GenerationPluginAuthority, String> {
    let profiles = harness_plugin_profiles()?;
    let store = PluginStore::open(root.join("store")).map_err(control_error)?;
    let active = load_active_set(root)?;
    let active = validate_active_set(active, &store, &profiles)?;
    generation_authority_from_document(store, &active)
}

fn generation_authority_from_active(
    root: &Path,
    active: ActivePluginSet,
) -> Result<GenerationPluginAuthority, String> {
    let profiles = harness_plugin_profiles()?;
    let store = PluginStore::open(root.join("store")).map_err(control_error)?;
    let active = validate_active_set(active, &store, &profiles)?;
    generation_authority_from_document(store, &active)
}

fn generation_authority_from_document(
    store: PluginStore,
    active: &CanonicalDocument<ActivePluginSet>,
) -> Result<GenerationPluginAuthority, String> {
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

pub(crate) fn generation_composition(
    authority: &GenerationPluginAuthority,
    base_plan: &ResolvedAppPlan,
) -> Result<GenerationComposition, String> {
    let profiles = harness_plugin_profiles()?;
    if authority.lock.value().instances.is_empty() {
        return Ok(GenerationComposition {
            base_instances: base_plan.module_instances().to_vec(),
            bindings: base_plan.capability_bindings().to_vec(),
            preserved_base_bindings: base_plan.capability_bindings().to_vec(),
        });
    }
    let mut base_instances = base_plan.module_instances().to_vec();
    let mut preserved_base_bindings = base_plan.capability_bindings().to_vec();
    let mut plugin_bindings = Vec::new();
    let mut displaced_instances = BTreeSet::new();
    let mut reconfigured_instances = BTreeSet::new();
    let target = host_target();
    for instance in &authority.lock.value().instances {
        let manifest = authority
            .manifests
            .get(&instance.plugin_id)
            .ok_or_else(|| {
                format!(
                    "Plugin Instance `{}` has no active Manifest",
                    instance.instance_key
                )
            })?;
        let contribution = manifest
            .value()
            .module_contributions
            .iter()
            .find(|contribution| contribution.id == instance.contribution_id)
            .ok_or_else(|| {
                format!(
                    "Plugin Instance `{}` has no Module contribution",
                    instance.instance_key
                )
            })?;
        match profiles.attachment_for(contribution, &target, &instance.instance_key, base_plan)? {
            ResolvedAttachment::AppendMany(binding) => plugin_bindings.push(binding),
            ResolvedAttachment::ReplaceOne {
                binding,
                displaced_provider_instance,
                base_configuration_replacements,
            } => {
                if !displaced_instances.insert(displaced_provider_instance.clone()) {
                    return Err(format!(
                        "more than one Plugin replacement targets Instance `{displaced_provider_instance}`"
                    ));
                }
                base_instances
                    .retain(|candidate| candidate.instance_key() != displaced_provider_instance);
                preserved_base_bindings.retain(|candidate| {
                    candidate.consumer_instance() != displaced_provider_instance
                        && candidate.provider_instance() != displaced_provider_instance
                });
                for replacement in base_configuration_replacements {
                    if !reconfigured_instances.insert(replacement.instance_key.clone()) {
                        return Err(format!(
                            "more than one Plugin replacement configures Instance `{}`",
                            replacement.instance_key
                        ));
                    }
                    let candidate = base_instances
                        .iter_mut()
                        .find(|candidate| candidate.instance_key() == replacement.instance_key)
                        .ok_or_else(|| {
                            format!(
                                "Plugin replacement configuration target `{}` is absent",
                                replacement.instance_key
                            )
                        })?;
                    if candidate.package_id() != replacement.allowed_package
                        || candidate.configuration() != replacement.expected_configuration
                    {
                        return Err(format!(
                            "Plugin replacement cannot configure base Instance `{}`",
                            replacement.instance_key
                        ));
                    }
                    *candidate = candidate
                        .clone()
                        .with_configuration(replacement.replacement_configuration);
                }
                plugin_bindings.push(binding);
            }
            ResolvedAttachment::IntraPluginOnly => {}
        }
    }
    plugin_bindings.extend(intra_plugin_bindings(authority)?);
    let mut bindings = preserved_base_bindings.clone();
    bindings.extend(plugin_bindings);
    Ok(GenerationComposition {
        base_instances,
        bindings,
        preserved_base_bindings,
    })
}

fn intra_plugin_bindings(
    authority: &GenerationPluginAuthority,
) -> Result<Vec<CapabilityBinding>, String> {
    let mut bindings = Vec::new();
    for (plugin_id, manifest) in &authority.manifests {
        for template in &manifest.value().binding_templates {
            let consumer = authority.lock.value().instances.iter().find(|instance| {
                instance.plugin_id == *plugin_id
                    && instance.contribution_id == template.consumer_contribution_id
            });
            let provider = authority.lock.value().instances.iter().find(|instance| {
                instance.plugin_id == *plugin_id
                    && instance.contribution_id == template.provider_contribution_id
            });
            if let (Some(consumer), Some(provider)) = (consumer, provider) {
                bindings.push(PluginProfileCatalog::binding_for_template(
                    manifest.value(),
                    template,
                    &consumer.instance_key,
                    &provider.instance_key,
                )?);
            }
        }
    }
    Ok(bindings)
}

fn validate_active_set(
    active: ActivePluginSet,
    store: &PluginStore,
    profiles: &PluginProfileCatalog,
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
        validate_supported_manifest(manifest.value(), profiles).map_err(control_error)?;
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
            profiles,
        )?);
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

fn validate_supported_manifest(
    manifest: &PluginManifest,
    profiles: &PluginProfileCatalog,
) -> Result<(), ControlPlaneError> {
    if !manifest.data_contributions.is_empty() || !manifest.permission_requests.is_empty() {
        return rejected("this Host rejects Plugin Data mounts and permission requests");
    }
    let target = host_target();
    profiles.validate_manifest_topology(manifest, &target)
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
    for contribution in manifest
        .module_contributions
        .iter()
        .filter(|contribution| module_contribution_ids.contains(&contribution.id))
    {
        for requirement in &contribution.requires {
            let provider_is_selected = manifest.binding_templates.iter().any(|template| {
                template.consumer_contribution_id == contribution.id
                    && template.capability_id == requirement.capability_id
                    && module_contribution_ids.contains(&template.provider_contribution_id)
            });
            if !provider_is_selected {
                return rejected(format!(
                    "selected Plugin contribution `{}` is missing provider `{}`",
                    contribution.id, requirement.capability_id
                ));
            }
        }
    }
    Ok(SelectedContent {
        module_contribution_ids: module_contribution_ids.into_iter().collect(),
        product_metadata_digests: metadata_digests.into_iter().collect(),
    })
}

fn locked_instances(
    manifest: &PluginManifest,
    selected_contributions: &[String],
    profiles: &PluginProfileCatalog,
) -> Result<Vec<LockedInstance>, String> {
    let target = host_target();
    selected_contributions
        .iter()
        .map(|contribution_id| {
            let contribution = manifest
                .module_contributions
                .iter()
                .find(|contribution| &contribution.id == contribution_id)
                .ok_or_else(|| {
                    format!("selected Module contribution `{contribution_id}` is absent")
                })?;
            let configuration = profiles
                .configuration_for(contribution, &target)
                .map_err(control_error)?;
            Ok(LockedInstance {
                plugin_id: manifest.plugin_id.clone(),
                contribution_id: contribution_id.clone(),
                instance_key: format!(
                    "plugin:{}:{}:{contribution_id}",
                    manifest.plugin_id.len(),
                    manifest.plugin_id
                ),
                implementation_variant: None,
                configuration,
                execution_lane: "main".to_owned(),
            })
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

fn active_set_record_path(root: &Path, digest: &str) -> Result<PathBuf, String> {
    active_set_record_path_in(root, ACTIVE_SET_DIRECTORY, digest)
}

fn active_set_record_path_in(
    root: &Path,
    directory: &str,
    digest: &str,
) -> Result<PathBuf, String> {
    let digest = digest
        .strip_prefix("sha256:")
        .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| "Active Set digest is not canonical SHA-256".to_owned())?;
    Ok(root.join(directory).join(format!("{digest}.json")))
}

fn load_active_set_record(root: &Path, digest: &str) -> Result<ActivePluginSet, String> {
    let path = active_set_record_path(root, digest)?;
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| format!("failed to inspect rollback Plugin Set: {error}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("rollback Plugin Set is not a regular file".to_owned());
    }
    let bytes =
        fs::read(&path).map_err(|error| format!("failed to read rollback Plugin Set: {error}"))?;
    let active = CanonicalDocument::<ActivePluginSet>::parse("active-set.json", &bytes)
        .map_err(control_error)?;
    if active.digest() != digest {
        return Err("rollback Plugin Set record does not match its digest".to_owned());
    }
    Ok(active.into_value())
}

fn active_set_by_digest(
    root: &Path,
    digest: &str,
) -> Result<(CanonicalDocument<ActivePluginSet>, bool), String> {
    let profiles = harness_plugin_profiles()?;
    let coordinator = AuthorityCoordinator::open_existing(root)?;
    let _fence = coordinator.snapshot()?;
    let store = open_existing_store(root)?;
    let current = validate_active_set(load_active_set(root)?, &store, &profiles)?;
    if current.digest() == digest {
        return Ok((current, true));
    }
    let retained = validate_active_set(load_active_set_record(root, digest)?, &store, &profiles)?;
    if retained.digest() != digest {
        return Err("retained Active Set does not match its requested digest".to_owned());
    }
    Ok((retained, false))
}

fn active_set_history(
    root: &Path,
) -> Result<(String, BTreeMap<String, CanonicalDocument<ActivePluginSet>>), String> {
    let profiles = harness_plugin_profiles()?;
    let coordinator = AuthorityCoordinator::open_existing(root)?;
    let _fence = coordinator.snapshot()?;
    let store = open_existing_store(root)?;
    let current = validate_active_set(load_active_set(root)?, &store, &profiles)?;
    let current_digest = current.digest().to_owned();
    let mut history = BTreeMap::from([(current_digest.clone(), current)]);
    let directory = root.join(ACTIVE_SET_DIRECTORY);
    let metadata = match fs::symlink_metadata(&directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((current_digest, history));
        }
        Err(error) => return Err(format!("failed to inspect Active Set history: {error}")),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Active Set history is not a regular directory".to_owned());
    }
    let mut entries = fs::read_dir(&directory)
        .map_err(|error| format!("failed to enumerate Active Set history: {error}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("failed to enumerate Active Set history: {error}"))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| "Active Set history contains a non-UTF-8 name".to_owned())?;
        if name.starts_with('.')
            && Path::new(&name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("tmp"))
        {
            continue;
        }
        let hash = name
            .strip_suffix(".json")
            .filter(|hash| {
                hash.len() == 64
                    && hash
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
            .ok_or_else(|| format!("Active Set history entry `{name}` is not content-addressed"))?;
        let digest = format!("sha256:{hash}");
        let active =
            validate_active_set(load_active_set_record(root, &digest)?, &store, &profiles)?;
        history.insert(digest, active);
    }
    Ok((current_digest, history))
}

pub(crate) fn retained_plugin_set_digests(root: &Path) -> Result<BTreeSet<String>, String> {
    let (_, history) = active_set_history(root)?;
    history
        .into_values()
        .map(|active| {
            CanonicalDocument::from_value("lenso-plugins.lock.json", active.value().lock.clone())
                .map(|lock| lock.digest().to_owned())
                .map_err(control_error)
        })
        .collect()
}

fn open_existing_store(root: &Path) -> Result<PluginStore, String> {
    for directory in [
        root.to_path_buf(),
        root.join("store"),
        root.join("store/objects"),
        root.join("store/manifests"),
        root.join("store/receipts"),
    ] {
        let metadata = fs::symlink_metadata(&directory).map_err(|error| {
            format!(
                "failed to inspect Plugin authority directory {}: {error}",
                directory.display()
            )
        })?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(format!(
                "Plugin authority path {} is not a regular directory",
                directory.display()
            ));
        }
    }
    PluginStore::open(root.join("store")).map_err(control_error)
}

fn record_active_set(
    root: &Path,
    active: &CanonicalDocument<ActivePluginSet>,
) -> Result<(), String> {
    record_active_set_in(root, ACTIVE_SET_DIRECTORY, active)
}

fn record_active_set_in(
    root: &Path,
    directory_name: &str,
    active: &CanonicalDocument<ActivePluginSet>,
) -> Result<(), String> {
    let directory = root.join(directory_name);
    fs::create_dir_all(&directory)
        .map_err(|error| format!("failed to create Active Set history: {error}"))?;
    let metadata = fs::symlink_metadata(&directory)
        .map_err(|error| format!("failed to inspect Active Set history: {error}"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Active Set history is not a regular directory".to_owned());
    }
    let destination = active_set_record_path_in(root, directory_name, active.digest())?;
    match fs::symlink_metadata(&destination) {
        Ok(metadata) => {
            if !metadata.is_file() || metadata.file_type().is_symlink() {
                return Err("Active Set history record is not a regular file".to_owned());
            }
            let existing = fs::read(&destination)
                .map_err(|error| format!("failed to read Active Set history: {error}"))?;
            if existing != active.bytes() {
                return Err("Active Set history record does not match its digest".to_owned());
            }
            return Ok(());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("failed to inspect Active Set history: {error}")),
    }
    let temporary = directory.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| format!("failed to create Active Set history: {error}"))?;
        file.write_all(active.bytes())
            .map_err(|error| format!("failed to write Active Set history: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("failed to sync Active Set history: {error}"))?;
        fs::rename(&temporary, &destination)
            .map_err(|error| format!("failed to commit Active Set history: {error}"))?;
        File::open(&directory)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("failed to sync Active Set history directory: {error}"))
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
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
    use lenso_agent_auth_openai_codex_module::{
        FACTORY_IDENTITY as CODEX_AUTH_FACTORY_IDENTITY, PACKAGE_ID as CODEX_AUTH_PACKAGE_ID,
    };
    use lenso_agent_model_fixture_module::{
        FACTORY_IDENTITY as FIXTURE_MODEL_FACTORY_IDENTITY, MODEL_ID as FIXTURE_MODEL_ID,
        PACKAGE_ID as FIXTURE_MODEL_PACKAGE_ID,
    };
    use lenso_agent_model_openai_codex_direct_module::{
        FACTORY_IDENTITY as CODEX_MODEL_FACTORY_IDENTITY, PACKAGE_ID as CODEX_MODEL_PACKAGE_ID,
    };
    use lenso_agent_text_tools_module::FACTORY_IDENTITY as TEXT_TOOLS_FACTORY_IDENTITY;
    use lenso_app_plan::{CapabilityOperationKind, ResolvedAppPlan};
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
    use lenso_plugin_control_plane::{
        ArtifactDeclaration, ArtifactKind, BindingTemplate, CapabilityDeclaration,
        CapabilityRequirement, ImplementationVariant, ModuleContribution, PermissionRequest,
        PluginFeature, ProductMetadataDeclaration, RequirementCardinality, SupportChannel,
        TrustLevel, sha256_digest,
    };

    use crate::plugin_profiles::{
        NATIVE_AUTH_PROFILE, NATIVE_EXECUTION_CLASS, NATIVE_MODEL_PROFILE, NATIVE_TOOL_PROFILE,
    };

    const PLAN: &[u8] = include_bytes!("../../../composition/headless-readonly/resolved-plan.json");
    const OPENAI_PLAN: &[u8] =
        include_bytes!("../../../composition/openai-codex-direct/resolved-plan.json");

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
        assert!(error.contains("does not match a registered Plugin profile"));
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
        let mut one_plan_value: serde_json::Value = serde_json::from_slice(PLAN).unwrap();
        let tools = one_plan_value["module_instances"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|instance| instance["instance_key"] == "tools")
            .unwrap();
        tools["required_capabilities"][0]["cardinality"] = "one".into();
        let one_plan: ResolvedAppPlan = serde_json::from_value(one_plan_value).unwrap();
        one_plan.validate().unwrap();
        let error = generation_composition(&authority, &one_plan).unwrap_err();
        assert!(error.contains("as a Many Capability"));

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
                binding.consumer_instance() == "tools"
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
    fn reviewed_fixture_model_plugin_replaces_one_provider_and_removal_restores_base_plan() {
        let root = tempfile::tempdir().unwrap();
        let bundle = tempfile::tempdir().unwrap();
        write_model_bundle(bundle.path(), "example.fixture-model");
        install(root.path(), bundle.path(), "review-ticket-88", Vec::new()).unwrap();

        let authority = load_generation_authority(root.path()).unwrap();
        let instance_key = authority.lock.value().instances[0].instance_key.clone();
        let composition =
            generation_composition(&authority, &serde_json::from_slice(PLAN).unwrap()).unwrap();
        assert!(
            composition
                .base_instances
                .iter()
                .all(|instance| instance.instance_key() != "model")
        );
        assert!(composition.bindings.iter().all(|binding| {
            binding.consumer_instance() != "model" && binding.provider_instance() != "model"
        }));

        let generation = crate::generation::resolve_initial_generation(PLAN, root.path()).unwrap();
        assert!(generation.plan.module_instance("model").is_none());
        let replacement = generation.plan.module_instance(&instance_key).unwrap();
        assert_eq!(replacement.package_id(), FIXTURE_MODEL_PACKAGE_ID);
        assert_eq!(
            replacement.configuration(),
            format!(r#"{{"model":"{FIXTURE_MODEL_ID}"}}"#)
        );
        assert_eq!(
            replacement.provided_capabilities()[0].stream_operations(),
            [MODEL_COMPLETE_OPERATION]
        );
        let binding = generation
            .plan
            .capability_bindings()
            .iter()
            .find(|binding| {
                binding.consumer_instance() == "agent"
                    && binding.capability_id() == MODEL_CAPABILITY_ID
            })
            .unwrap();
        assert_eq!(binding.provider_instance(), instance_key);
        assert_eq!(binding.provider_order(), 0);

        remove(root.path(), "example.fixture-model").unwrap();
        let generation = crate::generation::resolve_initial_generation(PLAN, root.path()).unwrap();
        let approved: ResolvedAppPlan = serde_json::from_slice(PLAN).unwrap();
        assert_eq!(generation.plan, approved);
    }

    #[test]
    fn fixture_model_replacement_is_restricted_to_the_fixture_base_profile() {
        let root = tempfile::tempdir().unwrap();
        let bundle = tempfile::tempdir().unwrap();
        write_model_bundle(bundle.path(), "example.fixture-model");
        install(root.path(), bundle.path(), "review-ticket-89", Vec::new()).unwrap();

        let error =
            crate::generation::resolve_initial_generation(OPENAI_PLAN, root.path()).unwrap_err();
        assert!(error.contains("cannot displace package `lenso.agent.model.openai-codex-direct`"));
    }

    #[test]
    fn two_fixture_model_replacements_for_one_base_instance_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        write_model_bundle(first.path(), "example.fixture-model-a");
        write_model_bundle(second.path(), "example.fixture-model-b");
        install(root.path(), first.path(), "review-ticket-90", Vec::new()).unwrap();
        install(root.path(), second.path(), "review-ticket-91", Vec::new()).unwrap();

        let error = crate::generation::resolve_initial_generation(PLAN, root.path()).unwrap_err();
        assert!(error.contains("more than one Plugin replacement targets Instance `model`"));
    }

    #[test]
    fn reviewed_codex_plugin_closes_model_auth_and_agent_configuration_atomically() {
        let root = tempfile::tempdir().unwrap();
        let bundle = tempfile::tempdir().unwrap();
        write_codex_bundle(bundle.path());
        install(root.path(), bundle.path(), "review-ticket-92", Vec::new()).unwrap();

        let authority = load_generation_authority(root.path()).unwrap();
        assert_eq!(authority.lock.value().instances.len(), 2);
        let model_key = authority
            .lock
            .value()
            .instances
            .iter()
            .find(|instance| instance.contribution_id == "codex-model")
            .unwrap()
            .instance_key
            .clone();
        let auth_key = authority
            .lock
            .value()
            .instances
            .iter()
            .find(|instance| instance.contribution_id == "codex-auth")
            .unwrap()
            .instance_key
            .clone();

        let mut incompatible_base: serde_json::Value = serde_json::from_slice(PLAN).unwrap();
        let agent = incompatible_base["module_instances"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|instance| instance["instance_key"] == "agent")
            .unwrap();
        let mut configuration: serde_json::Value =
            serde_json::from_str(agent["configuration"].as_str().unwrap()).unwrap();
        configuration["max_steps"] = 9.into();
        agent["configuration"] = configuration.to_string().into();
        let incompatible_base: ResolvedAppPlan = serde_json::from_value(incompatible_base).unwrap();
        incompatible_base.validate().unwrap();
        let error = generation_composition(&authority, &incompatible_base).unwrap_err();
        assert!(error.contains("cannot configure base Instance `agent`"));

        let generation = crate::generation::resolve_initial_generation(PLAN, root.path()).unwrap();
        assert!(generation.plan.module_instance("model").is_none());
        assert_eq!(
            generation
                .plan
                .module_instance("agent")
                .unwrap()
                .configuration(),
            r#"{"max_history_events":200,"max_output_tokens":1024,"max_steps":8,"max_tool_calls":4,"model":"gpt-5.6-luna"}"#
        );
        assert_eq!(
            generation
                .plan
                .module_instance(&model_key)
                .unwrap()
                .package_id(),
            CODEX_MODEL_PACKAGE_ID
        );
        assert_eq!(
            generation
                .plan
                .module_instance(&model_key)
                .unwrap()
                .configuration(),
            r#"{"base_url":"https://chatgpt.com/backend-api","max_event_bytes":1048576,"model":"gpt-5.6-luna","reasoning_effort":"medium"}"#
        );
        assert_eq!(
            generation
                .plan
                .module_instance(&auth_key)
                .unwrap()
                .package_id(),
            CODEX_AUTH_PACKAGE_ID
        );
        assert_eq!(
            generation
                .plan
                .module_instance(&auth_key)
                .unwrap()
                .configuration(),
            r#"{"issuer":"https://auth.openai.com","profile":"default","refresh_margin_seconds":60}"#
        );
        assert!(generation.plan.capability_bindings().iter().any(|binding| {
            binding.consumer_instance() == "agent"
                && binding.provider_instance() == model_key
                && binding.capability_id() == MODEL_CAPABILITY_ID
        }));
        assert!(generation.plan.capability_bindings().iter().any(|binding| {
            binding.consumer_instance() == model_key
                && binding.provider_instance() == auth_key
                && binding.capability_id() == CODEX_AUTH_CAPABILITY_ID
        }));
        assert_eq!(generation.artifact_set.value().instances.len(), 2);

        remove(root.path(), "example.codex-direct").unwrap();
        let generation = crate::generation::resolve_initial_generation(PLAN, root.path()).unwrap();
        let approved: ResolvedAppPlan = serde_json::from_slice(PLAN).unwrap();
        assert_eq!(generation.plan, approved);
    }

    #[test]
    fn codex_plugin_rejects_an_incomplete_or_incompatible_dependency_template() {
        let root = tempfile::tempdir().unwrap();
        let bundle = tempfile::tempdir().unwrap();
        write_codex_bundle(bundle.path());
        let manifest_path = bundle.path().join(MANIFEST_FILE);
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["binding_templates"] = serde_json::json!([]);
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let error =
            install(root.path(), bundle.path(), "review-ticket-93", Vec::new()).unwrap_err();
        assert!(error.contains("must be consumed exactly once"));

        write_codex_bundle(bundle.path());
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["binding_templates"][0]["provider_contribution_id"] = "codex-model".into();
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let error =
            install(root.path(), bundle.path(), "review-ticket-94", Vec::new()).unwrap_err();
        assert!(error.contains("is incompatible"));

        write_codex_bundle(bundle.path());
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["features"] = serde_json::json!([{
            "id": "auth",
            "module_contribution_ids": ["codex-auth"],
            "data_contribution_ids": [],
            "artifact_ids": [],
            "permission_request_ids": [],
            "product_metadata_ids": []
        }]);
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let error =
            install(root.path(), bundle.path(), "review-ticket-95", Vec::new()).unwrap_err();
        assert!(error.contains("is missing provider"));

        write_codex_bundle(bundle.path());
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["module_contributions"] =
            serde_json::json!([manifest["module_contributions"][0].clone()]);
        manifest["binding_templates"] = serde_json::json!([]);
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let error =
            install(root.path(), bundle.path(), "review-ticket-96", Vec::new()).unwrap_err();
        assert!(error.contains("must be consumed exactly once"));
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
                    operation_kinds: BTreeMap::new(),
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

    fn write_model_bundle(root: &Path, plugin_id: &str) {
        let manifest = PluginManifest {
            schema_version: 1,
            plugin_id: plugin_id.to_owned(),
            release_version: "1.0.0".to_owned(),
            artifacts: Vec::new(),
            module_contributions: vec![ModuleContribution {
                id: "fixture-model".to_owned(),
                package_id: FIXTURE_MODEL_PACKAGE_ID.to_owned(),
                configuration_schema_digest: sha256_digest(FIXTURE_MODEL_CONFIGURATION_SCHEMA),
                provides: vec![CapabilityDeclaration {
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
                implementations: vec![ImplementationVariant {
                    id: "native".to_owned(),
                    artifact: None,
                    built_in_factory: Some(FIXTURE_MODEL_FACTORY_IDENTITY.to_owned()),
                    entrypoint: "default".to_owned(),
                    execution_class: NATIVE_EXECUTION_CLASS.to_owned(),
                    targets: vec![host_target()],
                    profiles: vec![NATIVE_MODEL_PROFILE.to_owned()],
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

    fn write_codex_bundle(root: &Path) {
        let manifest = PluginManifest {
            schema_version: 1,
            plugin_id: "example.codex-direct".to_owned(),
            release_version: "1.0.0".to_owned(),
            artifacts: Vec::new(),
            module_contributions: vec![
                ModuleContribution {
                    id: "codex-auth".to_owned(),
                    package_id: CODEX_AUTH_PACKAGE_ID.to_owned(),
                    configuration_schema_digest: sha256_digest(CODEX_AUTH_CONFIGURATION_SCHEMA),
                    provides: vec![CapabilityDeclaration {
                        capability_id: CODEX_AUTH_CAPABILITY_ID.to_owned(),
                        descriptor_version: CODEX_AUTH_DESCRIPTOR_VERSION.to_owned(),
                        descriptor_digest: sha256_digest(CODEX_AUTH_DESCRIPTOR),
                        request_operations: vec![CODEX_AUTH_ACCESS_OPERATION.to_owned()],
                        operation_kinds: BTreeMap::new(),
                    }],
                    requires: Vec::new(),
                    implementations: vec![ImplementationVariant {
                        id: "native".to_owned(),
                        artifact: None,
                        built_in_factory: Some(CODEX_AUTH_FACTORY_IDENTITY.to_owned()),
                        entrypoint: "default".to_owned(),
                        execution_class: NATIVE_EXECUTION_CLASS.to_owned(),
                        targets: vec![host_target()],
                        profiles: vec![NATIVE_AUTH_PROFILE.to_owned()],
                        support_channel: SupportChannel::Experimental,
                        trust: TrustLevel::Trusted,
                    }],
                    permission_request_ids: Vec::new(),
                    state: None,
                },
                ModuleContribution {
                    id: "codex-model".to_owned(),
                    package_id: CODEX_MODEL_PACKAGE_ID.to_owned(),
                    configuration_schema_digest: sha256_digest(CODEX_MODEL_CONFIGURATION_SCHEMA),
                    provides: vec![CapabilityDeclaration {
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
                    implementations: vec![ImplementationVariant {
                        id: "native".to_owned(),
                        artifact: None,
                        built_in_factory: Some(CODEX_MODEL_FACTORY_IDENTITY.to_owned()),
                        entrypoint: "default".to_owned(),
                        execution_class: NATIVE_EXECUTION_CLASS.to_owned(),
                        targets: vec![host_target()],
                        profiles: vec![NATIVE_MODEL_PROFILE.to_owned()],
                        support_channel: SupportChannel::Experimental,
                        trust: TrustLevel::Trusted,
                    }],
                    permission_request_ids: Vec::new(),
                    state: None,
                },
            ],
            data_contributions: Vec::new(),
            permission_requests: Vec::new(),
            features: Vec::new(),
            binding_templates: vec![BindingTemplate {
                consumer_contribution_id: "codex-model".to_owned(),
                provider_contribution_id: "codex-auth".to_owned(),
                capability_id: CODEX_AUTH_CAPABILITY_ID.to_owned(),
            }],
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
