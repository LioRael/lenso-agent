//! Native, workspace-rooted process execution provider.

use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Component, Path, PathBuf},
    process::{ExitStatus, Stdio},
    rc::Rc,
    time::{Duration, Instant},
};

use futures::future::ready;
use lenso_capability_agent_process::{
    CatalogRequest, CatalogResponse, CatalogResponseProgramsItem, ProcessEndpoint, ProcessProvider,
    ProcessRun, RunError, RunRequest, RunResponse,
};
use lenso_kernel::{InvocationContext, NativeRequestEndpoint, RuntimeFailure};
use lenso_native_adapter::{NativeModuleFactory, NativeModuleFactoryContext, NativeModuleInstance};
use tokio::{io::AsyncReadExt, process::Child};

/// Runtime package identity selected by App Composition.
pub const PACKAGE_ID: &str = "lenso.agent.process.native";
/// Exact linked package version.
pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProcessConfig {
    root: PathBuf,
    allowed_programs: Vec<String>,
    environment_allowlist: Vec<String>,
    max_timeout_ms: u64,
    max_output_bytes: usize,
    max_argument_bytes: usize,
}

#[derive(Clone, Debug)]
struct NativeProcessProvider {
    config: ProcessConfig,
    root: PathBuf,
    programs: BTreeMap<String, ResolvedProgram>,
    environment: BTreeMap<String, String>,
}

#[derive(Clone, Debug)]
struct ResolvedProgram {
    invocation_path: PathBuf,
    canonical_target: PathBuf,
}

/// Native factory for one explicitly configured process authority.
#[derive(Clone, Debug, Default)]
pub struct NativeProcessFactory;

impl NativeModuleFactory for NativeProcessFactory {
    fn package_id(&self) -> &'static str {
        PACKAGE_ID
    }

    fn package_version(&self) -> &'static str {
        PACKAGE_VERSION
    }

    fn instantiate(
        &self,
        context: NativeModuleFactoryContext<'_>,
    ) -> Result<NativeModuleInstance, RuntimeFailure> {
        if context.entrypoint() != "default" {
            return Err(invalid_plan("unsupported native-process entrypoint"));
        }
        let config =
            serde_json::from_str::<ProcessConfig>(context.configuration()).map_err(|error| {
                invalid_plan(format!("invalid native-process configuration: {error}"))
            })?;
        validate_config(&config)?;
        let root = fs::canonicalize(&config.root)
            .map_err(|error| invalid_plan(format!("process root is unavailable: {error}")))?;
        if !root.is_dir() {
            return Err(invalid_plan("process root is not a directory"));
        }
        let programs = resolve_programs(&config.allowed_programs)?;
        let environment = config
            .environment_allowlist
            .iter()
            .filter_map(|name| env::var(name).ok().map(|value| (name.clone(), value)))
            .collect();
        let endpoint = Rc::new(ProcessEndpoint::new(NativeProcessProvider {
            config,
            root,
            programs,
            environment,
        })) as Rc<dyn NativeRequestEndpoint>;
        Ok(NativeModuleInstance::new(vec![endpoint]))
    }
}

fn validate_config(config: &ProcessConfig) -> Result<(), RuntimeFailure> {
    if config.allowed_programs.is_empty() || config.allowed_programs.len() > 32 {
        return Err(invalid_plan(
            "allowed_programs must contain between 1 and 32 names",
        ));
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

fn resolve_programs(names: &[String]) -> Result<BTreeMap<String, ResolvedProgram>, RuntimeFailure> {
    let search_path = env::var_os("PATH")
        .ok_or_else(|| invalid_plan("PATH is unavailable while resolving allowed programs"))?;
    names
        .iter()
        .map(|name| {
            let invocation_path = env::split_paths(&search_path)
                .filter_map(|directory| fs::canonicalize(directory).ok())
                .map(|directory| directory.join(name))
                .find(|candidate| candidate.is_file())
                .ok_or_else(|| invalid_plan(format!("allowed program `{name}` was not found")))?;
            let canonical_target = fs::canonicalize(&invocation_path).map_err(|error| {
                invalid_plan(format!("allowed program `{name}` is unavailable: {error}"))
            })?;
            Ok((
                name.clone(),
                ResolvedProgram {
                    invocation_path,
                    canonical_target,
                },
            ))
        })
        .collect()
}

impl ProcessProvider for NativeProcessProvider {
    fn catalog(
        &self,
        _context: InvocationContext,
        _request: CatalogRequest,
    ) -> lenso_kernel::NativeRequestFuture<lenso_capability_agent_process::ProcessCatalog> {
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
        Box::pin(async move { Box::pin(provider.run_process(context, request)).await })
    }
}

impl NativeProcessProvider {
    async fn run_process(
        &self,
        context: InvocationContext,
        request: RunRequest,
    ) -> Result<Result<RunResponse, RunError>, RuntimeFailure> {
        let Some(program) = self.programs.get(&request.program) else {
            return Ok(Err(RunError::ProgramNotAllowed));
        };
        let current_target = fs::canonicalize(&program.invocation_path).map_err(|error| {
            RuntimeFailure::ModuleFailure {
                detail: format!("configured process executable is unavailable: {error}"),
            }
        })?;
        if current_target != program.canonical_target {
            return Err(RuntimeFailure::ModuleFailure {
                detail: "configured process executable identity changed after startup".to_owned(),
            });
        }
        let timeout_ms =
            request
                .timeout_ms
                .parse::<u64>()
                .map_err(|_| RuntimeFailure::ProtocolViolation {
                    capability: lenso_capability_agent_process::CAPABILITY_ID,
                })?;
        let request_shape_valid = request.program.len() <= 128
            && request.cwd.len() <= 4096
            && request.arguments.len() <= 128
            && request
                .arguments
                .iter()
                .all(|argument| argument.len() <= 16_384);
        let argument_bytes = request
            .arguments
            .iter()
            .try_fold(0usize, |total, argument| {
                total
                    .checked_add(argument.len())
                    .ok_or(RunError::InvalidRequest)
            });
        let Ok(argument_bytes) = argument_bytes else {
            return Ok(Err(RunError::InvalidRequest));
        };
        if timeout_ms == 0
            || timeout_ms > self.config.max_timeout_ms
            || !request_shape_valid
            || argument_bytes > self.config.max_argument_bytes
        {
            return Ok(Err(RunError::InvalidRequest));
        }
        let root = fs::canonicalize(&self.root).map_err(|error| RuntimeFailure::ModuleFailure {
            detail: format!("process root is unavailable: {error}"),
        })?;
        if root != self.root {
            return Err(RuntimeFailure::ModuleFailure {
                detail: "process root identity changed after startup".to_owned(),
            });
        }
        let Some(cwd) = resolve_cwd(&self.root, &request.cwd) else {
            return Ok(Err(RunError::InvalidWorkingDirectory));
        };

        let mut command = tokio::process::Command::new(&program.invocation_path);
        command
            .args(&request.arguments)
            .current_dir(cwd)
            .env_clear()
            .envs(&self.environment)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.as_std_mut().process_group(0);
        }
        let child = command
            .spawn()
            .map_err(|error| RuntimeFailure::ModuleFailure {
                detail: format!("failed to spawn configured process: {error}"),
            })?;
        observe_child(child, context, timeout_ms, self.config.max_output_bytes).await
    }
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
            detail: "spawned process has no stdout pipe".to_owned(),
        })?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| RuntimeFailure::Internal {
            detail: "spawned process has no stderr pipe".to_owned(),
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
                let read = result.map_err(|error| RuntimeFailure::ModuleFailure {
                    detail: format!("failed to read process stdout: {error}"),
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
                let read = result.map_err(|error| RuntimeFailure::ModuleFailure {
                    detail: format!("failed to read process stderr: {error}"),
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
                status = Some(result.map_err(|error| RuntimeFailure::ModuleFailure {
                    detail: format!("failed to wait for process: {error}"),
                })?);
            }
        }
    }
    guard.disarm();
    let status = status.expect("process loop ends only after observing status");
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use lenso_kernel::CancellationToken;
    use nix::{errno::Errno, sys::signal::kill, unistd::Pid};

    fn provider(
        root: PathBuf,
        maximum_output: usize,
        maximum_timeout: u64,
    ) -> NativeProcessProvider {
        let root = fs::canonicalize(root).unwrap();
        NativeProcessProvider {
            config: ProcessConfig {
                root: root.clone(),
                allowed_programs: vec!["test".to_owned()],
                environment_allowlist: Vec::new(),
                max_timeout_ms: maximum_timeout,
                max_output_bytes: maximum_output,
                max_argument_bytes: 4096,
            },
            root,
            programs: BTreeMap::from([(
                "test".to_owned(),
                ResolvedProgram {
                    invocation_path: PathBuf::from("/bin/sh"),
                    canonical_target: fs::canonicalize("/bin/sh").unwrap(),
                },
            )]),
            environment: BTreeMap::new(),
        }
    }

    fn request(script: &str, timeout_ms: u64) -> RunRequest {
        RunRequest {
            program: "test".to_owned(),
            arguments: vec!["-c".to_owned(), script.to_owned()],
            cwd: ".".to_owned(),
            timeout_ms: timeout_ms.to_string(),
        }
    }

    fn context(cancellation: CancellationToken) -> InvocationContext {
        InvocationContext::new(7, None, cancellation)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn captures_nonzero_exit_and_both_output_streams() {
        let temporary = tempfile::tempdir().unwrap();
        let provider = provider(temporary.path().to_path_buf(), 4096, 1000);
        let response = provider
            .run_process(
                context(CancellationToken::new()),
                request("printf out; printf err >&2; exit 7", 1000),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response.exit_code, "7");
        assert_eq!(response.stdout, "out");
        assert_eq!(response.stderr, "err");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejects_unbound_programs_and_escaped_working_directories() {
        let temporary = tempfile::tempdir().unwrap();
        let provider = provider(temporary.path().to_path_buf(), 4096, 1000);
        let mut unbound = request("exit 0", 1000);
        unbound.program = "other".to_owned();
        assert_eq!(
            provider
                .run_process(context(CancellationToken::new()), unbound)
                .await,
            Ok(Err(RunError::ProgramNotAllowed))
        );
        let mut escaped = request("exit 0", 1000);
        escaped.cwd = "../".to_owned();
        assert_eq!(
            provider
                .run_process(context(CancellationToken::new()), escaped)
                .await,
            Ok(Err(RunError::InvalidWorkingDirectory))
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn kills_the_process_group_on_timeout_and_output_limit() {
        let temporary = tempfile::tempdir().unwrap();
        let provider = provider(temporary.path().to_path_buf(), 64, 1000);
        assert_eq!(
            provider
                .run_process(context(CancellationToken::new()), request("sleep 30", 20),)
                .await,
            Ok(Err(RunError::Timeout))
        );
        assert_eq!(
            provider
                .run_process(
                    context(CancellationToken::new()),
                    request("while :; do printf xxxxxxxxxxxxxxxx; done", 1000),
                )
                .await,
            Ok(Err(RunError::OutputLimitExceeded))
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancellation_kills_descendants_and_returns_kernel_cancellation() {
        let temporary = tempfile::tempdir().unwrap();
        let provider = provider(temporary.path().to_path_buf(), 4096, 1000);
        let cancellation = CancellationToken::new();
        let running = provider.run_process(
            context(cancellation.clone()),
            request("sleep 30 & echo $! > child.pid; wait", 1000),
        );
        let cancel = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancellation.cancel();
        };
        let (result, ()) = tokio::join!(running, cancel);
        assert!(matches!(
            result,
            Err(RuntimeFailure::Cancelled { request_id: 7 })
        ));

        let child_pid = fs::read_to_string(temporary.path().join("child.pid"))
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
    async fn root_identity_loss_is_a_runtime_failure() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("workspace");
        fs::create_dir(&root).unwrap();
        let provider = provider(root.clone(), 4096, 1000);
        fs::remove_dir(root).unwrap();
        assert!(matches!(
            provider
                .run_process(context(CancellationToken::new()), request("exit 0", 1000),)
                .await,
            Err(RuntimeFailure::ModuleFailure { .. })
        ));
    }
}
