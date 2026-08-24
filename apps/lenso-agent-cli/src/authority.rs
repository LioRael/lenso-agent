use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

use fs2::FileExt;

const LOCK_FILE: &str = "active-set.lock";

/// Coordinates one Host's durable Plugin authority across processes.
#[derive(Debug)]
pub(crate) struct AuthorityCoordinator {
    root: PathBuf,
}

impl AuthorityCoordinator {
    /// Prepare an authority root for startup or a mutating transition.
    pub(crate) fn prepare(root: &Path) -> Result<Self, String> {
        fs::create_dir_all(root)
            .map_err(|error| format!("failed to create Plugin authority root: {error}"))?;
        validate_root(root)?;
        let coordinator = Self {
            root: root.to_path_buf(),
        };
        coordinator.open_lock(true)?;
        Ok(coordinator)
    }

    /// Open an existing authority without creating inspection state.
    pub(crate) fn open_existing(root: &Path) -> Result<Self, String> {
        validate_root(root)?;
        let coordinator = Self {
            root: root.to_path_buf(),
        };
        coordinator.open_lock(false)?;
        Ok(coordinator)
    }

    /// Fence a complete read and validation of one immutable authority snapshot.
    pub(crate) fn snapshot(&self) -> Result<AuthorityFence, String> {
        let file = self.open_lock(false)?;
        FileExt::lock_shared(&file)
            .map_err(|error| format!("failed to snapshot Plugin authority: {error}"))?;
        Ok(AuthorityFence { file })
    }

    /// Fence one Ready-before-commit authority transition.
    pub(crate) fn transition(&self) -> Result<AuthorityFence, String> {
        let file = self.open_lock(false)?;
        FileExt::lock_exclusive(&file)
            .map_err(|error| format!("failed to fence Plugin authority transition: {error}"))?;
        Ok(AuthorityFence { file })
    }

    fn open_lock(&self, create: bool) -> Result<File, String> {
        let path = self.root.join(LOCK_FILE);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if !metadata.is_file() || metadata.file_type().is_symlink() {
                    return Err("Plugin authority lock is not a regular file".to_owned());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && create => {}
            Err(error) => {
                return Err(format!("failed to inspect Plugin authority lock: {error}"));
            }
        }
        let file = OpenOptions::new()
            .create(create)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| format!("failed to open Plugin authority lock: {error}"))?;
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| format!("failed to recheck Plugin authority lock: {error}"))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err("Plugin authority lock is not a regular file".to_owned());
        }
        Ok(file)
    }
}

/// RAII ownership of a shared snapshot or exclusive transition fence.
#[derive(Debug)]
pub(crate) struct AuthorityFence {
    file: File,
}

impl Drop for AuthorityFence {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

fn validate_root(root: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(root)
        .map_err(|error| format!("failed to inspect Plugin authority root: {error}"))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Plugin authority root is not a regular directory".to_owned());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        env,
        process::Command,
        thread,
        time::{Duration, Instant},
    };

    use super::*;

    const CHILD_MODE: &str = "LENSO_AUTHORITY_FENCE_CHILD";
    const CHILD_ROOT: &str = "LENSO_AUTHORITY_FENCE_ROOT";
    const CHILD_READY: &str = "LENSO_AUTHORITY_FENCE_READY";

    #[test]
    fn app_startup_waits_for_another_process_transition() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("plugins");
        let ready = temporary.path().join("ready");
        AuthorityCoordinator::prepare(&root).unwrap();
        let executable = env::current_exe().unwrap();
        let mut child = Command::new(executable)
            .args([
                "--exact",
                "authority::tests::child_holds_authority_fence",
                "--nocapture",
            ])
            .env(CHILD_MODE, "hold")
            .env(CHILD_ROOT, &root)
            .env(CHILD_READY, &ready)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.exists() {
            assert!(
                Instant::now() < deadline,
                "child did not acquire transition fence"
            );
            thread::sleep(Duration::from_millis(10));
        }

        let started = Instant::now();
        let authority = crate::plugins::load_generation_authority(&root).unwrap();
        assert!(started.elapsed() >= Duration::from_millis(500));
        assert!(authority.lock.value().plugins.is_empty());
        assert!(child.wait().unwrap().success());
    }

    #[test]
    fn transition_waits_for_another_process_snapshot() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("plugins");
        let ready = temporary.path().join("ready");
        AuthorityCoordinator::prepare(&root).unwrap();
        let executable = env::current_exe().unwrap();
        let mut child = Command::new(executable)
            .args([
                "--exact",
                "authority::tests::child_holds_authority_fence",
                "--nocapture",
            ])
            .env(CHILD_MODE, "snapshot")
            .env(CHILD_ROOT, &root)
            .env(CHILD_READY, &ready)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.exists() {
            assert!(
                Instant::now() < deadline,
                "child did not acquire snapshot fence"
            );
            thread::sleep(Duration::from_millis(10));
        }

        let coordinator = AuthorityCoordinator::prepare(&root).unwrap();
        let started = Instant::now();
        drop(coordinator.transition().unwrap());
        assert!(started.elapsed() >= Duration::from_millis(500));
        assert!(child.wait().unwrap().success());
    }

    #[test]
    fn process_exit_releases_the_transition_fence() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("plugins");
        let ready = temporary.path().join("ready");
        AuthorityCoordinator::prepare(&root).unwrap();
        let executable = env::current_exe().unwrap();
        let status = Command::new(executable)
            .args([
                "--exact",
                "authority::tests::child_holds_authority_fence",
                "--nocapture",
            ])
            .env(CHILD_MODE, "exit")
            .env(CHILD_ROOT, &root)
            .env(CHILD_READY, &ready)
            .status()
            .unwrap();
        assert!(status.success());
        assert!(ready.exists());
        let coordinator = AuthorityCoordinator::prepare(&root).unwrap();
        drop(coordinator.snapshot().unwrap());
    }

    #[test]
    fn child_holds_authority_fence() {
        if env::var_os(CHILD_MODE).is_none() {
            return;
        }
        let root = PathBuf::from(env::var_os(CHILD_ROOT).unwrap());
        let ready = PathBuf::from(env::var_os(CHILD_READY).unwrap());
        let coordinator = AuthorityCoordinator::prepare(&root).unwrap();
        let mode = env::var(CHILD_MODE).unwrap();
        let _fence = if mode == "snapshot" {
            coordinator.snapshot().unwrap()
        } else {
            coordinator.transition().unwrap()
        };
        fs::write(ready, b"ready").unwrap();
        if mode == "exit" {
            std::process::exit(0);
        }
        thread::sleep(Duration::from_secs(1));
    }

    #[test]
    fn existing_open_never_creates_authority() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("missing");
        assert!(AuthorityCoordinator::open_existing(&root).is_err());
        assert!(!root.exists());
    }
}
