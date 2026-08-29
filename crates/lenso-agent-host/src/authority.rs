use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

use fs2::FileExt;

const LOCK_FILE: &str = "generation-authority.lock";
const GENERATION_GC_LOCK_FILE: &str = "generation-gc.lock";
const LEASE_DIRECTORY: &str = ".leases";

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
        let root = root.join(LEASE_DIRECTORY);
        fs::create_dir_all(&root)
            .map_err(|error| format!("failed to create Agent runtime lease root: {error}"))?;
        validate_root(&root)?;
        let coordinator = Self { root };
        coordinator.open_lock(true)?;
        Ok(coordinator)
    }

    /// Open an existing authority without creating inspection state.
    pub(crate) fn open_existing(root: &Path) -> Result<Self, String> {
        let root = root.join(LEASE_DIRECTORY);
        validate_root(&root)?;
        let coordinator = Self { root };
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

    /// Attempts to snapshot Plugin authority without blocking the Host's async runtime.
    ///
    /// A mutating CLI process may hold the exclusive fence while it validates a candidate.
    /// The live Host keeps serving its current Generation and retries on the next reconcile tick.
    pub(crate) fn try_snapshot(&self) -> Result<Option<AuthorityFence>, String> {
        let file = self.open_lock(false)?;
        match FileExt::try_lock_shared(&file) {
            Ok(()) => Ok(Some(AuthorityFence { file })),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(format!("failed to snapshot Plugin authority: {error}")),
        }
    }

    /// Fence one Ready-before-commit authority transition.
    pub(crate) fn transition(&self) -> Result<AuthorityFence, String> {
        let file = self.open_lock(false)?;
        FileExt::lock_exclusive(&file)
            .map_err(|error| format!("failed to fence Plugin authority transition: {error}"))?;
        Ok(AuthorityFence { file })
    }

    /// Prevents Generation collection while this Host can admit or persist a Turn.
    pub(crate) fn generation_gc_snapshot(&self) -> Result<AuthorityFence, String> {
        let file = open_regular_lock(&self.root.join(GENERATION_GC_LOCK_FILE), true)?;
        FileExt::lock_shared(&file)
            .map_err(|error| format!("failed to lease Generation provenance: {error}"))?;
        Ok(AuthorityFence { file })
    }

    /// Waits for every Host using this authority root before applying one GC snapshot.
    pub(crate) fn generation_gc_transition(&self) -> Result<AuthorityFence, String> {
        let file = open_regular_lock(&self.root.join(GENERATION_GC_LOCK_FILE), true)?;
        FileExt::lock_exclusive(&file)
            .map_err(|error| format!("failed to fence Generation collection: {error}"))?;
        Ok(AuthorityFence { file })
    }

    /// Attempts one maintenance transition without delaying Host startup or shutdown.
    pub(crate) fn try_generation_gc_transition(&self) -> Result<Option<AuthorityFence>, String> {
        let file = open_regular_lock(&self.root.join(GENERATION_GC_LOCK_FILE), true)?;
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => Ok(Some(AuthorityFence { file })),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(format!("failed to fence Generation collection: {error}")),
        }
    }

    /// Attempts to own one Controller namespace without blocking another Host.
    pub(crate) fn try_host_lease(&self, namespace: &str) -> Result<Option<AuthorityFence>, String> {
        if namespace.is_empty()
            || !namespace
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err("Host lease namespace is invalid".to_owned());
        }
        let path = self.root.join(format!("{namespace}.host.lock"));
        let file = open_regular_lock(&path, true)?;
        match FileExt::try_lock_exclusive(&file) {
            Ok(()) => Ok(Some(AuthorityFence { file })),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(error) => Err(format!(
                "failed to lease the `{namespace}` Controller namespace: {error}"
            )),
        }
    }

    fn open_lock(&self, create: bool) -> Result<File, String> {
        let path = self.root.join(LOCK_FILE);
        open_regular_lock(&path, create)
    }
}

fn open_regular_lock(path: &Path, create: bool) -> Result<File, String> {
    match fs::symlink_metadata(path) {
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
        .open(path)
        .map_err(|error| format!("failed to open Plugin authority lock: {error}"))?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("failed to recheck Plugin authority lock: {error}"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Plugin authority lock is not a regular file".to_owned());
    }
    Ok(file)
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
        let authority = crate::generation_authority::load_generation_authority(&root).unwrap();
        assert!(started.elapsed() >= Duration::from_millis(500));
        assert!(authority.resolution_authority_digest.starts_with("sha256:"));
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
    fn live_reconcile_never_blocks_on_an_active_transition() {
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

        let coordinator = AuthorityCoordinator::prepare(&root).unwrap();
        let started = Instant::now();
        assert!(coordinator.try_snapshot().unwrap().is_none());
        assert!(started.elapsed() < Duration::from_millis(100));
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
    fn generation_collection_waits_for_a_running_host_lease() {
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
            .env(CHILD_MODE, "generation-gc-snapshot")
            .env(CHILD_ROOT, &root)
            .env(CHILD_READY, &ready)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.exists() {
            assert!(
                Instant::now() < deadline,
                "child did not acquire Generation GC lease"
            );
            thread::sleep(Duration::from_millis(10));
        }

        let coordinator = AuthorityCoordinator::prepare(&root).unwrap();
        let started = Instant::now();
        drop(coordinator.generation_gc_transition().unwrap());
        assert!(started.elapsed() >= Duration::from_millis(500));
        assert!(child.wait().unwrap().success());
    }

    #[test]
    fn controller_slots_are_claimed_independently() {
        let temporary = tempfile::tempdir().unwrap();
        let coordinator = AuthorityCoordinator::prepare(temporary.path()).unwrap();
        let first = coordinator
            .try_host_lease("tui-generation-control")
            .unwrap();
        assert!(first.is_some());
        assert!(
            coordinator
                .try_host_lease("tui-generation-control")
                .unwrap()
                .is_none()
        );
        let second = coordinator
            .try_host_lease("tui-generation-control-2")
            .unwrap();
        assert!(second.is_some());
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
        let _fence = match mode.as_str() {
            "snapshot" => coordinator.snapshot().unwrap(),
            "generation-gc-snapshot" => coordinator.generation_gc_snapshot().unwrap(),
            _ => coordinator.transition().unwrap(),
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
