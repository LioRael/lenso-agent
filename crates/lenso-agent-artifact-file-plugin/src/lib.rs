//! Bounded file-backed Artifact Provider.

use std::{
    cell::RefCell,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    rc::Rc,
    time::SystemTime,
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use lenso::prelude::*;
use lenso_capability_agent_artifact::{
    self as artifact_contract, PutError, PutRequest, PutResponse, ReadError, ReadRequest,
    ReadResponse,
};
use lenso_kernel::RuntimeFailure;
use sha2::{Digest as _, Sha256};

const HANDLE_PREFIX: &str = "artifact://";
const DIGEST_PREFIX: &str = "sha256:";
const MAX_READ_BYTES: i64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactConfig {
    directory: PathBuf,
    max_artifact_bytes: u64,
    max_total_bytes: u64,
    max_items: usize,
}

fn validate_config(config: &ArtifactConfig) -> Result<(), RuntimeFailure> {
    if !config.directory.is_absolute()
        || !(1..=16_777_216).contains(&config.max_artifact_bytes)
        || config.max_total_bytes < config.max_artifact_bytes
        || config.max_total_bytes > 17_179_869_184
        || !(1..=65_536).contains(&config.max_items)
    {
        return Err(invalid_plan(
            "Artifact storage requires an absolute directory and bounded item and byte limits",
        ));
    }
    Ok(())
}

#[lenso::plugin(configuration_schema = "config.schema.json", validate = validate_config)]
#[derive(Clone, Debug)]
struct FileArtifactPlugin {
    #[config]
    config: ArtifactConfig,
    lock: Rc<RefCell<()>>,
}

#[lenso::provides(artifact_contract::Artifact)]
impl FileArtifactPlugin {
    async fn put(&self, _context: Ctx, request: PutRequest) -> PluginResult<PutResponse, PutError> {
        if !valid_component(&request.session_id, 128)
            || request.name.is_empty()
            || request.name.len() > 256
            || !valid_media_type(&request.media_type)
        {
            return Err(PluginError::domain(PutError::InvalidRequest));
        }
        let bytes = STANDARD
            .decode(request.data_base64.as_bytes())
            .map_err(|_| PluginError::domain(PutError::InvalidData))?;
        if bytes.is_empty() {
            return Err(PluginError::domain(PutError::InvalidData));
        }
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > self.config.max_artifact_bytes {
            return Err(PluginError::domain(PutError::TooLarge));
        }

        let _guard = self.lock.borrow_mut();
        ensure_directory(&self.config.directory).map_err(PluginError::runtime)?;
        let session_directory = self.config.directory.join(&request.session_id);
        ensure_directory(&session_directory).map_err(PluginError::runtime)?;
        let digest_hex = format!("{:x}", Sha256::digest(&bytes));
        let path = session_directory.join(&digest_hex);
        if path.exists() {
            validate_existing_artifact(&path, &digest_hex, bytes.len())
                .map_err(PluginError::runtime)?;
        } else {
            prune_to_fit(
                &self.config.directory,
                u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                self.config.max_total_bytes,
                self.config.max_items,
            )
            .map_err(|error| match error {
                PruneError::Capacity => PluginError::domain(PutError::CapacityExceeded),
                PruneError::Runtime(error) => PluginError::runtime(error),
            })?;
            write_atomic(&path, &bytes).map_err(PluginError::runtime)?;
        }
        Ok(PutResponse {
            handle: format!("{HANDLE_PREFIX}{}/{digest_hex}", request.session_id),
            digest: format!("{DIGEST_PREFIX}{digest_hex}"),
            size: bytes.len().to_string(),
        })
    }

    async fn read(
        &self,
        _context: Ctx,
        request: ReadRequest,
    ) -> PluginResult<ReadResponse, ReadError> {
        let (session_id, digest) = parse_handle(&request.handle)
            .ok_or_else(|| PluginError::domain(ReadError::InvalidHandle))?;
        let offset = request
            .offset
            .parse::<u64>()
            .map_err(|_| PluginError::domain(ReadError::InvalidRange))?;
        if request.max_bytes == 0 || request.max_bytes > MAX_READ_BYTES {
            return Err(PluginError::domain(ReadError::InvalidRange));
        }
        let _guard = self.lock.borrow_mut();
        ensure_existing_directory(&self.config.directory).map_err(PluginError::runtime)?;
        let session_directory = self.config.directory.join(session_id);
        ensure_existing_directory(&session_directory).map_err(|error| match error {
            RuntimeFailure::Unavailable { .. } => PluginError::domain(ReadError::NotFound),
            error => PluginError::runtime(error),
        })?;
        let path = session_directory.join(digest);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => metadata,
            Ok(_) => {
                return Err(PluginError::runtime(storage_failure(
                    "Artifact path is unsafe",
                )));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(PluginError::domain(ReadError::NotFound));
            }
            Err(_) => {
                return Err(PluginError::runtime(storage_failure(
                    "Artifact read failed",
                )));
            }
        };
        if offset > metadata.len() {
            return Err(PluginError::domain(ReadError::InvalidRange));
        }
        let remaining = metadata.len() - offset;
        let read_len = remaining.min(u64::try_from(request.max_bytes).unwrap_or(0));
        let mut file = File::open(&path)
            .map_err(|_| PluginError::runtime(storage_failure("Artifact read failed")))?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|_| PluginError::runtime(storage_failure("Artifact read failed")))?;
        let mut bytes = vec![0; usize::try_from(read_len).unwrap_or(0)];
        file.read_exact(&mut bytes)
            .map_err(|_| PluginError::runtime(storage_failure("Artifact read failed")))?;
        let next_offset = offset.saturating_add(read_len);
        Ok(ReadResponse {
            data_base64: STANDARD.encode(bytes),
            total_size: metadata.len().to_string(),
            next_offset: next_offset.to_string(),
            complete: next_offset == metadata.len(),
        })
    }
}

fn valid_component(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn valid_media_type(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'+' | b'-' | b'.'))
}

fn parse_handle(handle: &str) -> Option<(&str, &str)> {
    let value = handle.strip_prefix(HANDLE_PREFIX)?;
    let (session_id, digest) = value.split_once('/')?;
    (valid_component(session_id, 128)
        && digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then_some((session_id, digest))
}

fn ensure_directory(path: &Path) -> Result<(), RuntimeFailure> {
    fs::create_dir_all(path).map_err(|_| storage_failure("Artifact directory is unavailable"))?;
    ensure_existing_directory(path)
}

fn ensure_existing_directory(path: &Path) -> Result<(), RuntimeFailure> {
    let metadata = fs::symlink_metadata(path).map_err(|_| RuntimeFailure::Unavailable {
        capability: artifact_contract::CAPABILITY_ID,
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(storage_failure("Artifact directory is unsafe"));
    }
    Ok(())
}

fn validate_regular_file(path: &Path) -> Result<(), RuntimeFailure> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| storage_failure("Artifact path is unavailable"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(storage_failure("Artifact path is unsafe"));
    }
    Ok(())
}

fn validate_existing_artifact(
    path: &Path,
    expected_digest: &str,
    expected_length: usize,
) -> Result<(), RuntimeFailure> {
    validate_regular_file(path)?;
    let mut file = File::open(path).map_err(|_| storage_failure("Artifact path is unavailable"))?;
    let metadata = file
        .metadata()
        .map_err(|_| storage_failure("Artifact metadata is unavailable"))?;
    if metadata.len() != u64::try_from(expected_length).unwrap_or(u64::MAX) {
        return Err(storage_failure("Existing Artifact content is inconsistent"));
    }
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| storage_failure("Artifact verification failed"))?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    if format!("{:x}", digest.finalize()) != expected_digest {
        return Err(storage_failure("Existing Artifact content is inconsistent"));
    }
    Ok(())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), RuntimeFailure> {
    let parent = path
        .parent()
        .ok_or_else(|| storage_failure("Artifact path has no parent"))?;
    let temporary = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options
            .open(&temporary)
            .map_err(|_| storage_failure("Artifact write failed"))?;
        file.write_all(bytes)
            .map_err(|_| storage_failure("Artifact write failed"))?;
        file.sync_all()
            .map_err(|_| storage_failure("Artifact write failed"))?;
        fs::rename(&temporary, path).map_err(|_| storage_failure("Artifact commit failed"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

enum PruneError {
    Capacity,
    Runtime(RuntimeFailure),
}

fn prune_to_fit(
    root: &Path,
    incoming: u64,
    max_total: u64,
    max_items: usize,
) -> Result<(), PruneError> {
    let mut files = Vec::new();
    for session in fs::read_dir(root)
        .map_err(|_| PruneError::Runtime(storage_failure("Artifact inventory failed")))?
    {
        let session = session
            .map_err(|_| PruneError::Runtime(storage_failure("Artifact inventory failed")))?;
        let metadata = session
            .metadata()
            .map_err(|_| PruneError::Runtime(storage_failure("Artifact inventory failed")))?;
        if !metadata.is_dir() || session.file_type().is_ok_and(|kind| kind.is_symlink()) {
            return Err(PruneError::Runtime(storage_failure(
                "Artifact inventory contains an unsafe entry",
            )));
        }
        for entry in fs::read_dir(session.path())
            .map_err(|_| PruneError::Runtime(storage_failure("Artifact inventory failed")))?
        {
            let entry = entry
                .map_err(|_| PruneError::Runtime(storage_failure("Artifact inventory failed")))?;
            let name = entry.file_name();
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            let metadata = entry
                .metadata()
                .map_err(|_| PruneError::Runtime(storage_failure("Artifact inventory failed")))?;
            if !metadata.is_file() || entry.file_type().is_ok_and(|kind| kind.is_symlink()) {
                return Err(PruneError::Runtime(storage_failure(
                    "Artifact inventory contains an unsafe entry",
                )));
            }
            files.push((
                metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                entry.path(),
                metadata.len(),
            ));
        }
    }
    files.sort_by_key(|(modified, path, _)| (*modified, path.clone()));
    let mut total = files.iter().map(|(_, _, size)| *size).sum::<u64>();
    let mut count = files.len();
    for (_, path, size) in files {
        if total.saturating_add(incoming) <= max_total && count.saturating_add(1) <= max_items {
            break;
        }
        fs::remove_file(path)
            .map_err(|_| PruneError::Runtime(storage_failure("Artifact retention failed")))?;
        total = total.saturating_sub(size);
        count = count.saturating_sub(1);
    }
    if total.saturating_add(incoming) > max_total || count.saturating_add(1) > max_items {
        return Err(PruneError::Capacity);
    }
    Ok(())
}

fn invalid_plan(detail: impl Into<String>) -> RuntimeFailure {
    RuntimeFailure::InvalidResolvedPlan {
        detail: detail.into(),
    }
}

fn storage_failure(detail: impl Into<String>) -> RuntimeFailure {
    RuntimeFailure::PluginFailure {
        detail: detail.into(),
    }
}

#[cfg(test)]
mod tests {
    use futures::executor::block_on;
    use lenso_kernel::{CancellationToken, InvocationContext};

    use super::*;

    fn provider(directory: &Path) -> FileArtifactPlugin {
        FileArtifactPlugin {
            config: ArtifactConfig {
                directory: directory.to_owned(),
                max_artifact_bytes: 1024,
                max_total_bytes: 2048,
                max_items: 2,
            },
            lock: Rc::new(RefCell::new(())),
        }
    }

    fn context() -> InvocationContext {
        InvocationContext::new(1, None, CancellationToken::new())
    }

    #[test]
    fn writes_content_addressed_bytes_and_reads_bounded_chunks() {
        let temporary = tempfile::tempdir().unwrap();
        let provider = provider(&temporary.path().join("artifacts"));
        let put = block_on(artifact_contract::ArtifactProvider::put(
            &provider,
            context(),
            PutRequest {
                session_id: "session-1".to_owned(),
                name: "result.txt".to_owned(),
                media_type: "text/plain".to_owned(),
                data_base64: STANDARD.encode(b"hello artifact"),
            },
        ))
        .unwrap()
        .unwrap();
        assert!(put.handle.starts_with("artifact://session-1/"));
        let read = block_on(artifact_contract::ArtifactProvider::read(
            &provider,
            context(),
            ReadRequest {
                handle: put.handle,
                offset: "6".to_owned(),
                max_bytes: 8,
            },
        ))
        .unwrap()
        .unwrap();
        assert_eq!(STANDARD.decode(read.data_base64).unwrap(), b"artifact");
        assert!(read.complete);
    }

    #[test]
    fn rejects_unsafe_handles_and_oversized_payloads() {
        let temporary = tempfile::tempdir().unwrap();
        let provider = provider(&temporary.path().join("artifacts"));
        let invalid = block_on(artifact_contract::ArtifactProvider::read(
            &provider,
            context(),
            ReadRequest {
                handle: "artifact://../secret".to_owned(),
                offset: "0".to_owned(),
                max_bytes: 8,
            },
        ))
        .unwrap();
        assert_eq!(invalid, Err(ReadError::InvalidHandle));

        let oversized = block_on(artifact_contract::ArtifactProvider::put(
            &provider,
            context(),
            PutRequest {
                session_id: "session-1".to_owned(),
                name: "large.bin".to_owned(),
                media_type: "application/octet-stream".to_owned(),
                data_base64: STANDARD.encode(vec![0_u8; 1025]),
            },
        ))
        .unwrap();
        assert_eq!(oversized, Err(PutError::TooLarge));
    }

    #[test]
    fn rejects_corrupt_existing_content_addressed_files() {
        let temporary = tempfile::tempdir().unwrap();
        let provider = provider(&temporary.path().join("artifacts"));
        let request = PutRequest {
            session_id: "session-1".to_owned(),
            name: "result.txt".to_owned(),
            media_type: "text/plain".to_owned(),
            data_base64: STANDARD.encode(b"expected"),
        };
        let stored = block_on(artifact_contract::ArtifactProvider::put(
            &provider,
            context(),
            request.clone(),
        ))
        .unwrap()
        .unwrap();
        let digest = stored.digest.strip_prefix(DIGEST_PREFIX).unwrap();
        fs::write(
            temporary
                .path()
                .join("artifacts")
                .join("session-1")
                .join(digest),
            b"tampered",
        )
        .unwrap();

        let error = block_on(artifact_contract::ArtifactProvider::put(
            &provider,
            context(),
            request,
        ))
        .unwrap_err();
        assert!(matches!(error, RuntimeFailure::PluginFailure { .. }));
    }
}
