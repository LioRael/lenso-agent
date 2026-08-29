use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use lenso_kernel::NativeApp;
use lenso_plugin_control_plane::{
    ControlHealth, ControlLifecycle, DurableControlState, DurableTransitionOutcome,
    GenerationControllerClient, GenerationControllerEvent, GenerationMaintenanceOutcome,
    ResolvedGeneration,
};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::{mpsc, oneshot};

use super::{
    DesiredGeneration, HostBuildIdentity, control_error, desired_generation_identity,
    directories_for_store_root, online_overlap_transition, record_generation_spec,
    resolve_generation_from_plan, resolve_host_plan_for_agent_in, resolve_host_plan_in,
};
use crate::authority::AuthorityFence;
use crate::online_generation::{
    OnlineGenerationEvent, OnlineGenerationEventLog, OnlineGenerationTracker,
};

const RECONCILE_QUIET_PERIOD: Duration = Duration::from_millis(200);
const RECONCILE_SETTLE_LIMIT: Duration = Duration::from_secs(2);
const RECONCILE_CONSISTENCY_INTERVAL: Duration = Duration::from_secs(2);
const RECONCILE_FULL_AUDIT_INTERVALS: u32 = 30;

static FULL_RECONCILE_ATTEMPTS: AtomicU64 = AtomicU64::new(0);

/// Process-local counters for deterministic reconciliation performance smoke tests.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OnlineReconcileTelemetry {
    pub canonical_snapshots: u64,
    pub full_reconcile_attempts: u64,
    pub metadata_probes: u64,
    pub resource_directory_reads: u64,
}

impl OnlineReconcileTelemetry {
    #[must_use]
    pub const fn delta(self, earlier: Self) -> Self {
        Self {
            canonical_snapshots: self
                .canonical_snapshots
                .saturating_sub(earlier.canonical_snapshots),
            full_reconcile_attempts: self
                .full_reconcile_attempts
                .saturating_sub(earlier.full_reconcile_attempts),
            metadata_probes: self.metadata_probes.saturating_sub(earlier.metadata_probes),
            resource_directory_reads: self
                .resource_directory_reads
                .saturating_sub(earlier.resource_directory_reads),
        }
    }
}

pub(super) fn telemetry() -> OnlineReconcileTelemetry {
    let (canonical_snapshots, metadata_probes, resource_directory_reads) =
        crate::plugin_root::io_telemetry();
    OnlineReconcileTelemetry {
        canonical_snapshots,
        full_reconcile_attempts: FULL_RECONCILE_ATTEMPTS.load(Ordering::Relaxed),
        metadata_probes,
        resource_directory_reads,
    }
}

#[derive(Debug)]
pub(super) struct GenerationReconciler {
    reopen: mpsc::Sender<()>,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl GenerationReconciler {
    pub(super) fn reopen(&self) -> Result<(), String> {
        match self.reopen.try_send(()) {
            Ok(()) | Err(mpsc::error::TrySendError::Full(())) => Ok(()),
            Err(mpsc::error::TrySendError::Closed(())) => {
                Err("Generation Reconciler is not available".to_owned())
            }
        }
    }

    pub(super) async fn shutdown(mut self) -> Result<(), String> {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        self.task
            .await
            .map_err(|error| format!("Generation Reconciler task failed: {error}"))
    }
}

#[derive(Debug)]
enum FilesystemReconcileSignal {
    Changed,
    Error(String),
}

struct FilesystemReconcileWatcher {
    watcher: Option<RecommendedWatcher>,
    signals: tokio::sync::mpsc::UnboundedReceiver<FilesystemReconcileSignal>,
    _sender: tokio::sync::mpsc::UnboundedSender<FilesystemReconcileSignal>,
    recursive_path: Option<PathBuf>,
    recursive_identity: Option<RecursiveWatchIdentity>,
    #[cfg(test)]
    recursive_watch_attachments: u64,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RecursiveWatchIdentity {
    device: u64,
    inode: u64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

#[cfg(not(unix))]
#[derive(Clone, Debug, Eq, PartialEq)]
struct RecursiveWatchIdentity {
    canonical_path: PathBuf,
    created: Option<std::time::SystemTime>,
}

#[derive(Debug)]
struct ReconcileConsistencyState {
    last_probe: Option<crate::plugin_root::DesiredStateProbe>,
    intervals_since_full_audit: u32,
}

impl ReconcileConsistencyState {
    fn new(last_probe: Option<crate::plugin_root::DesiredStateProbe>) -> Self {
        Self {
            last_probe,
            intervals_since_full_audit: 0,
        }
    }

    /// Metadata only suppresses redundant work; it never admits authority.
    /// Probe changes/errors and bounded deep-audit ticks all route through the
    /// complete canonical snapshot before any Generation transition.
    fn should_run_full_reconcile(
        &mut self,
        probe: Result<crate::plugin_root::DesiredStateProbe, String>,
    ) -> bool {
        self.intervals_since_full_audit = self.intervals_since_full_audit.saturating_add(1);
        match probe {
            Ok(probe) if self.last_probe.as_ref() == Some(&probe) => {
                if self.intervals_since_full_audit < RECONCILE_FULL_AUDIT_INTERVALS {
                    return false;
                }
            }
            Ok(probe) => self.last_probe = Some(probe),
            Err(_) => self.last_probe = None,
        }
        self.intervals_since_full_audit = 0;
        true
    }

    fn refreshed(&mut self, probe: Result<crate::plugin_root::DesiredStateProbe, String>) {
        self.last_probe = probe.ok();
        self.intervals_since_full_audit = 0;
    }

    fn retry_required(&mut self) {
        self.last_probe = None;
        self.intervals_since_full_audit = 0;
    }
}

impl FilesystemReconcileWatcher {
    fn start(non_recursive: &[&Path], recursive_path: Option<PathBuf>) -> (Self, Vec<String>) {
        let (sender, signals) = tokio::sync::mpsc::unbounded_channel();
        let callback_sender = sender.clone();
        let mut errors = Vec::new();
        let watcher =
            match notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                match event {
                    Ok(_) => {
                        let _ = callback_sender.send(FilesystemReconcileSignal::Changed);
                    }
                    Err(error) => {
                        let _ = callback_sender
                            .send(FilesystemReconcileSignal::Error(error.to_string()));
                    }
                }
            }) {
                Ok(mut watcher) => {
                    let mut watched_paths = BTreeSet::new();
                    for path in non_recursive
                        .iter()
                        .copied()
                        .filter(|path| watched_paths.insert((*path).to_path_buf()))
                    {
                        if let Err(error) = watcher.watch(path, RecursiveMode::NonRecursive) {
                            errors.push(format!(
                                "failed to watch Desired State path {}: {error}",
                                path.display()
                            ));
                        }
                    }
                    Some(watcher)
                }
                Err(error) => {
                    errors.push(format!("failed to start filesystem watcher: {error}"));
                    None
                }
            };
        let mut watcher = Self {
            watcher,
            signals,
            _sender: sender,
            recursive_path,
            recursive_identity: None,
            #[cfg(test)]
            recursive_watch_attachments: 0,
        };
        if let Some(error) = watcher.refresh_recursive_watch() {
            errors.push(error);
        }
        (watcher, errors)
    }

    fn refresh_recursive_watch(&mut self) -> Option<String> {
        let path = self.recursive_path.as_ref()?;
        let identity = match recursive_watch_identity(path) {
            Ok(identity) => identity,
            Err(error) => return Some(error),
        };
        if self.recursive_identity == identity {
            return None;
        }
        let watcher = self.watcher.as_mut()?;
        if self.recursive_identity.take().is_some() {
            let _ = watcher.unwatch(path);
        }
        if let Some(identity) = identity {
            match watcher.watch(path, RecursiveMode::Recursive) {
                Ok(()) => {
                    self.recursive_identity = Some(identity);
                    #[cfg(test)]
                    {
                        self.recursive_watch_attachments =
                            self.recursive_watch_attachments.saturating_add(1);
                    }
                }
                Err(error) => {
                    return Some(format!(
                        "failed to watch Plugin discovery path {}: {error}",
                        path.display()
                    ));
                }
            }
        }
        None
    }

    async fn changed(&mut self) -> Option<FilesystemReconcileSignal> {
        self.signals.recv().await
    }

    async fn settle_after(&mut self, initial: Option<FilesystemReconcileSignal>) -> Vec<String> {
        let mut errors = Vec::new();
        if let Some(FilesystemReconcileSignal::Error(error)) = initial {
            errors.push(error);
        }
        let deadline = tokio::time::Instant::now() + RECONCILE_SETTLE_LIMIT;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            let quiet_period = RECONCILE_QUIET_PERIOD.min(remaining);
            let Ok(Some(signal)) = tokio::time::timeout(quiet_period, self.signals.recv()).await
            else {
                break;
            };
            if let FilesystemReconcileSignal::Error(error) = signal {
                errors.push(error);
            }
        }
        errors
    }
}

fn recursive_watch_identity(path: &Path) -> Result<Option<RecursiveWatchIdentity>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "failed to inspect Plugin discovery path {}: {error}",
                path.display()
            ));
        }
    };
    if !metadata.file_type().is_dir() {
        return Ok(None);
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        Ok(Some(RecursiveWatchIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }))
    }
    #[cfg(not(unix))]
    {
        let canonical_path = fs::canonicalize(path).map_err(|error| {
            format!(
                "failed to identify Plugin discovery path {}: {error}",
                path.display()
            )
        })?;
        Ok(Some(RecursiveWatchIdentity {
            canonical_path,
            created: metadata.created().ok(),
        }))
    }
}

pub(super) fn start(
    client: GenerationControllerClient<NativeApp>,
    store_root: PathBuf,
    host_build: HostBuildIdentity,
    profile_name: Option<String>,
    authoring_managed: bool,
    events: Rc<RefCell<OnlineGenerationEventLog>>,
    online_generation: Rc<RefCell<OnlineGenerationTracker>>,
) -> GenerationReconciler {
    let (stop, stopped) = oneshot::channel();
    let (reopen, reopen_requests) = mpsc::channel(1);
    let directories = directories_for_store_root(&store_root)
        .expect("validated Agent runtime root must have an Agent Home parent");
    let plugin_root = directories.plugins();
    let plugin_parent = watch_parent(&plugin_root);
    let profile_directory = directories.profiles();
    let selected_profile_path = profile_name
        .as_deref()
        .map(|profile| profile_directory.join(format!("{profile}.toml")));
    let mut watched_paths = vec![store_root.as_path()];
    if authoring_managed {
        watched_paths.push(plugin_parent.as_path());
        if profile_name.is_some() {
            watched_paths.push(profile_directory.as_path());
        }
    }
    let (watcher, watcher_errors) =
        FilesystemReconcileWatcher::start(&watched_paths, Some(plugin_root.clone()));
    report_watcher_errors(&events, watcher_errors);
    let consistency = ReconcileConsistencyState::new(
        crate::plugin_root::desired_state_probe(&plugin_root, selected_profile_path.as_deref())
            .ok(),
    );
    let task = tokio::task::spawn_local(
        OnlineReconcilerLoop {
            controller_events: client.subscribe(),
            client,
            store_root,
            plugin_root,
            host_build,
            profile_name,
            authoring_managed,
            events,
            online_generation,
            watcher,
            consistency,
            reopen_requests,
            last_attempted_desired: None,
            last_deduplicated_outcome: None,
        }
        .run(stopped),
    );
    GenerationReconciler {
        reopen,
        stop: Some(stop),
        task,
    }
}

struct OnlineReconcilerLoop {
    client: GenerationControllerClient<NativeApp>,
    store_root: PathBuf,
    plugin_root: PathBuf,
    host_build: HostBuildIdentity,
    profile_name: Option<String>,
    authoring_managed: bool,
    events: Rc<RefCell<OnlineGenerationEventLog>>,
    online_generation: Rc<RefCell<OnlineGenerationTracker>>,
    controller_events: tokio::sync::broadcast::Receiver<GenerationControllerEvent>,
    watcher: FilesystemReconcileWatcher,
    consistency: ReconcileConsistencyState,
    reopen_requests: mpsc::Receiver<()>,
    last_attempted_desired: Option<AttemptedDesiredState>,
    last_deduplicated_outcome: Option<OnlineGenerationEvent>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AttemptedDesiredState {
    plugin_root_revision: String,
    desired_state_digest: String,
}

impl AttemptedDesiredState {
    fn new(plugin_root_revision: &str, desired_state_digest: &str) -> Self {
        Self {
            plugin_root_revision: plugin_root_revision.to_owned(),
            desired_state_digest: desired_state_digest.to_owned(),
        }
    }
}

fn reopen_explicit_attempt(last_attempted_desired: &mut Option<AttemptedDesiredState>) {
    *last_attempted_desired = None;
}

impl OnlineReconcilerLoop {
    async fn run(mut self, mut stopped: oneshot::Receiver<()>) {
        let mut interval = tokio::time::interval(RECONCILE_CONSISTENCY_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                _ = &mut stopped => break,
                event = self.controller_events.recv() => {
                    if !self.handle_controller_event(event).await {
                        break;
                    }
                }
                _ = interval.tick() => {
                    report_watcher_errors(
                        &self.events,
                        self.watcher.refresh_recursive_watch(),
                    );
                    let probe = self.desired_state_probe();
                    if self.authoring_managed
                        && self.consistency.should_run_full_reconcile(probe)
                        && !self.reconcile_and_record().await
                    {
                        self.consistency.retry_required();
                    }
                }
                request = self.reopen_requests.recv() => {
                    if request.is_none() {
                        break;
                    }
                    reopen_explicit_attempt(&mut self.last_attempted_desired);
                    self.consistency.retry_required();
                    if self.authoring_managed && !self.reconcile_and_record().await {
                        self.consistency.retry_required();
                    }
                }
                signal = self.watcher.changed() => {
                    self.handle_filesystem_signal(signal).await;
                }
            }
        }
    }

    fn selected_profile_path(&self) -> Option<PathBuf> {
        self.profile_name.as_deref().map(|profile| {
            directories_for_store_root(&self.store_root)
                .expect("validated Agent runtime root must have an Agent Home parent")
                .profiles()
                .join(format!("{profile}.toml"))
        })
    }

    fn desired_state_probe(&self) -> Result<crate::plugin_root::DesiredStateProbe, String> {
        let profile_path = self.selected_profile_path();
        crate::plugin_root::desired_state_probe(&self.plugin_root, profile_path.as_deref())
    }

    async fn handle_controller_event(
        &self,
        event: Result<GenerationControllerEvent, tokio::sync::broadcast::error::RecvError>,
    ) -> bool {
        match event {
            Ok(event) => {
                if let Some(event) = online_event_from_controller_event(event) {
                    let cursor = push_reconcile_event(&self.events, event.clone());
                    apply_online_generation_event(&self.online_generation, &event, cursor);
                }
                true
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                let (event, resynchronized) = match self
                    .client
                    .inspect()
                    .await
                    .map_err(control_error)
                {
                    Ok(state) => {
                        lagged_controller_resync_event(&self.online_generation, &state, skipped)
                    }
                    Err(detail) => (
                        OnlineGenerationEvent::WatchDegraded {
                            detail: format!(
                                "Generation Controller event stream lagged by {skipped} events and \
                             Controller inspection failed: {detail}"
                            ),
                        },
                        false,
                    ),
                };
                let cursor = push_reconcile_event(&self.events, event);
                if resynchronized {
                    self.online_generation
                        .borrow_mut()
                        .observe_resynchronized_rejection(cursor);
                }
                true
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => false,
        }
    }

    async fn handle_filesystem_signal(&mut self, signal: Option<FilesystemReconcileSignal>) {
        let mut errors = self.watcher.settle_after(signal).await;
        if let Some(error) = self.watcher.refresh_recursive_watch() {
            errors.push(error);
        }
        report_watcher_errors(&self.events, errors);
        if self.authoring_managed && !self.reconcile_and_record().await {
            self.consistency.retry_required();
        } else {
            let probe = self.desired_state_probe();
            self.consistency.refreshed(probe);
        }
    }

    async fn reconcile_and_record(&mut self) -> bool {
        match reconcile_online_generation(self).await {
            OnlineReconcileAttempt::AuthorityBusy => false,
            OnlineReconcileAttempt::Retryable(event) => {
                record_reconcile_outcome(
                    &self.events,
                    &self.online_generation,
                    &mut self.last_deduplicated_outcome,
                    event,
                );
                false
            }
            OnlineReconcileAttempt::Completed(event) => {
                record_completed_reconcile_outcome(
                    &self.events,
                    &self.online_generation,
                    &mut self.last_deduplicated_outcome,
                    event,
                );
                true
            }
        }
    }
}

fn lagged_controller_resync_event(
    tracker: &Rc<RefCell<OnlineGenerationTracker>>,
    state: &DurableControlState,
    skipped: u64,
) -> (OnlineGenerationEvent, bool) {
    let (detail, resynchronized) = match synchronize_online_generation_from_controller_state(
        tracker, state,
    ) {
        Ok(()) => (
            format!(
                "Generation Controller event stream lagged by {skipped} events; online Generation \
             projection was resynchronized from durable Controller state"
            ),
            true,
        ),
        Err(detail) => (
            format!(
                "Generation Controller event stream lagged by {skipped} events and durable \
             resynchronization failed: {detail}"
            ),
            false,
        ),
    };
    (
        OnlineGenerationEvent::WatchDegraded { detail },
        resynchronized,
    )
}

fn synchronize_online_generation_from_controller_state(
    tracker: &Rc<RefCell<OnlineGenerationTracker>>,
    state: &DurableControlState,
) -> Result<(), String> {
    let active = state
        .active_generation_spec_digest
        .as_deref()
        .ok_or_else(|| {
            "durable Controller state does not contain an active Generation".to_owned()
        })?;
    let failed = state
        .generations
        .iter()
        .filter(|record| record.health == ControlHealth::Failed)
        .map(|record| record.generation_spec_digest.clone())
        .collect::<BTreeSet<_>>();
    if !tracker.borrow_mut().synchronize_active(active, &failed) {
        return Err(format!(
            "durable active Generation `{active}` is not retained by this Host"
        ));
    }
    Ok(())
}

fn record_reconcile_outcome(
    events: &Rc<RefCell<OnlineGenerationEventLog>>,
    tracker: &Rc<RefCell<OnlineGenerationTracker>>,
    last_deduplicated_outcome: &mut Option<OnlineGenerationEvent>,
    event: OnlineGenerationEvent,
) {
    let deduplicated = matches!(
        event,
        OnlineGenerationEvent::Rejected { .. } | OnlineGenerationEvent::WatchDegraded { .. }
    );
    if !deduplicated || last_deduplicated_outcome.as_ref() != Some(&event) {
        let cursor = push_reconcile_event(events, event.clone());
        apply_online_generation_event(tracker, &event, cursor);
    }
    *last_deduplicated_outcome = deduplicated.then_some(event);
}

fn record_completed_reconcile_outcome(
    events: &Rc<RefCell<OnlineGenerationEventLog>>,
    tracker: &Rc<RefCell<OnlineGenerationTracker>>,
    last_deduplicated_outcome: &mut Option<OnlineGenerationEvent>,
    event: Option<OnlineGenerationEvent>,
) {
    match event {
        Some(event) => record_reconcile_outcome(events, tracker, last_deduplicated_outcome, event),
        None => *last_deduplicated_outcome = None,
    }
}

fn watch_parent(path: &Path) -> PathBuf {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

fn online_event_from_controller_event(
    event: GenerationControllerEvent,
) -> Option<OnlineGenerationEvent> {
    let GenerationControllerEvent::Maintained(GenerationMaintenanceOutcome::Failed(failure)) =
        event
    else {
        return None;
    };
    let detail = format!("terminal App Generation failure: {:?}", failure.failure);
    Some(match failure.automatic_rollback {
        Some(rollback) => OnlineGenerationEvent::RolledBack {
            failed_generation_spec_digest: failure.generation_spec_digest,
            restored_generation_spec_digest: rollback.active_generation_spec_digest,
            routing_epoch: rollback.routing_epoch,
            detail,
        },
        None => OnlineGenerationEvent::Failed {
            generation_spec_digest: failure.generation_spec_digest,
            detail,
        },
    })
}

async fn activate_online_candidate(
    client: &GenerationControllerClient<NativeApp>,
    state: &DurableControlState,
    previous_generation_spec_digest: &str,
    candidate: ResolvedGeneration,
) -> Result<Option<DurableTransitionOutcome>, String> {
    let candidate_digest = candidate.spec.digest();
    let retained_candidate = state.generations.iter().find(|record| {
        record.generation_spec_digest == candidate_digest
            && matches!(
                record.lifecycle,
                ControlLifecycle::Draining | ControlLifecycle::Standby
            )
            && record.health == ControlHealth::Healthy
    });
    if let Some(retained_candidate) = retained_candidate {
        let is_direct_predecessor = state.generations.iter().any(|record| {
            record.generation_spec_digest == previous_generation_spec_digest
                && record.transition_spec_digest == retained_candidate.transition_spec_digest
        });
        if !is_direct_predecessor {
            return Ok(None);
        }
        return client
            .rollback(candidate_digest)
            .await
            .map(Some)
            .map_err(control_error);
    }
    let transition = online_overlap_transition(previous_generation_spec_digest, &candidate)
        .map_err(control_error)?;
    client
        .transition(transition, candidate, BTreeMap::new())
        .await
        .map(Some)
        .map_err(control_error)
}

fn durable_active_generation_is_healthy(state: &DurableControlState, digest: &str) -> bool {
    state.active_generation_spec_digest.as_deref() == Some(digest)
        && state.generations.iter().any(|record| {
            record.generation_spec_digest == digest
                && record.lifecycle == ControlLifecycle::Active
                && record.health == ControlHealth::Healthy
        })
}

async fn reconcile_online_generation(context: &mut OnlineReconcilerLoop) -> OnlineReconcileAttempt {
    let prepared = match prepare_online_candidate(context).await {
        Ok(CandidatePreparation::Ready(prepared)) => *prepared,
        Ok(CandidatePreparation::AuthorityBusy) => return OnlineReconcileAttempt::AuthorityBusy,
        Ok(CandidatePreparation::NoChange) => return OnlineReconcileAttempt::Completed(None),
        Ok(CandidatePreparation::Retryable(event)) => {
            return OnlineReconcileAttempt::Retryable(*event);
        }
        Err(event) => return OnlineReconcileAttempt::Completed(Some(*event)),
    };
    let _authoring_fence = prepared.authoring_fence;
    let _authority_fence = prepared.authority_fence;
    let desired = prepared.desired;
    let state = prepared.state;
    let previous_generation_spec_digest = prepared.previous_generation_spec_digest;
    let plugin_root_revision = desired.plugin_root_revision.clone();
    let resolution_authority_digest = desired.resolution_authority_digest.clone();
    let desired_state_digest = desired.desired_state_digest.clone();
    let plan_digest = desired.plan_digest.clone();
    let desired_selection = prepared.desired_selection;
    let candidate = desired.generation;
    if previous_generation_spec_digest == candidate.spec.digest() {
        if !durable_active_generation_is_healthy(&state, candidate.spec.digest()) {
            return OnlineReconcileAttempt::Retryable(retryable_candidate_failure(
                &mut context.last_attempted_desired,
                format!(
                    "active Generation `{}` is not durably healthy",
                    candidate.spec.digest()
                ),
            ));
        }
        let mut online_generation = context.online_generation.borrow_mut();
        project_same_generation_selection(
            &mut online_generation,
            desired_selection,
            candidate.spec.digest(),
        );
        return OnlineReconcileAttempt::Completed(None);
    }
    if let Err(detail) = record_generation_spec(&context.store_root, &candidate.spec) {
        return OnlineReconcileAttempt::Retryable(retryable_candidate_failure(
            &mut context.last_attempted_desired,
            format!("failed to retain candidate Generation: {detail}"),
        ));
    }
    let generation_spec_digest = candidate.spec.digest().to_owned();
    context
        .online_generation
        .borrow_mut()
        .mark_preparing(desired_selection);
    push_reconcile_event(
        &context.events,
        OnlineGenerationEvent::Preparing {
            plugin_root_revision: plugin_root_revision.clone(),
            resolution_authority_digest: resolution_authority_digest.clone(),
            desired_state_digest: desired_state_digest.clone(),
            plan_digest: plan_digest.clone(),
            generation_spec_digest: generation_spec_digest.clone(),
            previous_generation_spec_digest: previous_generation_spec_digest.clone(),
        },
    );
    let event = match activate_online_candidate(
        &context.client,
        &state,
        &previous_generation_spec_digest,
        candidate,
    )
    .await
    {
        Ok(Some(outcome)) => Some(OnlineGenerationEvent::Switched {
            plugin_root_revision,
            resolution_authority_digest,
            desired_state_digest,
            plan_digest,
            generation_spec_digest: outcome.active_generation_spec_digest,
            previous_generation_spec_digest,
            routing_epoch: outcome.routing_epoch,
        }),
        Ok(None) => {
            context.last_attempted_desired = None;
            None
        }
        Err(detail) => {
            return OnlineReconcileAttempt::Retryable(retryable_candidate_failure(
                &mut context.last_attempted_desired,
                format!("Generation Controller could not activate the candidate: {detail}"),
            ));
        }
    };
    OnlineReconcileAttempt::Completed(event)
}

fn project_same_generation_selection(
    tracker: &mut OnlineGenerationTracker,
    selection: crate::online_generation::OnlineGenerationSelection,
    generation_spec_digest: &str,
) {
    tracker.mark_preparing(selection);
    tracker.switched(generation_spec_digest);
}

enum OnlineReconcileAttempt {
    AuthorityBusy,
    Completed(Option<OnlineGenerationEvent>),
    Retryable(OnlineGenerationEvent),
}

struct PreparedOnlineCandidate {
    authoring_fence: fs::File,
    authority_fence: AuthorityFence,
    desired: DesiredGeneration,
    desired_selection: crate::online_generation::OnlineGenerationSelection,
    state: DurableControlState,
    previous_generation_spec_digest: String,
}

enum CandidatePreparation {
    AuthorityBusy,
    NoChange,
    Ready(Box<PreparedOnlineCandidate>),
    Retryable(Box<OnlineGenerationEvent>),
}

async fn prepare_online_candidate(
    context: &mut OnlineReconcilerLoop,
) -> Result<CandidatePreparation, Box<OnlineGenerationEvent>> {
    let authoring_fence = match try_plugin_root_authoring_fence(&context.plugin_root) {
        Ok(Some(fence)) => fence,
        Ok(None) => return Ok(CandidatePreparation::AuthorityBusy),
        Err(detail) => {
            return Ok(CandidatePreparation::Retryable(Box::new(
                retryable_reconcile_event(detail),
            )));
        }
    };
    let coordinator = match crate::authority::AuthorityCoordinator::prepare(&context.store_root) {
        Ok(coordinator) => coordinator,
        Err(detail) => {
            return Ok(CandidatePreparation::Retryable(Box::new(
                retryable_reconcile_event(format!(
                    "failed to prepare Generation authority: {detail}"
                )),
            )));
        }
    };
    let authority_fence = match coordinator.try_snapshot() {
        Ok(Some(authority_fence)) => authority_fence,
        Ok(None) => return Ok(CandidatePreparation::AuthorityBusy),
        Err(detail) => {
            return Ok(CandidatePreparation::Retryable(Box::new(
                retryable_reconcile_event(format!(
                    "failed to snapshot Generation authority: {detail}"
                )),
            )));
        }
    };
    FULL_RECONCILE_ATTEMPTS.fetch_add(1, Ordering::Relaxed);
    let Some(desired) = resolve_desired_generation(
        &context.plugin_root,
        &context.store_root,
        &context.host_build,
        context.profile_name.as_deref(),
        &mut context.last_attempted_desired,
    )?
    else {
        return Ok(CandidatePreparation::NoChange);
    };
    let desired_selection = desired.selection();
    context
        .online_generation
        .borrow_mut()
        .observe_desired(desired_selection.clone());
    let state = match context.client.inspect().await.map_err(control_error) {
        Ok(state) => state,
        Err(detail) => {
            context.last_attempted_desired = None;
            return Ok(CandidatePreparation::Retryable(Box::new(
                retryable_reconcile_event(format!(
                    "Generation Controller inspection failed: {detail}"
                )),
            )));
        }
    };
    let Some(previous_generation_spec_digest) = state.active_generation_spec_digest.clone() else {
        context.last_attempted_desired = None;
        return Ok(CandidatePreparation::Retryable(Box::new(
            retryable_reconcile_event(
                "online reconcile requires one active App Generation".to_owned(),
            ),
        )));
    };
    context.last_attempted_desired = Some(AttemptedDesiredState::new(
        &desired.plugin_root_revision,
        &desired.desired_state_digest,
    ));
    Ok(CandidatePreparation::Ready(Box::new(
        PreparedOnlineCandidate {
            authoring_fence,
            authority_fence,
            desired,
            desired_selection,
            state,
            previous_generation_spec_digest,
        },
    )))
}

fn try_plugin_root_authoring_fence(plugin_root: &Path) -> Result<Option<fs::File>, String> {
    let home = plugin_root
        .parent()
        .ok_or_else(|| "Plugin Root has no App root".to_owned())?;
    let path = home.join(".lenso/plugin-root-authoring.lock");
    fs::create_dir_all(
        path.parent()
            .expect("Plugin Root authoring lock has a parent"),
    )
    .map_err(|error| format!("failed to prepare Plugin Root authoring lock: {error}"))?;
    let file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(|error| format!("failed to open {}: {error}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(fs::TryLockError::WouldBlock) => Ok(None),
        Err(fs::TryLockError::Error(error)) => {
            Err(format!("failed to lock {}: {error}", path.display()))
        }
    }
}

fn retryable_reconcile_event(detail: String) -> OnlineGenerationEvent {
    OnlineGenerationEvent::WatchDegraded { detail }
}

fn retryable_candidate_failure(
    last_attempted_desired: &mut Option<AttemptedDesiredState>,
    detail: String,
) -> OnlineGenerationEvent {
    *last_attempted_desired = None;
    retryable_reconcile_event(detail)
}

fn resolve_desired_generation(
    plugin_root: &Path,
    store_root: &Path,
    host_build: &HostBuildIdentity,
    profile_name: Option<&str>,
    last_attempted_desired: &mut Option<AttemptedDesiredState>,
) -> Result<Option<DesiredGeneration>, Box<OnlineGenerationEvent>> {
    let directories = directories_for_store_root(store_root).map_err(|detail| {
        Box::new(OnlineGenerationEvent::Rejected {
            plugin_root_revision: None,
            resolution_authority_digest: None,
            desired_state_digest: None,
            plan_digest: None,
            detail,
        })
    })?;
    let authority = crate::generation_authority::load_generation_authority_unfenced(store_root);
    let rejected_without_revision = |detail| {
        Box::new(OnlineGenerationEvent::Rejected {
            plugin_root_revision: None,
            resolution_authority_digest: Some(authority.resolution_authority_digest.clone()),
            desired_state_digest: None,
            plan_digest: None,
            detail,
        })
    };
    let root = crate::plugin_root::snapshot_with_resources(plugin_root)
        .map_err(rejected_without_revision)?;
    let plugin_root_revision = root.revision().map_err(rejected_without_revision)?;
    let rejected = |detail| {
        Box::new(OnlineGenerationEvent::Rejected {
            plugin_root_revision: Some(plugin_root_revision.clone()),
            resolution_authority_digest: Some(authority.resolution_authority_digest.clone()),
            desired_state_digest: None,
            plan_digest: None,
            detail,
        })
    };
    let plan = if let Some(profile_name) = profile_name {
        let profile = crate::profile::select(profile_name, root.root(), &directories.profiles())
            .map_err(rejected)?;
        resolve_host_plan_for_agent_in(&directories, profile.root(), profile.agent())
            .map_err(rejected)?
    } else {
        resolve_host_plan_in(&directories, root.root()).map_err(rejected)?
    };
    let resources =
        crate::plugin_root::plan_resources_from_snapshot(&root, &plan).map_err(rejected)?;
    let (desired_state_digest, plan_digest) =
        desired_generation_identity(&authority.resolution_authority_digest, &plan, &resources)
            .map_err(rejected)?;
    if last_attempted_desired.as_ref()
        == Some(&AttemptedDesiredState::new(
            &plugin_root_revision,
            &desired_state_digest,
        ))
    {
        return Ok(None);
    }
    let generation =
        resolve_generation_from_plan(&plan, &authority, host_build, plugin_root, resources)
            .map_err(rejected)?;
    Ok(Some(DesiredGeneration {
        plugin_root_revision,
        resolution_authority_digest: authority.resolution_authority_digest,
        desired_state_digest,
        plan_digest,
        generation,
    }))
}

fn push_reconcile_event(
    events: &Rc<RefCell<OnlineGenerationEventLog>>,
    event: OnlineGenerationEvent,
) -> u64 {
    events.borrow_mut().push(event)
}

fn report_watcher_errors(
    events: &Rc<RefCell<OnlineGenerationEventLog>>,
    errors: impl IntoIterator<Item = String>,
) {
    for detail in errors {
        let event = OnlineGenerationEvent::WatchDegraded { detail };
        let duplicate = events
            .borrow()
            .after(None)
            .events()
            .last()
            .is_some_and(|record| record.event() == &event);
        if !duplicate {
            push_reconcile_event(events, event);
        }
    }
}

fn apply_online_generation_event(
    tracker: &Rc<RefCell<OnlineGenerationTracker>>,
    event: &OnlineGenerationEvent,
    cursor: u64,
) {
    let mut tracker = tracker.borrow_mut();
    match event {
        OnlineGenerationEvent::Preparing { .. } | OnlineGenerationEvent::WatchDegraded { .. } => {}
        OnlineGenerationEvent::Switched {
            generation_spec_digest,
            ..
        } => tracker.switched(generation_spec_digest),
        event @ OnlineGenerationEvent::Rejected { .. } => tracker.rejected(event, cursor),
        OnlineGenerationEvent::Failed {
            generation_spec_digest,
            ..
        } => tracker.failed(generation_spec_digest, cursor),
        OnlineGenerationEvent::RolledBack {
            failed_generation_spec_digest,
            restored_generation_spec_digest,
            ..
        } => tracker.rolled_back(
            failed_generation_spec_digest,
            restored_generation_spec_digest,
            cursor,
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        process::{Command, Stdio},
        time::Instant,
    };

    use lenso_plugin_control_plane::{ActivationDirection, GenerationControlRecord};

    use super::*;

    fn two_generation_fixture() -> (DesiredGeneration, DesiredGeneration) {
        let directory = tempfile::tempdir().unwrap();
        let plugin_root = directory.path().join("plugins");
        let store_root = directory.path().join("state");
        fs::create_dir_all(&store_root).unwrap();
        let host_build = HostBuildIdentity::current().unwrap();
        let mut last_attempted = None;
        let first = resolve_desired_generation(
            &plugin_root,
            &store_root,
            &host_build,
            None,
            &mut last_attempted,
        )
        .unwrap()
        .unwrap();

        let text_tools = plugin_root.join("lenso.agent.text-tools");
        fs::create_dir_all(&text_tools).unwrap();
        fs::write(text_tools.join("default.toml"), "").unwrap();
        let second = resolve_desired_generation(
            &plugin_root,
            &store_root,
            &host_build,
            None,
            &mut last_attempted,
        )
        .unwrap()
        .unwrap();
        (first, second)
    }

    fn durable_state_with_active(active_generation_spec_digest: &str) -> DurableControlState {
        DurableControlState {
            schema_version: 1,
            app_id: "lenso.agent.harness".to_owned(),
            revision: 2,
            supervisor_epoch: 1,
            routing_epoch: 2,
            host_suspended: false,
            active_generation_spec_digest: Some(active_generation_spec_digest.to_owned()),
            generations: vec![GenerationControlRecord {
                generation_spec_digest: active_generation_spec_digest.to_owned(),
                transition_spec_digest: "fixture-transition".to_owned(),
                lifecycle: ControlLifecycle::Active,
                health: ControlHealth::Healthy,
                activation_direction: ActivationDirection::Rollback,
                ready_timeout_nanos: "1".to_owned(),
                drain_timeout_nanos: "1".to_owned(),
                drain_deadline_unix_nanos: None,
                rollback_deadline_unix_nanos: None,
                automatic_rollback_on_generation_failure: true,
                state_compatibility_receipt_digests: Vec::new(),
                retirement_reason: None,
            }],
        }
    }

    #[test]
    fn relative_plugin_root_watches_the_current_directory() {
        assert_eq!(watch_parent(Path::new("plugins")), Path::new("."));
    }

    #[test]
    fn repeated_retryable_degradation_does_not_advance_the_event_cursor() {
        let (first, _) = two_generation_fixture();
        let events = Rc::new(RefCell::new(OnlineGenerationEventLog::default()));
        let tracker = Rc::new(RefCell::new(OnlineGenerationTracker::new(
            first.selection(),
        )));
        let mut last_outcome = None;
        let degraded = OnlineGenerationEvent::WatchDegraded {
            detail: "persistent Controller inspection failure".to_owned(),
        };

        record_reconcile_outcome(&events, &tracker, &mut last_outcome, degraded.clone());
        record_reconcile_outcome(&events, &tracker, &mut last_outcome, degraded);

        let page = events.borrow().after(None);
        assert_eq!(page.cursor(), 1);
        assert_eq!(page.events().len(), 1);
    }

    #[test]
    fn successful_no_change_reconcile_starts_a_new_degradation_episode() {
        let (first, _) = two_generation_fixture();
        let events = Rc::new(RefCell::new(OnlineGenerationEventLog::default()));
        let tracker = Rc::new(RefCell::new(OnlineGenerationTracker::new(
            first.selection(),
        )));
        let mut last_outcome = None;
        let degraded = OnlineGenerationEvent::WatchDegraded {
            detail: "intermittent Controller inspection failure".to_owned(),
        };

        record_reconcile_outcome(&events, &tracker, &mut last_outcome, degraded.clone());
        record_completed_reconcile_outcome(&events, &tracker, &mut last_outcome, None);
        record_reconcile_outcome(&events, &tracker, &mut last_outcome, degraded);

        let page = events.borrow().after(None);
        assert_eq!(page.cursor(), 2);
        assert_eq!(page.events().len(), 2);
    }

    #[test]
    fn same_generation_selection_advances_the_observable_desired_epoch() {
        let (first, _) = two_generation_fixture();
        let first = first.selection();
        let generation_spec_digest = first.generation_spec_digest().to_owned();
        let next = crate::online_generation::OnlineGenerationSelection::new(
            "sha256:root-next".to_owned(),
            "sha256:desired-next".to_owned(),
            generation_spec_digest.clone(),
            "sha256:plan-next".to_owned(),
            first.plan().clone(),
        );
        let mut tracker = OnlineGenerationTracker::new(first);

        tracker.observe_desired(next.clone());
        project_same_generation_selection(&mut tracker, next, &generation_spec_digest);

        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.desired_epoch(), 1);
        assert_eq!(
            snapshot.active().desired_state_digest(),
            "sha256:desired-next"
        );
        assert_eq!(
            snapshot.desired().desired_state_digest(),
            "sha256:desired-next"
        );
        assert!(snapshot.preparing().is_none());
    }

    #[test]
    fn same_generation_fast_path_requires_durable_health() {
        let (first, _) = two_generation_fixture();
        let digest = first.generation.spec.digest();
        let mut state = durable_state_with_active(digest);
        assert!(durable_active_generation_is_healthy(&state, digest));

        state.generations[0].health = ControlHealth::Failed;

        assert!(!durable_active_generation_is_healthy(&state, digest));
    }

    #[test]
    fn explicit_authoring_reopen_allows_the_same_identity_to_retry() {
        let directory = tempfile::tempdir().unwrap();
        let plugin_root = directory.path().join("plugins");
        let store_root = directory.path().join("state");
        fs::create_dir_all(&store_root).unwrap();
        let host_build = HostBuildIdentity::current().unwrap();
        let mut last_attempted = None;
        let first = resolve_desired_generation(
            &plugin_root,
            &store_root,
            &host_build,
            None,
            &mut last_attempted,
        )
        .unwrap()
        .unwrap();
        last_attempted = Some(AttemptedDesiredState::new(
            &first.plugin_root_revision,
            &first.desired_state_digest,
        ));
        assert!(
            resolve_desired_generation(
                &plugin_root,
                &store_root,
                &host_build,
                None,
                &mut last_attempted,
            )
            .unwrap()
            .is_none()
        );

        reopen_explicit_attempt(&mut last_attempted);
        let retried = resolve_desired_generation(
            &plugin_root,
            &store_root,
            &host_build,
            None,
            &mut last_attempted,
        )
        .unwrap()
        .unwrap();

        assert_eq!(retried.plugin_root_revision, first.plugin_root_revision);
        assert_eq!(retried.desired_state_digest, first.desired_state_digest);
    }

    #[test]
    fn unchanged_consistency_probes_skip_repeated_full_plugin_root_io() {
        let directory = tempfile::tempdir().unwrap();
        let plugin_root = directory.path().join("plugins");
        fs::create_dir(&plugin_root).unwrap();
        let initial = crate::plugin_root::desired_state_probe(&plugin_root, None).unwrap();
        let mut consistency = ReconcileConsistencyState::new(Some(initial));
        let mut full_reconciles = 0;

        for _ in 1..RECONCILE_FULL_AUDIT_INTERVALS {
            let probe = crate::plugin_root::desired_state_probe(&plugin_root, None);
            if consistency.should_run_full_reconcile(probe) {
                full_reconciles += 1;
            }
        }

        assert_eq!(full_reconciles, 0);
        assert!(
            consistency.should_run_full_reconcile(crate::plugin_root::desired_state_probe(
                &plugin_root,
                None
            ))
        );
    }

    #[test]
    fn changed_consistency_probe_requires_canonical_reconciliation() {
        let directory = tempfile::tempdir().unwrap();
        let plugin_root = directory.path().join("plugins");
        fs::create_dir(&plugin_root).unwrap();
        let initial = crate::plugin_root::desired_state_probe(&plugin_root, None).unwrap();
        let mut consistency = ReconcileConsistencyState::new(Some(initial));

        fs::create_dir(plugin_root.join("example.plugin")).unwrap();

        assert!(
            consistency.should_run_full_reconcile(crate::plugin_root::desired_state_probe(
                &plugin_root,
                None
            ))
        );
    }

    #[test]
    fn busy_authority_is_retried_on_the_next_consistency_probe() {
        let directory = tempfile::tempdir().unwrap();
        let plugin_root = directory.path().join("plugins");
        fs::create_dir(&plugin_root).unwrap();
        let initial = crate::plugin_root::desired_state_probe(&plugin_root, None).unwrap();
        let mut consistency = ReconcileConsistencyState::new(Some(initial));

        consistency.retry_required();

        assert!(
            consistency.should_run_full_reconcile(crate::plugin_root::desired_state_probe(
                &plugin_root,
                None
            ))
        );
    }

    #[test]
    fn unit_test_canonical_telemetry_is_isolated_from_parallel_test_threads() {
        let directory = tempfile::tempdir().unwrap();
        let plugin_root = directory.path().join("plugins");
        fs::create_dir(&plugin_root).unwrap();
        let before = telemetry();

        std::thread::spawn(move || crate::plugin_root::snapshot(&plugin_root).unwrap())
            .join()
            .unwrap();

        assert_eq!(telemetry().delta(before).canonical_snapshots, 0);
    }

    #[test]
    fn authoring_lock_child_process() {
        let Ok(root) = std::env::var("LENSO_RECONCILE_LOCK_CHILD_ROOT") else {
            return;
        };
        let ready = PathBuf::from(std::env::var("LENSO_RECONCILE_LOCK_CHILD_READY").unwrap());
        let release = PathBuf::from(std::env::var("LENSO_RECONCILE_LOCK_CHILD_RELEASE").unwrap());
        let lock_path = Path::new(&root).join(".lenso/plugin-root-authoring.lock");
        fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
        let lock = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(lock_path)
            .unwrap();
        lock.lock().unwrap();
        fs::write(&ready, "ready").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !release.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            release.exists(),
            "parent did not release child authoring lock"
        );
    }

    #[test]
    fn shared_external_authoring_lock_suppresses_canonical_snapshot_until_release() {
        let directory = tempfile::tempdir().unwrap();
        let plugin_root = directory.path().join("plugins");
        fs::create_dir(&plugin_root).unwrap();
        let ready = directory.path().join("child-ready");
        let release = directory.path().join("child-release");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "generation::online_reconciler::tests::authoring_lock_child_process",
                "--nocapture",
            ])
            .env("LENSO_RECONCILE_LOCK_CHILD_ROOT", directory.path())
            .env("LENSO_RECONCILE_LOCK_CHILD_READY", &ready)
            .env("LENSO_RECONCILE_LOCK_CHILD_RELEASE", &release)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !ready.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(ready.exists(), "child did not acquire authoring lock");
        let before = telemetry();

        assert!(
            try_plugin_root_authoring_fence(&plugin_root)
                .unwrap()
                .is_none()
        );
        assert_eq!(telemetry().delta(before).canonical_snapshots, 0);

        fs::write(&release, "release").unwrap();
        assert!(child.wait().unwrap().success());
        assert!(
            try_plugin_root_authoring_fence(&plugin_root)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn unchanged_desired_state_can_retry_after_a_transient_downstream_failure() {
        struct FakeController {
            fail_next_transition: bool,
        }

        impl FakeController {
            fn transition(&mut self) -> Result<(), String> {
                if std::mem::take(&mut self.fail_next_transition) {
                    Err("fixture Controller failed its first transition".to_owned())
                } else {
                    Ok(())
                }
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let plugin_root = directory.path().join("plugins");
        let store_root = directory.path().join("state");
        fs::create_dir_all(&store_root).unwrap();
        let host_build = HostBuildIdentity::current().unwrap();
        let mut last_attempted = None;
        let mut controller = FakeController {
            fail_next_transition: true,
        };

        let first = resolve_desired_generation(
            &plugin_root,
            &store_root,
            &host_build,
            None,
            &mut last_attempted,
        )
        .unwrap()
        .unwrap();
        // The candidate is remembered only after Controller inspection. A
        // retryable record/transition failure clears that memory and emits a
        // non-terminal degraded event, so unchanged Desired State is retried.
        last_attempted = Some(AttemptedDesiredState::new(
            &first.plugin_root_revision,
            &first.desired_state_digest,
        ));
        let failure =
            retryable_candidate_failure(&mut last_attempted, controller.transition().unwrap_err());
        assert!(matches!(
            failure,
            OnlineGenerationEvent::WatchDegraded { .. }
        ));
        let retried = resolve_desired_generation(
            &plugin_root,
            &store_root,
            &host_build,
            None,
            &mut last_attempted,
        )
        .unwrap()
        .unwrap();

        assert_eq!(retried.desired_state_digest, first.desired_state_digest);
        controller.transition().unwrap();
    }

    #[test]
    fn durable_resync_restores_a_rollback_missed_by_the_broadcast_stream() {
        let (first, second) = two_generation_fixture();
        let first_digest = first.generation.spec.digest().to_owned();
        let second_digest = second.generation.spec.digest().to_owned();
        let mut projection = OnlineGenerationTracker::new(first.selection());
        projection.preparing(second.selection());
        projection.switched(&second_digest);
        projection.preparing(second.selection());
        let projection = Rc::new(RefCell::new(projection));
        let mut durable = durable_state_with_active(&first_digest);
        let mut failed = durable.generations[0].clone();
        failed.generation_spec_digest = second_digest.clone();
        failed.lifecycle = ControlLifecycle::Retired;
        failed.health = ControlHealth::Failed;
        failed.activation_direction = ActivationDirection::Forward;
        durable.generations.push(failed);

        let (event, resynchronized) = lagged_controller_resync_event(&projection, &durable, 3);
        let events = Rc::new(RefCell::new(OnlineGenerationEventLog::default()));
        let cursor = push_reconcile_event(&events, event.clone());
        if resynchronized {
            projection
                .borrow_mut()
                .observe_resynchronized_rejection(cursor);
        }
        let snapshot = projection.borrow().snapshot();

        assert!(
            matches!(event, OnlineGenerationEvent::WatchDegraded { detail } if detail.contains("resynchronized"))
        );
        assert_eq!(snapshot.active().generation_spec_digest(), first_digest);
        assert!(snapshot.preparing().is_none());
        assert_eq!(
            snapshot
                .rejected()
                .expect("durable failed Generation must survive a lagged rollback")
                .generation_spec_digest(),
            second_digest
        );
        assert_eq!(snapshot.rejected_cursor(), Some(cursor));
    }

    #[test]
    fn later_healthy_durable_resync_clears_old_terminal_rejection() {
        let (first, second) = two_generation_fixture();
        let first_digest = first.generation.spec.digest().to_owned();
        let second_digest = second.generation.spec.digest().to_owned();
        let mut projection = OnlineGenerationTracker::new(first.selection());
        projection.preparing(second.selection());
        projection.failed(&second_digest, 1);
        let projection = Rc::new(RefCell::new(projection));

        let (_, resynchronized) = lagged_controller_resync_event(
            &projection,
            &durable_state_with_active(&first_digest),
            1,
        );
        let snapshot = projection.borrow().snapshot();

        assert!(resynchronized);
        assert!(snapshot.rejected().is_none());
        assert_eq!(snapshot.rejected_cursor(), None);
    }

    #[test]
    fn terminal_rejection_identity_survives_the_bounded_event_window() {
        let (first, second) = two_generation_fixture();
        let second_digest = second.generation.spec.digest().to_owned();
        let tracker = Rc::new(RefCell::new(OnlineGenerationTracker::new(
            first.selection(),
        )));
        tracker.borrow_mut().preparing(second.selection());
        let events = Rc::new(RefCell::new(OnlineGenerationEventLog::default()));
        let failure = OnlineGenerationEvent::Failed {
            generation_spec_digest: second_digest.clone(),
            detail: "candidate preparation failed".to_owned(),
        };
        let failure_cursor = push_reconcile_event(&events, failure.clone());
        apply_online_generation_event(&tracker, &failure, failure_cursor);
        for index in 0..80 {
            push_reconcile_event(
                &events,
                OnlineGenerationEvent::WatchDegraded {
                    detail: format!("later watcher event {index}"),
                },
            );
        }

        let retained_page = events.borrow().after(Some(0));
        assert!(retained_page.truncated());
        assert!(
            retained_page
                .events()
                .iter()
                .all(|record| !matches!(record.event(), OnlineGenerationEvent::Failed { .. }))
        );
        let snapshot = tracker.borrow().snapshot();
        assert_eq!(
            snapshot
                .rejected()
                .expect("terminal identity must outlive the event window")
                .generation_spec_digest(),
            second_digest
        );
        assert_eq!(snapshot.rejected_cursor(), Some(failure_cursor));
        assert!(snapshot.preparing().is_none());
    }

    #[test]
    fn durable_resync_preserves_projection_when_active_generation_is_not_retained() {
        let (first, second) = two_generation_fixture();
        let first_digest = first.generation.spec.digest().to_owned();
        let second_digest = second.generation.spec.digest().to_owned();
        let mut projection = OnlineGenerationTracker::new(first.selection());
        projection.preparing(second.selection());
        let projection = Rc::new(RefCell::new(projection));

        let (event, resynchronized) = lagged_controller_resync_event(
            &projection,
            &durable_state_with_active("sha256:missing-retained-generation"),
            5,
        );
        let snapshot = projection.borrow().snapshot();

        assert!(
            matches!(event, OnlineGenerationEvent::WatchDegraded { detail } if detail.contains("not retained"))
        );
        assert!(!resynchronized);
        assert_eq!(snapshot.active().generation_spec_digest(), first_digest);
        assert_eq!(
            snapshot
                .preparing()
                .expect("failed resync must preserve the in-flight projection")
                .generation_spec_digest(),
            second_digest
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn recursive_watch_attaches_after_late_create_and_root_recreation() {
        let directory = tempfile::tempdir().unwrap();
        let plugin_root = directory.path().join("plugins");
        let (mut watcher, errors) =
            FilesystemReconcileWatcher::start(&[directory.path()], Some(plugin_root.clone()));
        assert!(errors.is_empty(), "{errors:?}");

        let nested = plugin_root.join("example/default/prompts");
        fs::create_dir_all(&nested).unwrap();
        assert!(watcher.refresh_recursive_watch().is_none());
        assert_eq!(watcher.recursive_watch_attachments, 1);
        watcher.settle_after(None).await;

        fs::write(nested.join("first.md"), "first").unwrap();
        let signal = tokio::time::timeout(Duration::from_secs(10), watcher.changed())
            .await
            .expect("recursive watcher missed a nested edit")
            .expect("watcher channel closed");
        watcher.settle_after(Some(signal)).await;
        assert!(watcher.refresh_recursive_watch().is_none());
        assert_eq!(watcher.recursive_watch_attachments, 1);

        fs::remove_dir_all(&plugin_root).unwrap();
        let recreated = plugin_root.join("example/default/prompts");
        fs::create_dir_all(&recreated).unwrap();
        assert!(watcher.refresh_recursive_watch().is_none());
        assert_eq!(watcher.recursive_watch_attachments, 2);
        watcher.settle_after(None).await;

        fs::write(recreated.join("second.md"), "second").unwrap();
        let signal = tokio::time::timeout(Duration::from_secs(10), watcher.changed())
            .await
            .expect("recursive watcher missed an edit after root recreation")
            .expect("watcher channel closed");
        watcher.settle_after(Some(signal)).await;
        assert!(watcher.refresh_recursive_watch().is_none());
        assert_eq!(watcher.recursive_watch_attachments, 2);
    }

    #[test]
    fn plugin_root_edits_derive_a_new_generation_and_reject_invalid_state() {
        let directory = tempfile::tempdir().unwrap();
        let plugin_root = directory.path().join("plugins");
        let store_root = directory.path().join("state");
        fs::create_dir_all(&store_root).unwrap();
        let host_build = HostBuildIdentity::current().unwrap();
        let mut last_attempted = None;

        let base = resolve_desired_generation(
            &plugin_root,
            &store_root,
            &host_build,
            None,
            &mut last_attempted,
        )
        .unwrap()
        .unwrap()
        .generation;

        let text_tools = plugin_root.join("lenso.agent.text-tools");
        fs::create_dir_all(&text_tools).unwrap();
        fs::write(text_tools.join("default.toml"), "").unwrap();
        let configured = resolve_desired_generation(
            &plugin_root,
            &store_root,
            &host_build,
            None,
            &mut last_attempted,
        )
        .unwrap()
        .unwrap()
        .generation;
        assert_ne!(configured.spec.digest(), base.spec.digest());
        assert!(
            configured
                .plan
                .plugin_instances()
                .iter()
                .any(|plugin| plugin.instance_key() == "lenso.agent.text-tools/default")
        );

        fs::write(text_tools.join("default.toml"), "not valid = [").unwrap();
        let rejected = resolve_desired_generation(
            &plugin_root,
            &store_root,
            &host_build,
            None,
            &mut last_attempted,
        )
        .unwrap_err();
        assert!(matches!(*rejected, OnlineGenerationEvent::Rejected { .. }));

        fs::remove_dir_all(text_tools).unwrap();
        let restored = resolve_desired_generation(
            &plugin_root,
            &store_root,
            &host_build,
            None,
            &mut last_attempted,
        )
        .unwrap()
        .unwrap()
        .generation;
        assert_eq!(restored.spec.digest(), base.spec.digest());
    }

    #[test]
    fn excluded_profile_mutations_keep_projecting_each_new_plugin_root_revision() {
        let directory = tempfile::tempdir().unwrap();
        let plugin_root = directory.path().join("plugins");
        let store_root = directory.path().join("state");
        fs::create_dir_all(directory.path().join("profiles")).unwrap();
        fs::create_dir_all(&store_root).unwrap();
        fs::write(
            directory.path().join("profiles/web.toml"),
            "instances = []\n",
        )
        .unwrap();
        let host_build = HostBuildIdentity::current().unwrap();
        let mut last_attempted = None;

        let initial = resolve_desired_generation(
            &plugin_root,
            &store_root,
            &host_build,
            Some("web"),
            &mut last_attempted,
        )
        .unwrap()
        .unwrap();
        last_attempted = Some(AttemptedDesiredState::new(
            &initial.plugin_root_revision,
            &initial.desired_state_digest,
        ));

        let excluded = plugin_root.join("example.excluded");
        fs::create_dir_all(&excluded).unwrap();
        fs::write(excluded.join("one.toml"), "").unwrap();
        let first_mutation = resolve_desired_generation(
            &plugin_root,
            &store_root,
            &host_build,
            Some("web"),
            &mut last_attempted,
        )
        .unwrap()
        .expect("root-only mutation must not be hidden by Generation identity deduplication");

        last_attempted = Some(AttemptedDesiredState::new(
            &first_mutation.plugin_root_revision,
            &first_mutation.desired_state_digest,
        ));
        fs::write(excluded.join("two.toml"), "").unwrap();
        let second_mutation = resolve_desired_generation(
            &plugin_root,
            &store_root,
            &host_build,
            Some("web"),
            &mut last_attempted,
        )
        .unwrap()
        .expect("each root-only mutation must project its own Plugin Root revision");

        assert_ne!(
            first_mutation.plugin_root_revision,
            second_mutation.plugin_root_revision
        );
        assert_eq!(
            first_mutation.generation.spec.digest(),
            second_mutation.generation.spec.digest()
        );
    }

    #[test]
    fn resource_only_edits_create_a_generation_and_retain_old_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let plugin_root = directory.path().join("plugins");
        let store_root = directory.path().join("state");
        let text_tools = plugin_root.join("lenso.agent.text-tools");
        let resource_directory = text_tools.join("default/prompts");
        fs::create_dir_all(&resource_directory).unwrap();
        fs::create_dir_all(&store_root).unwrap();
        fs::write(text_tools.join("default.toml"), "").unwrap();
        let resource = resource_directory.join("system.md");
        fs::write(&resource, "generation one").unwrap();
        let host_build = HostBuildIdentity::current().unwrap();
        let mut last_attempted = None;

        let first = resolve_desired_generation(
            &plugin_root,
            &store_root,
            &host_build,
            None,
            &mut last_attempted,
        )
        .unwrap()
        .unwrap()
        .generation;
        let retained_resources = first.resources.clone();

        fs::write(&resource, "generation two").unwrap();
        let second = resolve_desired_generation(
            &plugin_root,
            &store_root,
            &host_build,
            None,
            &mut last_attempted,
        )
        .unwrap()
        .unwrap()
        .generation;

        assert_ne!(second.spec.digest(), first.spec.digest());
        assert_eq!(
            retained_resources
                .for_instance("lenso.agent.text-tools/default")
                .read_text("prompts/system.md")
                .unwrap(),
            "generation one"
        );
        assert_eq!(
            second
                .resources
                .for_instance("lenso.agent.text-tools/default")
                .read_text("prompts/system.md")
                .unwrap(),
            "generation two"
        );
    }
}
