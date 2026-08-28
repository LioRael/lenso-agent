use std::{collections::BTreeSet, path::Path};

use lenso_plugin_control_plane::sha256_digest;

#[derive(Clone, Debug)]
pub(crate) struct GenerationAuthority {
    pub(crate) resolution_authority_digest: String,
}

fn current() -> GenerationAuthority {
    GenerationAuthority {
        resolution_authority_digest: sha256_digest(b"lenso.host-plugin-root-authority@2"),
    }
}

#[cfg(test)]
pub(crate) fn load_generation_authority(root: &Path) -> Result<GenerationAuthority, String> {
    let coordinator = crate::authority::AuthorityCoordinator::prepare(root)?;
    let _fence = coordinator.snapshot()?;
    Ok(current())
}

pub(crate) fn load_generation_authority_unfenced(_root: &Path) -> GenerationAuthority {
    current()
}

pub(crate) fn record_resolved_generation_authority_unfenced(
    _root: &Path,
    _authority: &GenerationAuthority,
) {
}

pub(crate) fn recovery_generation_authorities(_root: &Path) -> Vec<GenerationAuthority> {
    vec![current()]
}

pub(crate) fn retained_resolution_authority_digests(_root: &Path) -> BTreeSet<String> {
    BTreeSet::new()
}

pub(crate) fn retained_resolution_authority_digests_unfenced(_root: &Path) -> BTreeSet<String> {
    BTreeSet::new()
}

pub(crate) fn recovery_generation_authority_gc_candidates_unfenced(
    _root: &Path,
    _retained: &BTreeSet<String>,
) -> Vec<String> {
    Vec::new()
}

pub(crate) fn prune_recovery_generation_authorities_unfenced(
    _root: &Path,
    _retained: &BTreeSet<String>,
) -> Vec<String> {
    Vec::new()
}
