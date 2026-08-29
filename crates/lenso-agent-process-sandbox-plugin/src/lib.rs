//! Workspace-write OS-isolated process execution provider.

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Component, Path, PathBuf},
    process::{ExitStatus, Stdio},
    rc::Rc,
    time::{Duration, Instant},
};

use futures::future::ready;
use lenso::prelude::*;
use lenso_agent_native_support::WorkspaceScope;
use lenso_capability_agent_process::{
    self as process_contract, CatalogRequest, CatalogResponse, CatalogResponseProgramsItem,
    ProcessProvider, ProcessRun, ProcessRunStreamInvocationError, RunError, RunRequest,
    RunResponse, RunStreamError, RunStreamRequest, RunStreamResponse, RunStreamResponseKind,
};
use lenso_kernel::{InvocationContext, NativeStreamSession, RuntimeFailure};
use tokio::{io::AsyncReadExt, process::Child};

const PRIVATE_TMP_WORKSPACE_CONFLICT: &str =
    "sandbox Workspace root cannot equal the Linux private temporary mount point `/tmp`";

#[cfg(target_os = "macos")]
const SEATBELT_DENY_NETWORK: &str = r#"(version 1)
(deny default)
(allow process*)
(allow signal (target self))
(allow sysctl-read)
(allow file-read*)
(allow file-write*
    (subpath (param "WORKSPACE"))
    (subpath (param "TEMP")))
(deny network*)"#;

#[cfg(target_os = "macos")]
const SEATBELT_ALLOW_NETWORK: &str = r#"(version 1)
(deny default)
(allow process*)
(allow signal (target self))
(allow sysctl-read)
(allow file-read*)
(allow file-write*
    (subpath (param "WORKSPACE"))
    (subpath (param "TEMP")))
(allow network*)"#;

#[derive(Clone, Copy, Debug, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
enum BackendSelection {
    Auto,
    Seatbelt,
    Bubblewrap,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ProgramPreset {
    Rust,
    Javascript,
    Python,
    Go,
    Build,
}

impl ProgramPreset {
    const fn candidates(self) -> &'static [&'static str] {
        match self {
            Self::Rust => &["cargo", "rustc", "rustfmt"],
            Self::Javascript => &["node", "npm", "npx", "corepack", "pnpm", "yarn", "bun"],
            Self::Python => &["python3", "pip3", "uv", "ruff", "pytest"],
            Self::Go => &["go", "gofmt"],
            Self::Build => &["make", "cmake", "ninja"],
        }
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SandboxProcessConfig {
    root: PathBuf,
    #[serde(default)]
    delegated_root: Option<PathBuf>,
    temporary_directory: PathBuf,
    backend: BackendSelection,
    allow_network: bool,
    allowed_programs: Vec<String>,
    #[serde(default)]
    program_presets: Vec<ProgramPreset>,
    environment_allowlist: Vec<String>,
    max_timeout_ms: u64,
    max_output_bytes: usize,
    max_argument_bytes: usize,
}

#[derive(Clone, Debug)]
struct SandboxProcessProvider {
    config: SandboxProcessConfig,
    root: PathBuf,
    temporary_directory: PathBuf,
    programs: BTreeMap<String, ResolvedProgram>,
    environment: BTreeMap<String, String>,
    backend: SandboxBackend,
    tasks: ManagedTasks,
}

#[derive(Clone, Debug)]
struct ResolvedProgram {
    invocation_path: PathBuf,
    canonical_target: PathBuf,
}

#[derive(Clone, Debug)]
enum SandboxBackend {
    #[cfg(target_os = "macos")]
    Seatbelt { launcher: ResolvedProgram },
    #[cfg(target_os = "linux")]
    Bubblewrap { launcher: ResolvedProgram },
}

#[derive(Debug)]
struct PreparedRequest {
    program: ResolvedProgram,
    arguments: Vec<String>,
    workspace_root: PathBuf,
    cwd: PathBuf,
    timeout_ms: u64,
}

#[derive(Clone, Copy, Debug)]
enum RequestRejection {
    ProgramNotAllowed,
    InvalidRequest,
    InvalidWorkingDirectory,
}

#[lenso::plugin(
    lifecycle,
    configuration_schema = "config.schema.json",
    validate = validate_config
)]
#[derive(Clone, Debug)]
struct SandboxProcessPlugin {
    #[config]
    config: SandboxProcessConfig,
    provider: Rc<RefCell<Option<SandboxProcessProvider>>>,
    #[tasks]
    tasks: ManagedTasks,
}

fn validate_config(config: &SandboxProcessConfig) -> Result<(), RuntimeFailure> {
    if (config.allowed_programs.is_empty() && config.program_presets.is_empty())
        || config.allowed_programs.len() > 32
    {
        return Err(invalid_plan(
            "allowed_programs and program_presets must select at least one program; allowed_programs cannot exceed 32 names",
        ));
    }
    let mut presets = BTreeSet::new();
    if config
        .program_presets
        .iter()
        .any(|preset| !presets.insert(*preset))
    {
        return Err(invalid_plan("program_presets must be unique"));
    }
    let mut programs = BTreeSet::new();
    for program in &config.allowed_programs {
        let path = Path::new(program);
        if program.is_empty()
            || program.len() > 128
            || path.components().count() != 1
            || !matches!(path.components().next(), Some(Component::Normal(_)))
            || !programs.insert(program)
        {
            return Err(invalid_plan(
                "allowed_programs must contain unique executable basenames",
            ));
        }
    }
    if config.environment_allowlist.len() > 64 {
        return Err(invalid_plan("environment_allowlist cannot exceed 64 names"));
    }
    let mut environment = BTreeSet::new();
    if config.environment_allowlist.iter().any(|name| {
        name.is_empty() || name.contains('=') || name.contains('\0') || !environment.insert(name)
    }) {
        return Err(invalid_plan(
            "environment_allowlist must contain unique valid names",
        ));
    }
    if config.environment_allowlist.iter().any(|name| {
        matches!(
            name.as_str(),
            "DBUS_SESSION_BUS_ADDRESS" | "DOCKER_HOST" | "SSH_AUTH_SOCK" | "XDG_RUNTIME_DIR"
        )
    }) {
        return Err(invalid_plan(
            "environment_allowlist cannot expose host IPC endpoints",
        ));
    }
    if !(1..=3_600_000).contains(&config.max_timeout_ms) {
        return Err(invalid_plan("max_timeout_ms must be between 1 and 3600000"));
    }
    if !(1..=1_048_576).contains(&config.max_output_bytes) {
        return Err(invalid_plan(
            "max_output_bytes must be between 1 and 1048576",
        ));
    }
    if !(1..=262_144).contains(&config.max_argument_bytes) {
        return Err(invalid_plan(
            "max_argument_bytes must be between 1 and 262144",
        ));
    }
    Ok(())
}

fn resolve_programs(
    names: &[String],
    presets: &[ProgramPreset],
) -> Result<BTreeMap<String, ResolvedProgram>, RuntimeFailure> {
    let mut programs = BTreeMap::new();
    for name in names {
        programs.insert(name.clone(), resolve_program(name)?);
    }
    for preset in presets {
        for name in preset.candidates() {
            if !programs.contains_key(*name)
                && let Some(program) = try_resolve_program(name)?
            {
                programs.insert((*name).to_owned(), program);
            }
        }
    }
    if programs.is_empty() || programs.len() > 64 {
        return Err(invalid_plan(
            "resolved programs must contain between 1 and 64 executables",
        ));
    }
    Ok(programs)
}

fn resolve_program(name: &str) -> Result<ResolvedProgram, RuntimeFailure> {
    try_resolve_program(name)?
        .ok_or_else(|| invalid_plan(format!("executable `{name}` was not found")))
}

fn try_resolve_program(name: &str) -> Result<Option<ResolvedProgram>, RuntimeFailure> {
    let search_path = env::var_os("PATH")
        .ok_or_else(|| invalid_plan("PATH is unavailable while resolving executables"))?;
    let Some(invocation_path) = env::split_paths(&search_path)
        .filter_map(|directory| fs::canonicalize(directory).ok())
        .map(|directory| directory.join(name))
        .find(|candidate| candidate.is_file())
    else {
        return Ok(None);
    };
    let canonical_target = fs::canonicalize(&invocation_path)
        .map_err(|error| invalid_plan(format!("executable `{name}` is unavailable: {error}")))?;
    Ok(Some(ResolvedProgram {
        invocation_path,
        canonical_target,
    }))
}

fn resolve_absolute_program(path: &Path) -> Result<ResolvedProgram, RuntimeFailure> {
    let canonical_target = fs::canonicalize(path).map_err(|error| {
        invalid_plan(format!(
            "sandbox backend `{}` is unavailable: {error}",
            path.display()
        ))
    })?;
    if !canonical_target.is_file() {
        return Err(invalid_plan(format!(
            "sandbox backend `{}` is not a file",
            path.display()
        )));
    }
    Ok(ResolvedProgram {
        invocation_path: path.to_path_buf(),
        canonical_target,
    })
}

fn resolve_backend(selection: BackendSelection) -> Result<SandboxBackend, RuntimeFailure> {
    match selection {
        BackendSelection::Auto => resolve_automatic_backend(),
        BackendSelection::Seatbelt => resolve_seatbelt_backend(),
        BackendSelection::Bubblewrap => resolve_bubblewrap_backend(),
    }
}

#[cfg(target_os = "macos")]
fn resolve_automatic_backend() -> Result<SandboxBackend, RuntimeFailure> {
    resolve_seatbelt_backend()
}

#[cfg(target_os = "linux")]
fn resolve_automatic_backend() -> Result<SandboxBackend, RuntimeFailure> {
    resolve_bubblewrap_backend()
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn resolve_automatic_backend() -> Result<SandboxBackend, RuntimeFailure> {
    Err(invalid_plan(
        "automatic sandbox backend is supported only on macOS and Linux",
    ))
}

#[cfg(target_os = "macos")]
fn resolve_seatbelt_backend() -> Result<SandboxBackend, RuntimeFailure> {
    Ok(SandboxBackend::Seatbelt {
        launcher: resolve_absolute_program(Path::new("/usr/bin/sandbox-exec"))?,
    })
}

#[cfg(not(target_os = "macos"))]
fn resolve_seatbelt_backend() -> Result<SandboxBackend, RuntimeFailure> {
    Err(invalid_plan("Seatbelt sandboxing requires macOS"))
}

#[cfg(target_os = "linux")]
fn resolve_bubblewrap_backend() -> Result<SandboxBackend, RuntimeFailure> {
    Ok(SandboxBackend::Bubblewrap {
        launcher: resolve_program("bwrap")?,
    })
}

#[cfg(not(target_os = "linux"))]
fn resolve_bubblewrap_backend() -> Result<SandboxBackend, RuntimeFailure> {
    Err(invalid_plan("bubblewrap sandboxing requires Linux"))
}

impl Lifecycle for SandboxProcessPlugin {
    async fn prepare(&self, _context: PrepareContext) -> Result<(), RuntimeFailure> {
        validate_config(&self.config)?;
        let root = fs::canonicalize(&self.config.root)
            .map_err(|error| invalid_plan(format!("sandbox root is unavailable: {error}")))?;
        if !root.is_dir() || root.parent().is_none() {
            return Err(invalid_plan(
                "sandbox root must be a directory narrower than the filesystem root",
            ));
        }
        if workspace_conflicts_with_private_tmp(&root) {
            return Err(invalid_plan(PRIVATE_TMP_WORKSPACE_CONFLICT));
        }
        fs::create_dir_all(&self.config.temporary_directory).map_err(|error| {
            invalid_plan(format!(
                "sandbox temporary directory is unavailable: {error}"
            ))
        })?;
        protect_directory(&self.config.temporary_directory)?;
        let temporary_directory =
            fs::canonicalize(&self.config.temporary_directory).map_err(|error| {
                invalid_plan(format!(
                    "sandbox temporary directory is unavailable: {error}"
                ))
            })?;
        if !temporary_directory.is_dir() || root.starts_with(&temporary_directory) {
            return Err(invalid_plan(
                "sandbox temporary directory cannot equal or contain the Workspace root",
            ));
        }
        let backend = resolve_backend(self.config.backend)?;
        let programs =
            resolve_programs(&self.config.allowed_programs, &self.config.program_presets)?;
        let environment = self
            .config
            .environment_allowlist
            .iter()
            .filter_map(|name| env::var(name).ok().map(|value| (name.clone(), value)))
            .collect();
        let provider = SandboxProcessProvider {
            config: self.config.clone(),
            root,
            temporary_directory,
            programs,
            environment,
            backend,
            tasks: self.tasks.clone(),
        };
        provider.probe_backend().await?;
        self.provider.replace(Some(provider));
        Ok(())
    }
}

#[cfg(unix)]
fn protect_directory(path: &Path) -> Result<(), RuntimeFailure> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
        invalid_plan(format!(
            "sandbox temporary directory permissions could not be restricted: {error}"
        ))
    })
}

#[cfg(not(unix))]
fn protect_directory(_path: &Path) -> Result<(), RuntimeFailure> {
    Ok(())
}

impl ProcessProvider for SandboxProcessProvider {
    fn catalog(
        &self,
        _context: InvocationContext,
        _request: CatalogRequest,
    ) -> lenso_kernel::NativeRequestFuture<process_contract::ProcessCatalog> {
        Box::pin(ready(Ok(Ok(CatalogResponse {
            programs: self
                .programs
                .keys()
                .map(|name| CatalogResponseProgramsItem { name: name.clone() })
                .collect(),
        }))))
    }

    fn run(
        &self,
        context: InvocationContext,
        request: RunRequest,
    ) -> lenso_kernel::NativeRequestFuture<ProcessRun> {
        let provider = self.clone();
        Box::pin(async move { provider.run_process(context, request).await })
    }

    fn run_stream(
        &self,
        context: InvocationContext,
        request: RunStreamRequest,
    ) -> futures::future::LocalBoxFuture<
        'static,
        Result<Box<dyn NativeStreamSession>, ProcessRunStreamInvocationError>,
    > {
        let provider = self.clone();
        Box::pin(async move {
            let (stream, mut channel) =
                ProviderStream::<process_contract::ProcessRunStream>::channel(&context, 8);
            let tasks = provider.tasks.clone();
            tasks
                .spawn_local(async move {
                    let result = provider
                        .run_process_stream(context, request, &mut channel)
                        .await;
                    let _ = channel.complete(result).await;
                })
                .map_err(|error| {
                    ProcessRunStreamInvocationError::Runtime(RuntimeFailure::PluginFailure {
                        detail: format!("sandbox process stream task failed to start: {error:?}"),
                    })
                })?;
            Ok(Box::new(stream) as Box<dyn NativeStreamSession>)
        })
    }
}

#[lenso::provides(process_contract::Process)]
impl ProcessProvider for SandboxProcessPlugin {
    fn catalog(
        &self,
        context: InvocationContext,
        request: CatalogRequest,
    ) -> lenso_kernel::NativeRequestFuture<process_contract::ProcessCatalog> {
        match self.provider.borrow().clone() {
            Some(provider) => provider.catalog(context, request),
            None => Box::pin(ready(Err(RuntimeFailure::Unavailable {
                capability: process_contract::CAPABILITY_ID,
            }))),
        }
    }

    fn run(
        &self,
        context: InvocationContext,
        request: RunRequest,
    ) -> lenso_kernel::NativeRequestFuture<ProcessRun> {
        match self.provider.borrow().clone() {
            Some(provider) => provider.run(context, request),
            None => Box::pin(ready(Err(RuntimeFailure::Unavailable {
                capability: process_contract::CAPABILITY_ID,
            }))),
        }
    }

    fn run_stream(
        &self,
        context: InvocationContext,
        request: RunStreamRequest,
    ) -> futures::future::LocalBoxFuture<
        'static,
        Result<Box<dyn NativeStreamSession>, ProcessRunStreamInvocationError>,
    > {
        match self.provider.borrow().clone() {
            Some(provider) => provider.run_stream(context, request),
            None => Box::pin(ready(Err(ProcessRunStreamInvocationError::Runtime(
                RuntimeFailure::Unavailable {
                    capability: process_contract::CAPABILITY_ID,
                },
            )))),
        }
    }
}

impl SandboxProcessProvider {
    async fn probe_backend(&self) -> Result<(), RuntimeFailure> {
        let target = if cfg!(target_os = "macos") {
            resolve_absolute_program(Path::new("/usr/bin/true"))?
        } else {
            resolve_program("true")?
        };
        let temporary = self.invocation_temporary_directory()?;
        let mut command =
            self.sandbox_command(&target, &[], &self.root, &self.root, temporary.path())?;
        command.stdout(Stdio::null()).stderr(Stdio::piped());
        let output = tokio::time::timeout(Duration::from_secs(5), command.output())
            .await
            .map_err(|_| invalid_plan("sandbox backend readiness probe timed out"))?
            .map_err(|error| invalid_plan(format!("sandbox backend failed to start: {error}")))?;
        if !output.status.success() {
            return Err(invalid_plan(format!(
                "sandbox backend readiness probe failed: {}",
                bounded_text(&output.stderr, 4096)
            )));
        }
        Ok(())
    }

    async fn run_process(
        &self,
        context: InvocationContext,
        request: RunRequest,
    ) -> Result<Result<RunResponse, RunError>, RuntimeFailure> {
        let prepared = match self.prepare_request(
            &context,
            &request.program,
            request.arguments,
            &request.cwd,
            &request.timeout_ms,
        )? {
            Ok(prepared) => prepared,
            Err(rejection) => return Ok(Err(run_error(rejection))),
        };
        let temporary = self.invocation_temporary_directory()?;
        let mut command = self.sandbox_command(
            &prepared.program,
            &prepared.arguments,
            &prepared.workspace_root,
            &prepared.cwd,
            temporary.path(),
        )?;
        configure_observed_command(&mut command);
        let child = command
            .spawn()
            .map_err(|error| RuntimeFailure::PluginFailure {
                detail: format!("failed to spawn sandboxed process: {error}"),
            })?;
        let result = observe_child(
            child,
            context,
            prepared.timeout_ms,
            self.config.max_output_bytes,
        )
        .await;
        drop(temporary);
        result
    }

    async fn run_process_stream(
        &self,
        context: InvocationContext,
        request: RunStreamRequest,
        channel: &mut ProviderStreamChannel<process_contract::ProcessRunStream>,
    ) -> PluginResult<(), RunStreamError> {
        let prepared = self
            .prepare_request(
                &context,
                &request.program,
                request.arguments,
                &request.cwd,
                &request.timeout_ms,
            )
            .map_err(PluginError::runtime)?
            .map_err(|rejection| PluginError::domain(run_stream_error(rejection)))?;
        let temporary = self
            .invocation_temporary_directory()
            .map_err(PluginError::runtime)?;
        let mut command = self
            .sandbox_command(
                &prepared.program,
                &prepared.arguments,
                &prepared.workspace_root,
                &prepared.cwd,
                temporary.path(),
            )
            .map_err(PluginError::runtime)?;
        configure_observed_command(&mut command);
        let child = command.spawn().map_err(|error| {
            PluginError::runtime(RuntimeFailure::PluginFailure {
                detail: format!("failed to spawn sandboxed process: {error}"),
            })
        })?;
        let result = observe_child_stream(
            child,
            context,
            prepared.timeout_ms,
            self.config.max_output_bytes,
            channel,
        )
        .await;
        drop(temporary);
        result
    }

    fn prepare_request(
        &self,
        context: &InvocationContext,
        program_name: &str,
        arguments: Vec<String>,
        cwd: &str,
        timeout: &str,
    ) -> Result<Result<PreparedRequest, RequestRejection>, RuntimeFailure> {
        let Some(program) = self.programs.get(program_name) else {
            return Ok(Err(RequestRejection::ProgramNotAllowed));
        };
        verify_program_identity(program, "configured process executable")?;
        self.backend.verify_identity()?;
        let timeout_ms = timeout
            .parse::<u64>()
            .map_err(|_| RuntimeFailure::ProtocolViolation {
                capability: process_contract::CAPABILITY_ID,
            })?;
        let argument_bytes = arguments
            .iter()
            .try_fold(0usize, |total, argument| total.checked_add(argument.len()));
        if timeout_ms == 0
            || timeout_ms > self.config.max_timeout_ms
            || program_name.len() > 128
            || cwd.len() > 4096
            || arguments.len() > 128
            || arguments.iter().any(|argument| argument.len() > 16_384)
            || argument_bytes.is_none_or(|bytes| bytes > self.config.max_argument_bytes)
        {
            return Ok(Err(RequestRejection::InvalidRequest));
        }
        let root = self.invocation_root(context)?;
        let Some(cwd) = resolve_cwd(&root, cwd) else {
            return Ok(Err(RequestRejection::InvalidWorkingDirectory));
        };
        Ok(Ok(PreparedRequest {
            program: program.clone(),
            arguments,
            workspace_root: root,
            cwd,
            timeout_ms,
        }))
    }

    fn invocation_temporary_directory(&self) -> Result<tempfile::TempDir, RuntimeFailure> {
        let current = fs::canonicalize(&self.temporary_directory).map_err(|error| {
            RuntimeFailure::PluginFailure {
                detail: format!("sandbox temporary directory is unavailable: {error}"),
            }
        })?;
        if current != self.temporary_directory {
            return Err(RuntimeFailure::PluginFailure {
                detail: "sandbox temporary directory identity changed after startup".to_owned(),
            });
        }
        tempfile::Builder::new()
            .prefix("invocation-")
            .tempdir_in(&self.temporary_directory)
            .map_err(|error| RuntimeFailure::PluginFailure {
                detail: format!("failed to create sandbox temporary directory: {error}"),
            })
    }

    fn sandbox_command(
        &self,
        target: &ResolvedProgram,
        arguments: &[String],
        workspace_root: &Path,
        cwd: &Path,
        temporary: &Path,
    ) -> Result<tokio::process::Command, RuntimeFailure> {
        verify_program_identity(target, "sandbox target executable")?;
        self.backend.verify_identity()?;
        let mut command = match &self.backend {
            #[cfg(target_os = "macos")]
            SandboxBackend::Seatbelt { launcher } => {
                let mut command = tokio::process::Command::new(&launcher.invocation_path);
                command
                    .arg("-D")
                    .arg(format!("WORKSPACE={}", workspace_root.display()))
                    .arg("-D")
                    .arg(format!("TEMP={}", temporary.display()))
                    .arg("-p")
                    .arg(if self.config.allow_network {
                        SEATBELT_ALLOW_NETWORK
                    } else {
                        SEATBELT_DENY_NETWORK
                    })
                    .arg(&target.invocation_path)
                    .args(arguments)
                    .current_dir(cwd);
                command
            }
            #[cfg(target_os = "linux")]
            SandboxBackend::Bubblewrap { launcher } => {
                let mut command = tokio::process::Command::new(&launcher.invocation_path);
                command.args(["--die-with-parent", "--new-session", "--unshare-all"]);
                if self.config.allow_network {
                    command.arg("--share-net");
                }
                command
                    .args(["--ro-bind", "/", "/"])
                    .args(["--dev", "/dev"])
                    .args(["--proc", "/proc"])
                    // Keep this before the workspace bind: a workspace nested below
                    // `/tmp` must be mounted back into the private temporary tree.
                    .arg("--bind")
                    .arg(temporary)
                    .arg("/tmp")
                    .arg("--bind")
                    .arg(workspace_root)
                    .arg(workspace_root)
                    .arg("--chdir")
                    .arg(cwd)
                    .arg("--")
                    .arg(&target.invocation_path)
                    .args(arguments)
                    .current_dir(workspace_root);
                command
            }
        };
        command
            .env_clear()
            .envs(&self.environment)
            .env("TMPDIR", sandbox_temporary_environment(temporary));
        Ok(command)
    }

    fn invocation_root(&self, context: &InvocationContext) -> Result<PathBuf, RuntimeFailure> {
        let root = fs::canonicalize(&self.root).map_err(|error| RuntimeFailure::PluginFailure {
            detail: format!("sandbox root is unavailable: {error}"),
        })?;
        if root != self.root {
            return Err(RuntimeFailure::PluginFailure {
                detail: "sandbox root identity changed after startup".to_owned(),
            });
        }
        if workspace_conflicts_with_private_tmp(&root) {
            return Err(RuntimeFailure::PluginFailure {
                detail: PRIVATE_TMP_WORKSPACE_CONFLICT.to_owned(),
            });
        }
        let Some(scope) = context
            .typed_extension::<WorkspaceScope>()
            .map_err(|error| RuntimeFailure::PluginFailure {
                detail: format!("Workspace scope is invalid: {error}"),
            })?
        else {
            return Ok(root);
        };
        let scoped = fs::canonicalize(&scope.absolute_path).map_err(|error| {
            RuntimeFailure::PluginFailure {
                detail: format!("scoped Workspace is unavailable: {error}"),
            }
        })?;
        if workspace_conflicts_with_private_tmp(&scoped) {
            return Err(RuntimeFailure::PluginFailure {
                detail: PRIVATE_TMP_WORKSPACE_CONFLICT.to_owned(),
            });
        }
        if scoped == root {
            return Ok(root);
        }
        let delegated =
            self.config
                .delegated_root
                .as_ref()
                .ok_or_else(|| RuntimeFailure::PluginFailure {
                    detail: "Workspace scope is outside the configured sandbox root".to_owned(),
                })?;
        let delegated =
            fs::canonicalize(delegated).map_err(|error| RuntimeFailure::PluginFailure {
                detail: format!("delegated Workspace root is unavailable: {error}"),
            })?;
        if !scoped.starts_with(&delegated) || !scoped.join(".git").exists() {
            return Err(RuntimeFailure::PluginFailure {
                detail: "Workspace scope is not an authorized delegated Git worktree".to_owned(),
            });
        }
        Ok(scoped)
    }
}

#[cfg(target_os = "linux")]
fn workspace_conflicts_with_private_tmp(root: &Path) -> bool {
    root == Path::new("/tmp")
}

#[cfg(not(target_os = "linux"))]
fn workspace_conflicts_with_private_tmp(_root: &Path) -> bool {
    false
}

impl SandboxBackend {
    fn verify_identity(&self) -> Result<(), RuntimeFailure> {
        let launcher = match self {
            #[cfg(target_os = "macos")]
            Self::Seatbelt { launcher } => launcher,
            #[cfg(target_os = "linux")]
            Self::Bubblewrap { launcher } => launcher,
        };
        verify_program_identity(launcher, "sandbox backend")
    }
}

#[cfg(target_os = "linux")]
fn sandbox_temporary_environment(_temporary: &Path) -> &Path {
    Path::new("/tmp")
}

#[cfg(not(target_os = "linux"))]
fn sandbox_temporary_environment(temporary: &Path) -> &Path {
    temporary
}

fn verify_program_identity(program: &ResolvedProgram, label: &str) -> Result<(), RuntimeFailure> {
    let current = fs::canonicalize(&program.invocation_path).map_err(|error| {
        RuntimeFailure::PluginFailure {
            detail: format!("{label} is unavailable: {error}"),
        }
    })?;
    if current != program.canonical_target {
        return Err(RuntimeFailure::PluginFailure {
            detail: format!("{label} identity changed after startup"),
        });
    }
    Ok(())
}

fn configure_observed_command(command: &mut tokio::process::Command) {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.as_std_mut().process_group(0);
    }
}

fn run_error(rejection: RequestRejection) -> RunError {
    match rejection {
        RequestRejection::ProgramNotAllowed => RunError::ProgramNotAllowed,
        RequestRejection::InvalidRequest => RunError::InvalidRequest,
        RequestRejection::InvalidWorkingDirectory => RunError::InvalidWorkingDirectory,
    }
}

fn run_stream_error(rejection: RequestRejection) -> RunStreamError {
    match rejection {
        RequestRejection::ProgramNotAllowed => RunStreamError::ProgramNotAllowed,
        RequestRejection::InvalidRequest => RunStreamError::InvalidRequest,
        RequestRejection::InvalidWorkingDirectory => RunStreamError::InvalidWorkingDirectory,
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one select loop must retain child status and both pipe states atomically"
)]
async fn observe_child_stream(
    mut child: Child,
    context: InvocationContext,
    timeout_ms: u64,
    max_output_bytes: usize,
    channel: &mut ProviderStreamChannel<process_contract::ProcessRunStream>,
) -> PluginResult<(), RunStreamError> {
    let process_id = child.id();
    let mut guard = ProcessGroupGuard::new(process_id);
    let mut stdout = child.stdout.take().ok_or_else(|| {
        PluginError::runtime(RuntimeFailure::Internal {
            detail: "spawned sandbox process has no stdout pipe".to_owned(),
        })
    })?;
    let mut stderr = child.stderr.take().ok_or_else(|| {
        PluginError::runtime(RuntimeFailure::Internal {
            detail: "spawned sandbox process has no stderr pipe".to_owned(),
        })
    })?;
    let started = Instant::now();
    let cancellation = context.cancellation();
    let request_id = context.request_id();
    let mut timeout = Box::pin(tokio::time::sleep(Duration::from_millis(timeout_ms)));
    let mut cancelled = Box::pin(cancellation.cancelled());
    let mut total_bytes = 0usize;
    let mut stdout_open = true;
    let mut stderr_open = true;
    let mut status = None;
    let mut stdout_buffer = Box::new([0_u8; 8192]);
    let mut stderr_buffer = Box::new([0_u8; 8192]);

    while status.is_none() || stdout_open || stderr_open {
        tokio::select! {
            () = &mut cancelled => {
                terminate(&mut child, process_id).await;
                guard.disarm();
                return Err(PluginError::runtime(RuntimeFailure::Cancelled { request_id }));
            }
            () = &mut timeout => {
                terminate(&mut child, process_id).await;
                guard.disarm();
                return Err(PluginError::domain(RunStreamError::Timeout));
            }
            result = stdout.read(&mut stdout_buffer[..]), if stdout_open => {
                let read = result.map_err(|error| PluginError::runtime(RuntimeFailure::PluginFailure {
                    detail: format!("failed to read sandbox process stdout: {error}"),
                }))?;
                if read == 0 {
                    stdout_open = false;
                } else {
                    total_bytes = total_bytes.saturating_add(read);
                    if total_bytes > max_output_bytes {
                        terminate(&mut child, process_id).await;
                        guard.disarm();
                        return Err(PluginError::domain(RunStreamError::OutputLimitExceeded));
                    }
                    channel.send(RunStreamResponse {
                        kind: RunStreamResponseKind::Stdout,
                        content: String::from_utf8_lossy(&stdout_buffer[..read]).into_owned(),
                        exit_code: None,
                        duration_ms: None,
                    }).await.map_err(PluginError::runtime)?;
                }
            }
            result = stderr.read(&mut stderr_buffer[..]), if stderr_open => {
                let read = result.map_err(|error| PluginError::runtime(RuntimeFailure::PluginFailure {
                    detail: format!("failed to read sandbox process stderr: {error}"),
                }))?;
                if read == 0 {
                    stderr_open = false;
                } else {
                    total_bytes = total_bytes.saturating_add(read);
                    if total_bytes > max_output_bytes {
                        terminate(&mut child, process_id).await;
                        guard.disarm();
                        return Err(PluginError::domain(RunStreamError::OutputLimitExceeded));
                    }
                    channel.send(RunStreamResponse {
                        kind: RunStreamResponseKind::Stderr,
                        content: String::from_utf8_lossy(&stderr_buffer[..read]).into_owned(),
                        exit_code: None,
                        duration_ms: None,
                    }).await.map_err(PluginError::runtime)?;
                }
            }
            result = child.wait(), if status.is_none() => {
                status = Some(result.map_err(|error| PluginError::runtime(RuntimeFailure::PluginFailure {
                    detail: format!("failed to wait for sandbox process: {error}"),
                }))?);
            }
        }
    }
    guard.disarm();
    let status = status.expect("sandbox process loop ends only after observing status");
    let Some(exit_code) = status.code() else {
        return Err(PluginError::domain(RunStreamError::Terminated));
    };
    channel
        .send(RunStreamResponse {
            kind: RunStreamResponseKind::Completed,
            content: String::new(),
            exit_code: Some(exit_code.to_string()),
            duration_ms: Some(
                u64::try_from(started.elapsed().as_millis())
                    .unwrap_or(u64::MAX)
                    .to_string(),
            ),
        })
        .await
        .map_err(PluginError::runtime)
}

async fn observe_child(
    mut child: Child,
    context: InvocationContext,
    timeout_ms: u64,
    max_output_bytes: usize,
) -> Result<Result<RunResponse, RunError>, RuntimeFailure> {
    let process_id = child.id();
    let mut guard = ProcessGroupGuard::new(process_id);
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| RuntimeFailure::Internal {
            detail: "spawned sandbox process has no stdout pipe".to_owned(),
        })?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| RuntimeFailure::Internal {
            detail: "spawned sandbox process has no stderr pipe".to_owned(),
        })?;
    let started = Instant::now();
    let cancellation = context.cancellation();
    let request_id = context.request_id();
    let mut timeout = Box::pin(tokio::time::sleep(Duration::from_millis(timeout_ms)));
    let mut cancelled = Box::pin(cancellation.cancelled());
    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    let mut stdout_open = true;
    let mut stderr_open = true;
    let mut status = None;
    let mut stdout_buffer = Box::new([0_u8; 8192]);
    let mut stderr_buffer = Box::new([0_u8; 8192]);

    while status.is_none() || stdout_open || stderr_open {
        tokio::select! {
            () = &mut cancelled => {
                terminate(&mut child, process_id).await;
                guard.disarm();
                return Err(RuntimeFailure::Cancelled { request_id });
            }
            () = &mut timeout => {
                terminate(&mut child, process_id).await;
                guard.disarm();
                return Ok(Err(RunError::Timeout));
            }
            result = stdout.read(&mut stdout_buffer[..]), if stdout_open => {
                let read = result.map_err(|error| RuntimeFailure::PluginFailure {
                    detail: format!("failed to read sandbox process stdout: {error}"),
                })?;
                if read == 0 {
                    stdout_open = false;
                } else if !append_bounded(
                    &mut stdout_bytes,
                    &stdout_buffer[..read],
                    stderr_bytes.len(),
                    max_output_bytes,
                ) {
                    terminate(&mut child, process_id).await;
                    guard.disarm();
                    return Ok(Err(RunError::OutputLimitExceeded));
                }
            }
            result = stderr.read(&mut stderr_buffer[..]), if stderr_open => {
                let read = result.map_err(|error| RuntimeFailure::PluginFailure {
                    detail: format!("failed to read sandbox process stderr: {error}"),
                })?;
                if read == 0 {
                    stderr_open = false;
                } else if !append_bounded(
                    &mut stderr_bytes,
                    &stderr_buffer[..read],
                    stdout_bytes.len(),
                    max_output_bytes,
                ) {
                    terminate(&mut child, process_id).await;
                    guard.disarm();
                    return Ok(Err(RunError::OutputLimitExceeded));
                }
            }
            result = child.wait(), if status.is_none() => {
                status = Some(result.map_err(|error| RuntimeFailure::PluginFailure {
                    detail: format!("failed to wait for sandbox process: {error}"),
                })?);
            }
        }
    }
    guard.disarm();
    let status = status.expect("sandbox process loop ends only after observing status");
    Ok(completed_response(
        status,
        &stdout_bytes,
        &stderr_bytes,
        started,
        max_output_bytes,
    ))
}

fn completed_response(
    status: ExitStatus,
    stdout_bytes: &[u8],
    stderr_bytes: &[u8],
    started: Instant,
    max_output_bytes: usize,
) -> Result<RunResponse, RunError> {
    let Some(exit_code) = status.code() else {
        return Err(RunError::Terminated);
    };
    let stdout = String::from_utf8_lossy(stdout_bytes).into_owned();
    let stderr = String::from_utf8_lossy(stderr_bytes).into_owned();
    if stdout
        .len()
        .checked_add(stderr.len())
        .is_none_or(|length| length > max_output_bytes)
    {
        return Err(RunError::OutputLimitExceeded);
    }
    Ok(RunResponse {
        exit_code: exit_code.to_string(),
        stdout,
        stderr,
        duration_ms: u64::try_from(started.elapsed().as_millis())
            .unwrap_or(u64::MAX)
            .to_string(),
    })
}

fn resolve_cwd(root: &Path, requested: &str) -> Option<PathBuf> {
    let relative = Path::new(requested);
    if relative.is_absolute()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return None;
    }
    let candidate = fs::canonicalize(root.join(relative)).ok()?;
    (candidate.starts_with(root) && candidate.is_dir()).then_some(candidate)
}

fn append_bounded(target: &mut Vec<u8>, bytes: &[u8], other: usize, maximum: usize) -> bool {
    let Some(total) = target
        .len()
        .checked_add(other)
        .and_then(|total| total.checked_add(bytes.len()))
    else {
        return false;
    };
    if total > maximum {
        return false;
    }
    target.extend_from_slice(bytes);
    true
}

fn bounded_text(bytes: &[u8], maximum: usize) -> String {
    String::from_utf8_lossy(&bytes[..bytes.len().min(maximum)]).into_owned()
}

async fn terminate(child: &mut Child, process_id: Option<u32>) {
    terminate_group(process_id);
    let _ = child.start_kill();
    let _ = child.wait().await;
}

#[cfg(unix)]
fn terminate_group(process_id: Option<u32>) {
    use nix::{
        sys::signal::{Signal, kill},
        unistd::Pid,
    };

    if let Some(process_id) = process_id.and_then(|id| i32::try_from(id).ok()) {
        let _ = kill(Pid::from_raw(-process_id), Signal::SIGKILL);
    }
}

#[cfg(not(unix))]
fn terminate_group(_process_id: Option<u32>) {}

#[derive(Debug)]
struct ProcessGroupGuard {
    process_id: Option<u32>,
}

impl ProcessGroupGuard {
    fn new(process_id: Option<u32>) -> Self {
        Self { process_id }
    }

    fn disarm(&mut self) {
        self.process_id = None;
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        terminate_group(self.process_id);
    }
}

fn invalid_plan(detail: impl Into<String>) -> RuntimeFailure {
    RuntimeFailure::InvalidResolvedPlan {
        detail: detail.into(),
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use lenso_kernel::{CancellationToken, NativeStreamItem};
    use nix::{errno::Errno, sys::signal::kill, unistd::Pid};

    fn provider(root: &Path, temporary: &Path, allow_network: bool) -> SandboxProcessProvider {
        let root = fs::canonicalize(root).unwrap();
        let temporary_directory = fs::canonicalize(temporary).unwrap();
        let shell = resolve_absolute_program(Path::new("/bin/sh")).unwrap();
        SandboxProcessProvider {
            config: SandboxProcessConfig {
                root: root.clone(),
                delegated_root: None,
                temporary_directory: temporary_directory.clone(),
                backend: BackendSelection::Seatbelt,
                allow_network,
                allowed_programs: vec!["sh".to_owned()],
                program_presets: Vec::new(),
                environment_allowlist: Vec::new(),
                max_timeout_ms: 5_000,
                max_output_bytes: 16_384,
                max_argument_bytes: 16_384,
            },
            root,
            temporary_directory,
            programs: BTreeMap::from([("sh".to_owned(), shell)]),
            environment: BTreeMap::new(),
            backend: resolve_backend(BackendSelection::Seatbelt).unwrap(),
            tasks: ManagedTasks::default(),
        }
    }

    fn context(cancellation: CancellationToken) -> InvocationContext {
        InvocationContext::new(31, None, cancellation)
    }

    fn request(script: &str, arguments: &[&Path]) -> RunRequest {
        let mut command_arguments = vec!["-c".to_owned(), script.to_owned(), "sh".to_owned()];
        command_arguments.extend(
            arguments
                .iter()
                .map(|path| path.to_string_lossy().into_owned()),
        );
        RunRequest {
            program: "sh".to_owned(),
            arguments: command_arguments,
            cwd: ".".to_owned(),
            timeout_ms: "2000".to_owned(),
        }
    }

    #[test]
    fn typed_presets_expand_installed_programs_without_a_shell() {
        let programs = resolve_programs(
            &[],
            &[
                ProgramPreset::Rust,
                ProgramPreset::Javascript,
                ProgramPreset::Python,
                ProgramPreset::Go,
                ProgramPreset::Build,
            ],
        )
        .unwrap();

        assert!(programs.contains_key("cargo"));
        assert!(!programs.contains_key("sh"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn seatbelt_allows_workspace_writes_and_denies_external_writes() {
        let workspace = tempfile::tempdir().unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        let inside = workspace.path().join("inside.txt");
        let provider = provider(workspace.path(), temporary.path(), false);
        provider.probe_backend().await.unwrap();

        let response = provider
            .run_process(
                context(CancellationToken::new()),
                request(
                    "printf inside > \"$1\"; printf escaped > \"$2\"",
                    &[&inside, outside.path()],
                ),
            )
            .await
            .unwrap()
            .unwrap();

        assert_ne!(response.exit_code, "0");
        assert_eq!(fs::read_to_string(inside).unwrap(), "inside");
        assert_eq!(fs::read(outside.path()).unwrap(), Vec::<u8>::new());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn seatbelt_denies_network_and_preserves_timeout_and_cancellation() {
        let workspace = tempfile::tempdir().unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let provider = provider(workspace.path(), temporary.path(), false);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        let network = provider.run_process(
            context(CancellationToken::new()),
            request(
                "exec /usr/bin/curl --silent --show-error --connect-timeout 1 \"$1\"",
                &[Path::new(&format!("http://{address}"))],
            ),
        );
        let accept = tokio::time::timeout(Duration::from_millis(500), listener.accept());
        let (network, accepted) = futures::future::join(network, accept).await;
        assert_ne!(network.unwrap().unwrap().exit_code, "0");
        assert!(
            accepted.is_err(),
            "sandboxed process reached the host listener"
        );

        let mut timed = request("sleep 30", &[]);
        timed.timeout_ms = "20".to_owned();
        assert_eq!(
            provider
                .run_process(context(CancellationToken::new()), timed)
                .await,
            Ok(Err(RunError::Timeout))
        );

        let cancellation = CancellationToken::new();
        let running = provider.run_process(
            context(cancellation.clone()),
            request("sleep 30 & echo $! > child.pid; wait", &[]),
        );
        let child_pid_path = workspace.path().join("child.pid");
        let cancel = async {
            for _ in 0..100 {
                if child_pid_path.exists() {
                    cancellation.cancel();
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("sandboxed descendant did not publish its process ID");
        };
        let (result, ()) = tokio::join!(running, cancel);
        assert!(matches!(
            result,
            Err(RuntimeFailure::Cancelled { request_id: 31 })
        ));
        let child_pid = fs::read_to_string(child_pid_path)
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();
        let mut outcome = kill(Pid::from_raw(child_pid), None);
        for _ in 0..20 {
            if matches!(outcome, Err(Errno::ESRCH)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
            outcome = kill(Pid::from_raw(child_pid), None);
        }
        assert!(matches!(outcome, Err(Errno::ESRCH)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn seatbelt_streams_output_before_completion() {
        let workspace = tempfile::tempdir().unwrap();
        let temporary = tempfile::tempdir().unwrap();
        let provider = provider(workspace.path(), temporary.path(), false);
        let invocation = context(CancellationToken::new());
        let (stream, mut channel) =
            ProviderStream::<process_contract::ProcessRunStream>::channel(&invocation, 8);
        let request = request(
            "printf first; sleep 0.05; printf second; printf err >&2",
            &[],
        );
        let stream_request = RunStreamRequest {
            program: request.program,
            arguments: request.arguments,
            cwd: request.cwd,
            timeout_ms: request.timeout_ms,
        };
        let producer = async {
            let result = provider
                .run_process_stream(invocation, stream_request, &mut channel)
                .await;
            channel.complete(result).await.unwrap();
        };
        let consumer = async {
            let mut messages = Vec::new();
            loop {
                match stream.receive().await.unwrap() {
                    NativeStreamItem::Message(message) => {
                        messages.push(*message.downcast::<RunStreamResponse>().unwrap());
                    }
                    NativeStreamItem::PeerHalfClosed => {}
                    NativeStreamItem::Terminal(Ok(())) => return messages,
                    NativeStreamItem::Terminal(Err(_)) => panic!("stream should succeed"),
                }
            }
        };
        let ((), messages) = futures::future::join(producer, consumer).await;
        assert_eq!(
            messages.first().unwrap().kind,
            RunStreamResponseKind::Stdout
        );
        assert_eq!(messages.first().unwrap().content, "first");
        assert!(messages.iter().any(|message| {
            message.kind == RunStreamResponseKind::Stderr && message.content == "err"
        }));
        assert_eq!(
            messages.last().unwrap().kind,
            RunStreamResponseKind::Completed
        );
    }
}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;
    use lenso_kernel::CancellationToken;

    fn provider(root: &Path, temporary: &Path) -> SandboxProcessProvider {
        let root = fs::canonicalize(root).unwrap();
        let temporary_directory = fs::canonicalize(temporary).unwrap();
        let shell = resolve_absolute_program(Path::new("/bin/sh")).unwrap();
        SandboxProcessProvider {
            config: SandboxProcessConfig {
                root: root.clone(),
                delegated_root: None,
                temporary_directory: temporary_directory.clone(),
                backend: BackendSelection::Bubblewrap,
                allow_network: false,
                allowed_programs: vec!["sh".to_owned()],
                program_presets: Vec::new(),
                environment_allowlist: Vec::new(),
                max_timeout_ms: 5_000,
                max_output_bytes: 16_384,
                max_argument_bytes: 16_384,
            },
            root,
            temporary_directory,
            programs: BTreeMap::from([("sh".to_owned(), shell)]),
            environment: BTreeMap::new(),
            backend: resolve_backend(BackendSelection::Bubblewrap).unwrap(),
            tasks: ManagedTasks::default(),
        }
    }

    #[test]
    fn private_tmp_mount_cannot_also_be_the_workspace_root() {
        assert!(workspace_conflicts_with_private_tmp(Path::new("/tmp")));
        assert!(!workspace_conflicts_with_private_tmp(Path::new(
            "/tmp/workspace"
        )));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn bubblewrap_keeps_a_workspace_below_its_private_tmp_mount_visible() {
        let parent = tempfile::tempdir_in("/tmp").unwrap();
        let workspace = parent.path().join("workspace");
        let temporary = parent.path().join("sandbox-tmp");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&temporary).unwrap();
        let provider = provider(&workspace, &temporary);

        provider.probe_backend().await.unwrap();
        let response = provider
            .run_process(
                InvocationContext::new(31, None, CancellationToken::new()),
                RunRequest {
                    program: "sh".to_owned(),
                    arguments: vec!["-c".to_owned(), "printf visible > inside.txt".to_owned()],
                    cwd: ".".to_owned(),
                    timeout_ms: "2000".to_owned(),
                },
            )
            .await
            .unwrap()
            .unwrap();

        assert_eq!(response.exit_code, "0");
        assert_eq!(
            fs::read_to_string(workspace.join("inside.txt")).unwrap(),
            "visible"
        );
    }
}
