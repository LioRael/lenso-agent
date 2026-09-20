use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Bounded limits for a read-only Workspace snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WorkspaceCaptureLimits {
    pub max_files: usize,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
}

impl Default for WorkspaceCaptureLimits {
    fn default() -> Self {
        Self {
            max_files: 20_000,
            max_file_bytes: 8 * 1024 * 1024,
            max_total_bytes: 64 * 1024 * 1024,
        }
    }
}

/// Digest and size for one regular Workspace file.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WorkspaceFile {
    pub sha256: String,
    pub bytes: u64,
}

/// Stable, content-only snapshot of a Workspace.
///
/// The snapshot deliberately does not retain an absolute source path, file
/// permissions, timestamps, Git metadata, or file contents. It is suitable for
/// comparing the source state around one Agent task without leaking the source
/// itself into a Session or report.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WorkspaceSnapshot {
    pub schema: String,
    pub files: BTreeMap<String, WorkspaceFile>,
}

impl WorkspaceSnapshot {
    pub const SCHEMA: &'static str = "lenso.agent.workspace-snapshot@1";

    /// Captures all regular files below `root`, excluding only Git's private
    /// top-level `.git` entry.
    pub fn capture(root: &Path) -> Result<Self, String> {
        Self::capture_with_limits(root, WorkspaceCaptureLimits::default())
    }

    /// Captures a Workspace under explicit resource limits.
    pub fn capture_with_limits(
        root: &Path,
        limits: WorkspaceCaptureLimits,
    ) -> Result<Self, String> {
        if limits.max_files == 0 || limits.max_file_bytes == 0 || limits.max_total_bytes == 0 {
            return Err("Workspace snapshot limits must be greater than zero".to_owned());
        }
        let metadata = fs::symlink_metadata(root)
            .map_err(|error| format!("failed to inspect Workspace root: {error}"))?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err("Workspace root must be a non-symlink directory".to_owned());
        }

        let mut state = CaptureState {
            limits,
            total_bytes: 0,
            files: BTreeMap::new(),
        };
        state.walk(root, Path::new(""))?;
        let snapshot = Self {
            schema: Self::SCHEMA.to_owned(),
            files: state.files,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    /// Validates a decoded snapshot before it is used as evaluation evidence.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != Self::SCHEMA {
            return Err(format!(
                "Workspace snapshot schema `{}` is unsupported",
                self.schema
            ));
        }
        for (path, file) in &self.files {
            validate_workspace_path(path)?;
            validate_digest(&file.sha256)?;
        }
        Ok(())
    }

    /// Computes the content-state delta from `self` to `after`.
    pub fn diff(&self, after: &Self) -> Result<WorkspaceDelta, String> {
        self.validate()?;
        after.validate()?;
        let mut added_paths = BTreeSet::new();
        let mut changed_paths = BTreeSet::new();
        let mut removed_paths = BTreeSet::new();

        for (path, after_file) in &after.files {
            match self.files.get(path) {
                None => {
                    added_paths.insert(path.clone());
                }
                Some(before_file) if before_file != after_file => {
                    changed_paths.insert(path.clone());
                }
                Some(_) => {}
            }
        }
        for path in self.files.keys() {
            if !after.files.contains_key(path) {
                removed_paths.insert(path.clone());
            }
        }
        Ok(WorkspaceDelta {
            added_paths,
            changed_paths,
            removed_paths,
        })
    }
}

/// File-path sets that changed between two Workspace snapshots.
#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct WorkspaceDelta {
    pub added_paths: BTreeSet<String>,
    pub changed_paths: BTreeSet<String>,
    pub removed_paths: BTreeSet<String>,
}

impl WorkspaceDelta {
    /// Returns every added, modified, or removed path as one deterministic set.
    pub fn all_changed_paths(&self) -> BTreeSet<String> {
        self.added_paths
            .iter()
            .chain(&self.changed_paths)
            .chain(&self.removed_paths)
            .cloned()
            .collect()
    }
}

struct CaptureState {
    limits: WorkspaceCaptureLimits,
    total_bytes: u64,
    files: BTreeMap<String, WorkspaceFile>,
}

impl CaptureState {
    fn walk(&mut self, directory: &Path, relative: &Path) -> Result<(), String> {
        let mut entries = fs::read_dir(directory)
            .map_err(|error| format!("failed to read Workspace directory: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("failed to inspect Workspace directory: {error}"))?;
        entries.sort_by_key(fs::DirEntry::file_name);

        for entry in entries {
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "Workspace paths must be valid UTF-8".to_owned())?;
            let child_relative = relative.join(&name);
            if child_relative == Path::new(".git") {
                continue;
            }
            let child = entry.path();
            let metadata = fs::symlink_metadata(&child)
                .map_err(|error| format!("failed to inspect Workspace path `{name}`: {error}"))?;
            if metadata.file_type().is_symlink() {
                return Err(format!(
                    "Workspace snapshot rejects symlink `{}`",
                    display_relative(&child_relative)
                ));
            }
            if metadata.is_dir() {
                self.walk(&child, &child_relative)?;
            } else if metadata.is_file() {
                self.capture_file(&child, &child_relative, metadata.len())?;
            } else {
                return Err(format!(
                    "Workspace snapshot rejects non-regular path `{}`",
                    display_relative(&child_relative)
                ));
            }
        }
        Ok(())
    }

    fn capture_file(
        &mut self,
        path: &Path,
        relative: &Path,
        metadata_length: u64,
    ) -> Result<(), String> {
        let relative = display_relative(relative);
        validate_workspace_path(&relative)?;
        if metadata_length > self.limits.max_file_bytes {
            return Err(format!(
                "Workspace file `{relative}` exceeds the per-file snapshot limit"
            ));
        }
        if self.files.len() >= self.limits.max_files {
            return Err("Workspace snapshot exceeds the file-count limit".to_owned());
        }
        let content = fs::read(path)
            .map_err(|error| format!("failed to read Workspace file `{relative}`: {error}"))?;
        let bytes = u64::try_from(content.len())
            .map_err(|_| "Workspace file length does not fit in u64".to_owned())?;
        if bytes > self.limits.max_file_bytes {
            return Err(format!(
                "Workspace file `{relative}` exceeds the per-file snapshot limit"
            ));
        }
        self.total_bytes = self
            .total_bytes
            .checked_add(bytes)
            .ok_or_else(|| "Workspace snapshot byte count overflowed".to_owned())?;
        if self.total_bytes > self.limits.max_total_bytes {
            return Err("Workspace snapshot exceeds the total-byte limit".to_owned());
        }
        let digest = format!("sha256:{:x}", Sha256::digest(content));
        let previous = self.files.insert(
            relative.clone(),
            WorkspaceFile {
                sha256: digest,
                bytes,
            },
        );
        if previous.is_some() {
            return Err(format!("Workspace path `{relative}` was encountered twice"));
        }
        Ok(())
    }
}

fn display_relative(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str(),
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Validates one snapshot path without touching the filesystem.
pub fn validate_workspace_path(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 4_096
        || value.contains('\\')
        || value.starts_with('/')
        || value.ends_with('/')
        || value
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        return Err(format!("Workspace path `{value}` is invalid"));
    }
    Ok(())
}

/// Validates the canonical SHA-256 form used by Workspace evidence.
pub fn validate_digest(value: &str) -> Result<(), String> {
    let Some(hash) = value.strip_prefix("sha256:") else {
        return Err("Workspace digest must use the `sha256:` prefix".to_owned());
    };
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("Workspace digest must be canonical lowercase SHA-256".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_reports_added_changed_and_removed_paths_in_order() {
        let temporary = tempfile::tempdir().unwrap();
        fs::create_dir_all(temporary.path().join("src")).unwrap();
        fs::write(temporary.path().join("src/lib.rs"), "before").unwrap();
        fs::write(temporary.path().join("keep.txt"), "same").unwrap();
        fs::write(temporary.path().join("gone.txt"), "gone").unwrap();
        let before = WorkspaceSnapshot::capture(temporary.path()).unwrap();

        fs::write(temporary.path().join("src/lib.rs"), "after").unwrap();
        fs::remove_file(temporary.path().join("gone.txt")).unwrap();
        fs::write(temporary.path().join("new.txt"), "new").unwrap();
        let after = WorkspaceSnapshot::capture(temporary.path()).unwrap();

        assert_eq!(
            before.diff(&after).unwrap(),
            WorkspaceDelta {
                added_paths: BTreeSet::from(["new.txt".to_owned()]),
                changed_paths: BTreeSet::from(["src/lib.rs".to_owned()]),
                removed_paths: BTreeSet::from(["gone.txt".to_owned()]),
            }
        );
    }

    #[test]
    fn capture_never_traverses_git_metadata() {
        let temporary = tempfile::tempdir().unwrap();
        fs::create_dir_all(temporary.path().join(".git")).unwrap();
        fs::write(temporary.path().join(".git/config"), "private").unwrap();
        fs::write(temporary.path().join("source.txt"), "source").unwrap();
        let snapshot = WorkspaceSnapshot::capture(temporary.path()).unwrap();
        assert_eq!(snapshot.files.len(), 1);
        assert!(snapshot.files.contains_key("source.txt"));
    }

    #[cfg(unix)]
    #[test]
    fn capture_rejects_symlinks() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("target.txt");
        fs::write(&target, "source").unwrap();
        symlink(&target, temporary.path().join("link.txt")).unwrap();

        let error = WorkspaceSnapshot::capture(temporary.path()).unwrap_err();
        assert!(error.contains("rejects symlink `link.txt`"));
    }

    #[test]
    fn decoded_snapshot_rejects_unsafe_paths_and_digests() {
        let snapshot = WorkspaceSnapshot {
            schema: WorkspaceSnapshot::SCHEMA.to_owned(),
            files: BTreeMap::from([(
                "../escape".to_owned(),
                WorkspaceFile {
                    sha256: format!("sha256:{}", "0".repeat(64)),
                    bytes: 1,
                },
            )]),
        };
        assert!(snapshot.validate().is_err());
    }
}
