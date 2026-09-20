//! Blank, product-neutral authoring support for a Lenso Agent composition.
//!
//! This crate deliberately provides no Plugin release, default Plugin, model,
//! Loop, Tool, memory, system prompt, or Surface. It only publishes the
//! generic Host Catalog used to inspect an explicitly assembled Plugin Root.

use std::{
    fs,
    path::{Path, PathBuf},
};

use lenso_app_plan::{
    ResolvedAppPlan,
    authoring::{
        HostCatalog, HostSlot, PluginInstanceSource, PluginRootSnapshot, ResolvedApp,
        resolve_plugin_root,
    },
};
use serde::Serialize;

/// Stable JSON schema used by [`FoundationCheck`].
pub const FOUNDATION_CHECK_SCHEMA: &str = "lenso.agent.foundation-check.v1";

/// Relative path for the generic Foundation Host Catalog.
pub const FOUNDATION_HOST_CATALOG_PATH: &str = ".lenso/host-catalog.json";

/// Returns the product-neutral attachment policy for an Agent Foundation.
///
/// Each slot is intentionally `many`: the Foundation neither selects a
/// product implementation nor declares an execution policy for one. A
/// concrete Host may impose stronger cardinality and binding policy when it
/// explicitly chooses to run an Agent composition.
#[must_use]
pub fn foundation_host_catalog() -> HostCatalog {
    HostCatalog::new(foundation_host_slots(), [], [])
}

/// Creates a new Foundation root without replacing another Host's authority.
///
/// The operation creates the visible `plugins/` directory and writes only the
/// empty-default Foundation Host Catalog. An existing different catalog fails
/// rather than being replaced.
pub fn initialize(root: &Path) -> Result<PathBuf, String> {
    ensure_root_directory(root)?;
    fs::create_dir_all(root.join("plugins")).map_err(|error| {
        format!(
            "failed to create Foundation Plugin Root {}: {error}",
            root.join("plugins").display()
        )
    })?;

    let catalog_path = root.join(FOUNDATION_HOST_CATALOG_PATH);
    let expected = catalog_json_bytes()?;
    match fs::read(&catalog_path) {
        Ok(existing) => {
            if catalog_matches_foundation(&existing)? {
                return Ok(catalog_path);
            }
            return Err(format!(
                "refusing to replace Host authority at {}; initialize a fresh Foundation root instead",
                catalog_path.display()
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(format!(
                "failed to read Foundation Host Catalog {}: {error}",
                catalog_path.display()
            ));
        }
    }

    let parent = catalog_path
        .parent()
        .ok_or_else(|| "Foundation Host Catalog has no parent directory".to_owned())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    let temporary = parent.join(format!("host-catalog.{}.tmp", std::process::id()));
    fs::write(&temporary, expected)
        .map_err(|error| format!("failed to write {}: {error}", temporary.display()))?;
    fs::rename(&temporary, &catalog_path)
        .map_err(|error| format!("failed to publish {}: {error}", catalog_path.display()))?;
    Ok(catalog_path)
}

/// Resolves a Foundation root through the public Plugin Root authoring path.
///
/// A successful result means only that the explicitly selected Plugin Root is
/// structurally resolvable under the blank Foundation Catalog. It never means
/// that a model, Loop, Surface, or external operation was selected or started.
pub fn check(root: &Path) -> Result<FoundationCheck, String> {
    ensure_foundation_catalog(root)?;
    let resolved = lenso_app_authoring::load_resolved_app(root)
        .map_err(|error| format!("Foundation Plugin Root cannot resolve: {error}"))?;
    Ok(check_resolved_app(&resolved))
}

/// Resolves an in-memory Plugin Root against the blank Foundation Catalog.
///
/// This is useful to embedding authors that already own their Plugin Root
/// authority. It does not read or write a filesystem root.
pub fn check_snapshot(snapshot: &PluginRootSnapshot) -> Result<FoundationCheck, String> {
    let resolved = resolve_plugin_root(&foundation_host_catalog(), snapshot)
        .map_err(|error| format!("Foundation Plugin Root cannot resolve: {error}"))?;
    Ok(check_resolved_app(&resolved))
}

/// Read-only result of resolving one explicit Foundation Plugin Root.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FoundationCheck {
    /// Versioned shape of this diagnostic output.
    pub schema: &'static str,
    /// Whether no Plugin has been selected or a composition was resolved.
    pub status: FoundationStatus,
    /// Exact App-local Plugin instance identities in deterministic Plan order.
    pub plugin_instances: Vec<String>,
    /// Exact capability binding identities in deterministic Plan order.
    pub capability_bindings: Vec<String>,
    /// Resolved Plugin provenance, package revision, and execution selection facts.
    pub plugin_sources: Vec<FoundationPluginSource>,
    /// Resolved Capability selection facts without exposing credentials or configuration.
    pub capability_selections: Vec<FoundationCapabilitySelection>,
    /// Authority deliberately withheld by the blank Foundation.
    pub authorization_boundary: FoundationAuthorizationBoundary,
    /// Diagnostics that explain the next explicit authoring step.
    pub diagnostics: Vec<FoundationDiagnostic>,
}

/// Product-neutral state of a Foundation check.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FoundationStatus {
    /// No Plugin was selected, so the Foundation intentionally has no behavior.
    Blank,
    /// One or more explicitly selected Plugins resolved into an App Plan.
    Composed,
}

/// Why a Plugin Instance entered the immutable resolved Plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FoundationPluginSelectionSource {
    /// The execution Host selected the instance as one of its defaults.
    HostDefault,
    /// The Host default remains selected, with Root-owned configuration applied.
    HostDefaultConfiguredByRoot,
    /// The visible Plugin Root explicitly selected the instance.
    PluginRoot,
    /// Resolver provenance was unavailable, so no selection source is inferred.
    Unknown,
}

/// Source and runtime facts for one exact resolved Plugin Instance.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FoundationPluginSource {
    /// Exact App-local instance identity.
    pub instance: String,
    /// Plugin release identity that owns the source selection.
    pub plugin_id: String,
    /// Resolver provenance for this selected instance.
    pub selection_source: FoundationPluginSelectionSource,
    /// Short explanation of the resolver provenance.
    pub selection_reason: &'static str,
    /// Runtime package selected for this instance.
    pub package_id: String,
    /// Exact package-manager revision selected before boot, if the source declared one.
    pub package_revision: String,
    /// Runtime entrypoint selected for the instance.
    pub entrypoint: String,
    /// Opaque Adapter runtime profile selected by composition.
    pub runtime_profile: String,
    /// Opaque execution-class identity selected by composition.
    pub execution_class: String,
}

/// One exact, already-resolved Capability provider selection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FoundationCapabilitySelection {
    /// Consumer-local requirement identity.
    pub requirement_id: String,
    /// Exact consumer instance identity.
    pub consumer_instance: String,
    /// Exact Capability series identity.
    pub capability_id: String,
    /// Exact Capability Descriptor version.
    pub descriptor_version: String,
    /// Exact selected provider instance identity.
    pub provider_instance: String,
    /// Stable order for a selected `many` provider binding.
    pub provider_order: usize,
    /// How the resolver reached this immutable binding.
    pub selection_reason: &'static str,
}

/// The authority boundary reported by a blank Foundation check.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FoundationAuthorizationBoundary {
    /// A blank Foundation has not granted any runtime authority.
    pub foundation_grants_authority: bool,
    /// Component that must own runtime and authorization decisions.
    pub policy_owner: &'static str,
    /// Concise description of the boundary preserved by this check.
    pub message: &'static str,
}

/// One stable, actionable Foundation diagnostic.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FoundationDiagnostic {
    /// Stable diagnostic identity.
    pub code: &'static str,
    /// Human-readable explanation of the current composition state.
    pub message: String,
    /// The narrow next action that preserves explicit composition authority.
    pub next_action: String,
}

fn foundation_host_slots() -> Vec<HostSlot> {
    [
        "agents",
        "artifact",
        "auth",
        "console",
        "plugin-configuration-authority",
        "context-compactor",
        "memory",
        "surfaces",
        "tool-providers",
        "tool-hooks",
        "tool-target",
        "lifecycle-hooks",
        "http-fetch",
        "model",
        "model-selection",
        "process",
        "oauth-access",
        "prompt-providers",
        "prompt-runtime",
        "tools-runtimes",
        "secrets",
        "session",
        "session-presentation",
        "restricted-tools-runtime",
        "terminal-command-runtime",
        "terminal-command-providers",
        "tui-panels",
        "tui-suggestions",
        "user-interaction",
        "workspace-import-read",
        "extensions",
    ]
    .into_iter()
    .map(HostSlot::many)
    .collect()
}

fn check_resolved_app(resolved: &ResolvedApp) -> FoundationCheck {
    let plan: &ResolvedAppPlan = resolved.plan();
    let plugin_instances = plan
        .plugin_instances()
        .iter()
        .map(|instance| instance.instance_key().to_owned())
        .collect::<Vec<_>>();
    let capability_bindings = plan
        .capability_bindings()
        .iter()
        .map(|binding| {
            format!(
                "{} --{}@{}--> {}",
                binding.consumer_instance(),
                binding.capability_id(),
                binding.descriptor_version(),
                binding.provider_instance()
            )
        })
        .collect::<Vec<_>>();
    let plugin_sources = plan
        .plugin_instances()
        .iter()
        .map(|instance| {
            let (plugin_id, selection_source, selection_reason) = resolved
                .instances()
                .iter()
                .find(|selected| selected.plan_key() == instance.instance_key())
                .map(|selected| {
                    let (selection_source, selection_reason) =
                        selection_provenance(selected.source());
                    (
                        selected.id().plugin_id().to_owned(),
                        selection_source,
                        selection_reason,
                    )
                })
                .unwrap_or((
                    instance.instance_key().to_owned(),
                    FoundationPluginSelectionSource::Unknown,
                    "The resolver did not retain source provenance for this Plan instance.",
                ));
            FoundationPluginSource {
                instance: instance.instance_key().to_owned(),
                plugin_id,
                selection_source,
                selection_reason,
                package_id: instance.package_id().to_owned(),
                package_revision: instance.package_revision().to_owned(),
                entrypoint: instance.entrypoint().to_owned(),
                runtime_profile: instance.runtime_profile().to_owned(),
                execution_class: instance.execution_class().as_str().to_owned(),
            }
        })
        .collect::<Vec<_>>();
    let capability_selections = plan
        .capability_bindings()
        .iter()
        .map(|binding| FoundationCapabilitySelection {
            requirement_id: binding.requirement_id().to_owned(),
            consumer_instance: binding.consumer_instance().to_owned(),
            capability_id: binding.capability_id().to_owned(),
            descriptor_version: binding.descriptor_version().to_owned(),
            provider_instance: binding.provider_instance().to_owned(),
            provider_order: binding.provider_order(),
            selection_reason: capability_selection_reason(resolved, binding),
        })
        .collect::<Vec<_>>();
    let (status, diagnostics) = if plugin_instances.is_empty() {
        (
            FoundationStatus::Blank,
            vec![FoundationDiagnostic {
                code: "lenso.agent.foundation.no-explicit-composition",
                message: "No explicit Plugin composition is selected. The Foundation added no model, Loop, Tool, memory, system prompt, or Surface.".to_owned(),
                next_action: "Select a concrete composition through an execution Host or App authoring flow, then check that Host's declared runtime requirements.".to_owned(),
            }],
        )
    } else {
        (
            FoundationStatus::Composed,
            vec![FoundationDiagnostic {
                code: "lenso.agent.foundation.execution-host-required",
                message: "The Plugin Root resolves structurally, but the Foundation never chooses or starts runtime behavior.".to_owned(),
                next_action: "Use an explicit execution Host whose slots, bindings, adapters, and authorization policy cover these selected Plugins.".to_owned(),
            }],
        )
    };
    FoundationCheck {
        schema: FOUNDATION_CHECK_SCHEMA,
        status,
        plugin_instances,
        capability_bindings,
        plugin_sources,
        capability_selections,
        authorization_boundary: FoundationAuthorizationBoundary {
            foundation_grants_authority: false,
            policy_owner: "the selected execution Host",
            message: "The blank Foundation grants no runtime, provider, credential, dynamic-resource, or external-side-effect authority. The selected execution Host must authorize every runtime binding and invocation.",
        },
        diagnostics,
    }
}

fn selection_provenance(
    source: PluginInstanceSource,
) -> (FoundationPluginSelectionSource, &'static str) {
    match source {
        PluginInstanceSource::HostDefault => (
            FoundationPluginSelectionSource::HostDefault,
            "The execution Host selected this instance as its declared default.",
        ),
        PluginInstanceSource::HostDefaultConfiguredByRoot => (
            FoundationPluginSelectionSource::HostDefaultConfiguredByRoot,
            "The execution Host selected its declared default and the Plugin Root supplied its configuration.",
        ),
        PluginInstanceSource::PluginRoot => (
            FoundationPluginSelectionSource::PluginRoot,
            "The visible Plugin Root explicitly selected this instance.",
        ),
    }
}

fn capability_selection_reason(
    resolved: &ResolvedApp,
    binding: &lenso_app_plan::CapabilityBinding,
) -> &'static str {
    let persisted_choice = resolved.dependency_choices().iter().any(|choice| {
        choice.consumer.plan_key() == binding.consumer_instance()
            && choice.requirement_id == binding.requirement_id()
            && choice
                .provider
                .as_ref()
                .is_some_and(|provider| provider.plan_key() == binding.provider_instance())
    });
    if persisted_choice {
        "The Plugin Root persisted this exact named dependency choice before Plan resolution."
    } else {
        "The resolver materialized this exact provider from the Host Catalog and selected Plugin Root constraints."
    }
}

fn ensure_foundation_catalog(root: &Path) -> Result<(), String> {
    let catalog_path = root.join(FOUNDATION_HOST_CATALOG_PATH);
    let bytes = fs::read(&catalog_path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => format!(
            "Foundation Host Catalog is missing at {}; run `lenso-agent-foundation init --root {}` first",
            catalog_path.display(),
            root.display()
        ),
        _ => format!(
            "failed to read Foundation Host Catalog {}: {error}",
            catalog_path.display()
        ),
    })?;
    if catalog_matches_foundation(&bytes)? {
        Ok(())
    } else {
        Err(format!(
            "Host Catalog at {} is not the blank Foundation Catalog; use the owning Host's check command instead",
            catalog_path.display()
        ))
    }
}

fn ensure_root_directory(root: &Path) -> Result<(), String> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
        Ok(_) => Err(format!(
            "Foundation root must be a directory: {}",
            root.display()
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir_all(root)
            .map_err(|error| {
                format!(
                    "failed to create Foundation root {}: {error}",
                    root.display()
                )
            }),
        Err(error) => Err(format!(
            "failed to inspect Foundation root {}: {error}",
            root.display()
        )),
    }
}

fn catalog_json_bytes() -> Result<Vec<u8>, String> {
    let mut bytes = serde_json::to_vec_pretty(&foundation_host_catalog())
        .map_err(|error| format!("failed to encode Foundation Host Catalog: {error}"))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn catalog_matches_foundation(bytes: &[u8]) -> Result<bool, String> {
    let actual: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("Foundation Host Catalog is invalid JSON: {error}"))?;
    let expected = serde_json::to_value(foundation_host_catalog())
        .map_err(|error| format!("failed to encode Foundation Host Catalog: {error}"))?;
    Ok(actual == expected)
}

#[cfg(test)]
mod tests {
    use lenso_app_plan::{
        CapabilityEndpointPlan, CapabilityRequirementPlan,
        authoring::{PluginDescriptor, PluginRootInstance, PluginRootSnapshot},
    };

    use super::{
        FoundationPluginSelectionSource, FoundationStatus, check, check_snapshot,
        foundation_host_catalog, initialize,
    };

    #[test]
    fn catalog_has_no_linked_releases_or_product_defaults() {
        let catalog = foundation_host_catalog();
        assert!(catalog.plugins().is_empty());
        assert!(catalog.defaults().is_empty());
        assert!(catalog.slots().iter().all(|slot| {
            matches!(
                slot.cardinality(),
                lenso_app_plan::authoring::HostSlotCardinality::Many
            )
        }));
    }

    #[test]
    fn blank_root_resolves_without_product_behavior() {
        let temporary = tempfile::tempdir().expect("temporary Foundation root");
        initialize(temporary.path()).expect("initialize blank Foundation root");
        let result = check(temporary.path()).expect("check blank Foundation root");
        assert_eq!(result.status, FoundationStatus::Blank);
        assert!(result.plugin_instances.is_empty());
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(
            result.diagnostics[0].code,
            "lenso.agent.foundation.no-explicit-composition"
        );
    }

    #[test]
    fn explicit_extension_resolves_without_becoming_a_default() {
        let descriptor = PluginDescriptor::new("example.task-board", "1.0.0", "extensions");
        let snapshot = PluginRootSnapshot::new(
            [descriptor],
            [PluginRootInstance::new("example.task-board", "default")],
            [],
        );
        let result = check_snapshot(&snapshot).expect("resolve explicit extension");
        assert_eq!(result.status, FoundationStatus::Composed);
        assert_eq!(result.plugin_instances, ["example.task-board/default"]);
    }

    #[test]
    fn check_reports_source_version_selection_and_authorization_boundary() {
        let provider = PluginDescriptor::new("example.provider", "1.0.0", "extensions")
            .with_runtime_package("example.provider.runtime", "sha256:provider")
            .with_capability(CapabilityEndpointPlan::new(
                "example.task@1",
                "1.0.0",
                ["read"],
            ));
        let consumer = PluginDescriptor::new("example.consumer", "2.0.0", "extensions")
            .with_authoring(2, "lenso.native-rust@1")
            .with_runtime_package("example.consumer.runtime", "sha256:consumer")
            .with_requirement(
                CapabilityRequirementPlan::one("example.task@1", "1.0.0")
                    .with_requirement_id("task"),
            );
        let snapshot = PluginRootSnapshot::new(
            [provider, consumer],
            [
                PluginRootInstance::new("example.provider", "default"),
                PluginRootInstance::new("example.consumer", "default"),
            ],
            [],
        );
        let result = check_snapshot(&snapshot).expect("resolve explicit composition");
        let provider = result
            .plugin_sources
            .iter()
            .find(|source| source.instance == "example.provider/default")
            .expect("provider provenance");
        assert_eq!(
            provider.selection_source,
            FoundationPluginSelectionSource::PluginRoot
        );
        assert_eq!(provider.package_id, "example.provider.runtime");
        assert_eq!(provider.package_revision, "sha256:provider");
        assert_eq!(result.capability_selections.len(), 1);
        assert_eq!(result.capability_selections[0].requirement_id, "task");
        assert_eq!(
            result.capability_selections[0].provider_instance,
            "example.provider/default"
        );
        assert!(!result.authorization_boundary.foundation_grants_authority);
        assert_eq!(
            result.authorization_boundary.policy_owner,
            "the selected execution Host"
        );
    }
}
