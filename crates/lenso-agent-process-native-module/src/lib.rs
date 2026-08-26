//! Native, workspace-rooted process execution provider.

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
use lenso_capability_agent_process::{
    self as process_contract, CatalogRequest, CatalogResponse, CatalogResponseProgramsItem,
    ProcessProvider, ProcessRun, ProcessRunStreamInvocationError, RunError, RunRequest,
    RunResponse, RunStreamError, RunStreamRequest, RunStreamResponse, RunStreamResponseKind,
};
use lenso_kernel::{InvocationContext, NativeStreamSession, RuntimeFailure};
use tokio::{io::AsyncReadExt, process::Child};

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
    tasks: ManagedTasks,
}

#[derive(Clone, Debug)]
struct ResolvedProgram {
    invocation_path: PathBuf,
    canonical_target: PathBuf,
}

#[lenso::module(
    lifecycle,
    configuration_schema = "config.schema.json",
    validate = validate_config
)]
#[derive(Clone, Debug)]
struct NativeProcessModule {
    #[config]
    config: ProcessConfig,
    provider: Rc<RefCell<Option<NativeProcessProvider>>>,
    #[tasks]
    tasks: ManagedTasks,
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
                    ProcessRunStreamInvocationError::Runtime(RuntimeFailure::ModuleFailure {
                        detail: format!("process stream task failed to start: {error:?}"),
                    })
                })?;
            Ok(Box::new(stream) as Box<dyn NativeStreamSession>)
        })
    }
}

#[lenso::provides(process_contract::Process)]
impl ProcessProvider for NativeProcessModule {
    fn catalog(
        &self,
        context: InvocationContext,
        request: CatalogRequest,
    ) -> lenso_kernel::NativeRequestFuture<process_contract::ProcessCatalog> {
        let provider = self.provider.borrow().clone();
        match provider {
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
        let provider = self.provider.borrow().clone();
        match provider {
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
        let provider = self.provider.borrow().clone();
        match provider {
            Some(provider) => provider.run_stream(context, request),
            None => Box::pin(futures::future::ready(Err(
                ProcessRunStreamInvocationError::Runtime(RuntimeFailure::Unavailable {
                    capability: process_contract::CAPABILITY_ID,
                }),
            ))),
        }
    }
}

impl Lifecycle for NativeProcessModule {
    #[allow(clippy::unused_async_trait_impl)]
    async fn prepare(&self, _context: PrepareContext) -> Result<(), RuntimeFailure> {
        let root = fs::canonicalize(&self.config.root)
            .map_err(|error| invalid_plan(format!("process root is unavailable: {error}")))?;
        if !root.is_dir() {
            return Err(invalid_plan("process root is not a directory"));
        }
        let programs = resolve_programs(&self.config.allowed_programs)?;
        let environment = self
            .config
            .environment_allowlist
            .iter()
            .filter_map(|name| env::var(name).ok().map(|value| (name.clone(), value)))
            .collect();
        self.provider.replace(Some(NativeProcessProvider {
            config: self.config.clone(),
            root,
            programs,
            environment,
            tasks: self.tasks.clone(),
        }));
        Ok(())
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

    async fn run_process_stream(
        &self,
        context: InvocationContext,
        request: RunStreamRequest,
        channel: &mut ProviderStreamChannel<process_contract::ProcessRunStream>,
    ) -> ModuleResult<(), RunStreamError> {
        let request = RunRequest {
            program: request.program,
            arguments: request.arguments,
            cwd: request.cwd,
            timeout_ms: request.timeout_ms,
        };
        let Some(program) = self.programs.get(&request.program) else {
            return Err(ModuleError::domain(RunStreamError::ProgramNotAllowed));
        };
        let current_target = fs::canonicalize(&program.invocation_path).map_err(|error| {
            ModuleError::runtime(RuntimeFailure::ModuleFailure {
                detail: format!("configured process executable is unavailable: {error}"),
            })
        })?;
        if current_target != program.canonical_target {
            return Err(ModuleError::runtime(RuntimeFailure::ModuleFailure {
                detail: "configured process executable identity changed after startup".to_owned(),
            }));
        }
        let timeout_ms = request.timeout_ms.parse::<u64>().map_err(|_| {
            ModuleError::runtime(RuntimeFailure::ProtocolViolation {
                capability: process_contract::CAPABILITY_ID,
            })
        })?;
        let argument_bytes = request
            .arguments
            .iter()
            .try_fold(0usize, |total, argument| {
                total.checked_add(argument.len()).ok_or(())
            });
        let valid = timeout_ms > 0
            && timeout_ms <= self.config.max_timeout_ms
            && request.program.len() <= 128
            && request.cwd.len() <= 4096
            && request.arguments.len() <= 128
            && request
                .arguments
                .iter()
                .all(|argument| argument.len() <= 16_384)
            && argument_bytes.is_ok_and(|bytes| bytes <= self.config.max_argument_bytes);
        if !valid {
            return Err(ModuleError::domain(RunStreamError::InvalidRequest));
        }
        let root = fs::canonicalize(&self.root).map_err(|error| {
            ModuleError::runtime(RuntimeFailure::ModuleFailure {
                detail: format!("process root is unavailable: {error}"),
            })
        })?;
        if root != self.root {
            return Err(ModuleError::runtime(RuntimeFailure::ModuleFailure {
                detail: "process root identity changed after startup".to_owned(),
            }));
        }
        let Some(cwd) = resolve_cwd(&self.root, &request.cwd) else {
            return Err(ModuleError::domain(RunStreamError::InvalidWorkingDirectory));
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
        let child = command.spawn().map_err(|error| {
            ModuleError::runtime(RuntimeFailure::ModuleFailure {
                detail: format!("failed to spawn configured process: {error}"),
            })
        })?;
        observe_child_stream(
            child,
            context,
            timeout_ms,
            self.config.max_output_bytes,
            channel,
        )
        .await
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
) -> ModuleResult<(), RunStreamError> {
    let process_id = child.id();
    let mut guard = ProcessGroupGuard::new(process_id);
    let mut stdout = child.stdout.take().ok_or_else(|| {
        ModuleError::runtime(RuntimeFailure::Internal {
            detail: "spawned process has no stdout pipe".to_owned(),
        })
    })?;
    let mut stderr = child.stderr.take().ok_or_else(|| {
        ModuleError::runtime(RuntimeFailure::Internal {
            detail: "spawned process has no stderr pipe".to_owned(),
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
                return Err(ModuleError::runtime(RuntimeFailure::Cancelled { request_id }));
            }
            () = &mut timeout => {
                terminate(&mut child, process_id).await;
                guard.disarm();
                return Err(ModuleError::domain(RunStreamError::Timeout));
            }
            result = stdout.read(&mut stdout_buffer[..]), if stdout_open => {
                let read = result.map_err(|error| ModuleError::runtime(RuntimeFailure::ModuleFailure {
                    detail: format!("failed to read process stdout: {error}"),
                }))?;
                if read == 0 {
                    stdout_open = false;
                } else {
                    total_bytes = total_bytes.saturating_add(read);
                    if total_bytes > max_output_bytes {
                        terminate(&mut child, process_id).await;
                        guard.disarm();
                        return Err(ModuleError::domain(RunStreamError::OutputLimitExceeded));
                    }
                    channel.send(RunStreamResponse {
                        kind: RunStreamResponseKind::Stdout,
                        content: String::from_utf8_lossy(&stdout_buffer[..read]).into_owned(),
                        exit_code: None,
                        duration_ms: None,
                    }).await.map_err(ModuleError::runtime)?;
                }
            }
            result = stderr.read(&mut stderr_buffer[..]), if stderr_open => {
                let read = result.map_err(|error| ModuleError::runtime(RuntimeFailure::ModuleFailure {
                    detail: format!("failed to read process stderr: {error}"),
                }))?;
                if read == 0 {
                    stderr_open = false;
                } else {
                    total_bytes = total_bytes.saturating_add(read);
                    if total_bytes > max_output_bytes {
                        terminate(&mut child, process_id).await;
                        guard.disarm();
                        return Err(ModuleError::domain(RunStreamError::OutputLimitExceeded));
                    }
                    channel.send(RunStreamResponse {
                        kind: RunStreamResponseKind::Stderr,
                        content: String::from_utf8_lossy(&stderr_buffer[..read]).into_owned(),
                        exit_code: None,
                        duration_ms: None,
                    }).await.map_err(ModuleError::runtime)?;
                }
            }
            result = child.wait(), if status.is_none() => {
                status = Some(result.map_err(|error| ModuleError::runtime(RuntimeFailure::ModuleFailure {
                    detail: format!("failed to wait for process: {error}"),
                }))?);
            }
        }
    }
    guard.disarm();
    let status = status.expect("process loop ends only after observing status");
    let Some(exit_code) = status.code() else {
        return Err(ModuleError::domain(RunStreamError::Terminated));
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
        .map_err(ModuleError::runtime)
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
    use lenso_kernel::{CancellationToken, NativeStreamItem};
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
            tasks: ManagedTasks::default(),
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
    async fn streams_stdout_before_process_completion() {
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .args([
                "-c",
                "printf first; sleep 0.05; printf second; printf err >&2",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let child = command.spawn().unwrap();
        let invocation = context(CancellationToken::new());
        let (stream, mut channel) =
            ProviderStream::<process_contract::ProcessRunStream>::channel(&invocation, 8);
        let producer = async {
            let result = observe_child_stream(child, invocation, 1_000, 4_096, &mut channel).await;
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
        assert_eq!(messages.last().unwrap().exit_code.as_deref(), Some("0"));
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
