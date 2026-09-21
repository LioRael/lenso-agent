//! Deliberately small external durable state Provider used by V05/V06.
//!
//! The store is file-backed solely to prove a process restart. Its public
//! behavior is defined by the Capability contracts; neither the runner nor a
//! caller receives a direct file handle or another Plugin's owner identity.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use lenso::prelude::*;
use lenso_capability_agent_durable_task as durable;
use lenso_capability_agent_extension_state as extension;

/// Fixture artifact identity compiled into each separately built Provider.
pub fn artifact_version() -> &'static str {
    if cfg!(feature = "incompatible") {
        "3.0.0"
    } else if cfg!(feature = "upgraded") {
        "2.0.0"
    } else {
        "1.0.0"
    }
}

fn supported_state_version() -> &'static str {
    if cfg!(feature = "incompatible") {
        "2"
    } else {
        "1"
    }
}

#[derive(Clone, Debug, serde::Deserialize, PluginConfig)]
#[serde(deny_unknown_fields)]
struct StateTaskConfig {
    storage_path: String,
}

#[derive(Clone, Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct Store {
    records: Vec<extension::ExtensionRecord>,
    start_ids: BTreeMap<String, String>,
    signals: BTreeSet<String>,
    tasks: BTreeMap<String, durable::TaskSnapshot>,
}

#[lenso::plugin(validate = validate_config)]
#[derive(Clone, Debug)]
struct StateTaskProvider {
    #[config]
    config: StateTaskConfig,
    #[tasks]
    tasks: ManagedTasks,
}

fn validate_config(config: &StateTaskConfig) -> Result<(), RuntimeFailure> {
    let path = Path::new(&config.storage_path);
    if !path.is_absolute() || config.storage_path.len() > 4_096 {
        return Err(RuntimeFailure::InvalidResolvedPlan {
            detail: "external durable-state fixture requires an absolute bounded storage path"
                .to_owned(),
        });
    }
    Ok(())
}

impl StateTaskProvider {
    fn storage_path(&self) -> PathBuf {
        PathBuf::from(&self.config.storage_path)
    }

    fn load(&self) -> Result<Store, RuntimeFailure> {
        let path = self.storage_path();
        match fs::read(&path) {
            Ok(bytes) => {
                let mut store: Store = serde_json::from_slice(&bytes).map_err(|error| {
                    RuntimeFailure::PluginFailure {
                        detail: format!(
                            "external durable-state fixture cannot decode {}: {error}",
                            path.display()
                        ),
                    }
                })?;
                if settle_task_policy(&mut store) {
                    self.persist(&store)?;
                }
                Ok(store)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Store::default()),
            Err(error) => Err(RuntimeFailure::PluginFailure {
                detail: format!(
                    "external durable-state fixture cannot read {}: {error}",
                    path.display()
                ),
            }),
        }
    }

    fn persist(&self, store: &Store) -> Result<(), RuntimeFailure> {
        let path = self.storage_path();
        let parent = path.parent().ok_or_else(|| RuntimeFailure::PluginFailure {
            detail: "external durable-state fixture storage path has no parent".to_owned(),
        })?;
        fs::create_dir_all(parent).map_err(|error| RuntimeFailure::PluginFailure {
            detail: format!(
                "external durable-state fixture cannot create {}: {error}",
                parent.display()
            ),
        })?;
        let temporary = parent.join(format!(".{}.tmp", std::process::id()));
        let bytes = serde_json::to_vec_pretty(store).map_err(|error| RuntimeFailure::Internal {
            detail: format!("external durable-state fixture cannot encode store: {error}"),
        })?;
        fs::write(&temporary, bytes).map_err(|error| RuntimeFailure::PluginFailure {
            detail: format!(
                "external durable-state fixture cannot write {}: {error}",
                temporary.display()
            ),
        })?;
        fs::rename(&temporary, &path).map_err(|error| RuntimeFailure::PluginFailure {
            detail: format!(
                "external durable-state fixture cannot publish {}: {error}",
                path.display()
            ),
        })
    }

    fn owner(context: &Ctx) -> PluginResult<String, extension::AppendError> {
        context
            .caller_instance()
            .map(ToOwned::to_owned)
            .ok_or_else(|| PluginError::domain(extension::AppendError::OwnerUnavailable))
    }

    fn task_owner(context: &Ctx) -> PluginResult<String, durable::StartError> {
        context
            .caller_instance()
            .map(ToOwned::to_owned)
            .ok_or_else(|| PluginError::domain(durable::StartError::OwnerMismatch))
    }

    fn task_for_owner(
        store: &Store,
        task_id: &str,
        owner: &str,
    ) -> Result<durable::TaskSnapshot, durable::ObserveError> {
        let task = store
            .tasks
            .get(task_id)
            .cloned()
            .ok_or(durable::ObserveError::NotFound)?;
        if task.owner_instance != owner {
            return Err(durable::ObserveError::OwnerMismatch);
        }
        Ok(task)
    }
}

#[lenso::provides(extension::ExtensionState, durable::DurableTask)]
impl StateTaskProvider {
    async fn append(
        &self,
        context: Ctx,
        request: extension::AppendRequest,
    ) -> PluginResult<extension::AppendResponse, extension::AppendError> {
        let owner = Self::owner(&context)?;
        extension::validate_append_request(&request, &owner)
            .map_err(|_| PluginError::domain(extension::AppendError::InvalidRecord))?;
        let mut store = self.load().map_err(PluginError::runtime)?;
        if let Some(existing) = store.records.iter().find(|record| {
            record.owner_instance == owner
                && record.namespace == request.namespace
                && record.idempotency_key == request.idempotency_key
        }) {
            let candidate = extension::record_from_append(request.clone(), owner.clone());
            if existing == &candidate {
                return Ok(extension::AppendResponse {
                    record: existing.clone(),
                    duplicate: true,
                });
            }
            return Err(PluginError::domain(
                extension::AppendError::IdempotencyConflict,
            ));
        }
        if store.records.iter().any(|record| {
            record.owner_instance == owner
                && record.namespace == request.namespace
                && record.association == request.association
                && record.sequence == request.sequence
        }) {
            return Err(PluginError::domain(
                extension::AppendError::SequenceConflict,
            ));
        }
        let record = extension::record_from_append(request, owner);
        store.records.push(record.clone());
        self.persist(&store).map_err(PluginError::runtime)?;
        Ok(extension::AppendResponse {
            record,
            duplicate: false,
        })
    }

    async fn read(
        &self,
        context: Ctx,
        request: extension::ReadRequest,
    ) -> PluginResult<extension::ReadResponse, extension::ReadError> {
        let owner = context
            .caller_instance()
            .ok_or_else(|| PluginError::domain(extension::ReadError::OwnerUnavailable))?;
        extension::validate_read_request(&request)
            .map_err(|_| PluginError::domain(extension::ReadError::InvalidRecord))?;
        let after = request
            .after_sequence
            .as_ref()
            .and_then(|value| value.as_deref())
            .map(|value| value.parse::<u64>().expect("validated sequence"))
            .unwrap_or(0);
        let store = self.load().map_err(PluginError::runtime)?;
        let records = store
            .records
            .into_iter()
            .filter(|record| {
                record.owner_instance == owner
                    && record.namespace == request.namespace
                    && record.association == request.association
                    && record
                        .sequence
                        .parse::<u64>()
                        .is_ok_and(|value| value > after)
            })
            .take(usize::try_from(request.limit).unwrap_or(128))
            .collect();
        Ok(extension::ReadResponse { records })
    }
    async fn start(
        &self,
        context: Ctx,
        request: durable::StartRequest,
    ) -> PluginResult<durable::StartResponse, durable::StartError> {
        let owner = Self::task_owner(&context)?;
        durable::validate_start(
            &request.task_id,
            &request.idempotency_key,
            &request.workflow_kind,
            &request.state_version,
            request.state_json.as_str(),
            &owner,
        )
        .map_err(|_| PluginError::domain(durable::StartError::InvalidTask))?;
        let mut store = self.load().map_err(PluginError::runtime)?;
        if let Some(existing_id) = store.start_ids.get(&request.idempotency_key) {
            let task = store.tasks.get(existing_id).cloned().ok_or_else(|| {
                PluginError::runtime(RuntimeFailure::PluginFailure {
                    detail: "external durable-state fixture lost its idempotent task".to_owned(),
                })
            })?;
            if task.task_id == request.task_id && task.owner_instance == owner {
                return Ok(durable::StartResponse {
                    task,
                    duplicate: true,
                });
            }
            return Err(PluginError::domain(
                durable::StartError::IdempotencyConflict,
            ));
        }
        if store.tasks.contains_key(&request.task_id) {
            return Err(PluginError::domain(
                durable::StartError::IdempotencyConflict,
            ));
        }
        if let Some(Some(parent_id)) = &request.parent_task_id {
            let parent = store
                .tasks
                .get(parent_id)
                .ok_or_else(|| PluginError::domain(durable::StartError::InvalidTask))?;
            if parent.owner_instance != owner || !task_is_active(parent) {
                return Err(PluginError::domain(durable::StartError::InvalidTask));
            }
        }
        let uncertain = request.workflow_kind.contains("uncertain");
        let task = durable::TaskSnapshot {
            task_id: request.task_id.clone(),
            owner_instance: owner,
            workflow_kind: request.workflow_kind,
            state_version: request.state_version,
            revision: "1".to_owned(),
            status: if uncertain {
                durable::TaskStatus::UncertainExternalEffect
            } else {
                durable::TaskStatus::WaitingForSignal
            },
            state_json: request.state_json,
            parent_task_id: request.parent_task_id,
            deadline_unix_ms: request.deadline_unix_ms,
            terminal_reason_code: None,
        };
        durable::validate_snapshot(&task)
            .map_err(|_| PluginError::domain(durable::StartError::InvalidTask))?;
        store
            .start_ids
            .insert(request.idempotency_key, task.task_id.clone());
        store.tasks.insert(task.task_id.clone(), task.clone());
        settle_task_policy(&mut store);
        let task = store.tasks[&task.task_id].clone();
        self.persist(&store).map_err(PluginError::runtime)?;
        Ok(durable::StartResponse {
            task,
            duplicate: false,
        })
    }

    async fn observe(
        &self,
        context: Ctx,
        request: durable::ObserveRequest,
    ) -> PluginResult<durable::ObserveResponse, durable::ObserveError> {
        let owner = context
            .caller_instance()
            .ok_or_else(|| PluginError::domain(durable::ObserveError::OwnerMismatch))?;
        let store = self.load().map_err(PluginError::runtime)?;
        let task =
            Self::task_for_owner(&store, &request.task_id, owner).map_err(PluginError::domain)?;
        Ok(durable::ObserveResponse { task })
    }

    async fn signal(
        &self,
        context: Ctx,
        request: durable::SignalRequest,
    ) -> PluginResult<durable::SignalResponse, durable::SignalError> {
        let owner = context
            .caller_instance()
            .ok_or_else(|| PluginError::domain(durable::SignalError::OwnerMismatch))?;
        let mut store = self.load().map_err(PluginError::runtime)?;
        let task = store
            .tasks
            .get(&request.task_id)
            .cloned()
            .ok_or_else(|| PluginError::domain(durable::SignalError::NotFound))?;
        if task.owner_instance != owner {
            return Err(PluginError::domain(durable::SignalError::OwnerMismatch));
        }
        if store.signals.contains(&request.signal_id) {
            return Ok(durable::SignalResponse {
                task,
                duplicate: true,
            });
        }
        if task.status == durable::TaskStatus::Cancelled {
            return Err(PluginError::domain(durable::SignalError::TaskCancelled));
        }
        if task.status == durable::TaskStatus::UncertainExternalEffect {
            return Err(PluginError::domain(
                durable::SignalError::UncertainExternalEffect,
            ));
        }
        if task.status != durable::TaskStatus::WaitingForSignal
            || task.revision != request.expected_revision
            || request.kind != "approval_granted"
        {
            return Err(PluginError::domain(durable::SignalError::StaleSignal));
        }
        let _: serde_json::Value = serde_json::from_str(request.payload_json.as_str())
            .map_err(|_| PluginError::domain(durable::SignalError::InvalidTask))?;
        let mut resumed = task;
        resumed.status = durable::TaskStatus::Ready;
        resumed.revision = next_revision(&resumed.revision);
        resumed.state_json = if cfg!(feature = "upgraded") {
            // v2 adds execution provenance while still accepting v1 snapshots.
            r#"{"step":"approval_granted","handled_by":"2.0.0"}"#
        } else {
            r#"{"step":"approval_granted"}"#
        }
        .try_into()
        .expect("fixture state is JSON");
        store.signals.insert(request.signal_id);
        store.tasks.insert(resumed.task_id.clone(), resumed.clone());
        self.persist(&store).map_err(PluginError::runtime)?;
        Ok(durable::SignalResponse {
            task: resumed,
            duplicate: false,
        })
    }

    async fn cancel(
        &self,
        context: Ctx,
        request: durable::CancelRequest,
    ) -> PluginResult<durable::CancelResponse, durable::CancelError> {
        let owner = context
            .caller_instance()
            .ok_or_else(|| PluginError::domain(durable::CancelError::OwnerMismatch))?;
        let mut store = self.load().map_err(PluginError::runtime)?;
        let mut task = store
            .tasks
            .get(&request.task_id)
            .cloned()
            .ok_or_else(|| PluginError::domain(durable::CancelError::NotFound))?;
        if task.owner_instance != owner {
            return Err(PluginError::domain(durable::CancelError::OwnerMismatch));
        }
        let terminal = matches!(
            task.status,
            durable::TaskStatus::Completed
                | durable::TaskStatus::Cancelled
                | durable::TaskStatus::TimedOut
                | durable::TaskStatus::UncertainExternalEffect
                | durable::TaskStatus::UpgradeRequired
        );
        if !terminal {
            task.status = durable::TaskStatus::Cancelled;
            task.revision = next_revision(&task.revision);
            task.terminal_reason_code = Some(Some(request.reason_code));
            store.tasks.insert(task.task_id.clone(), task.clone());
            settle_task_policy(&mut store);
            self.persist(&store).map_err(PluginError::runtime)?;
        }
        Ok(durable::CancelResponse {
            task,
            already_terminal: terminal,
        })
    }

    async fn recover(
        &self,
        context: Ctx,
        request: durable::RecoverRequest,
    ) -> PluginResult<durable::RecoverResponse, durable::RecoverError> {
        let owner = context
            .caller_instance()
            .ok_or_else(|| PluginError::domain(durable::RecoverError::OwnerMismatch))?;
        let mut store = self.load().map_err(PluginError::runtime)?;
        let mut task = store
            .tasks
            .get(&request.task_id)
            .cloned()
            .ok_or_else(|| PluginError::domain(durable::RecoverError::NotFound))?;
        if task.owner_instance != owner {
            return Err(PluginError::domain(durable::RecoverError::OwnerMismatch));
        }
        // Recovery must also respect the installed implementation, not only a
        // caller's claim that it understands an arbitrary state version.
        let supported: Vec<_> = request
            .supported_state_versions
            .iter()
            .filter(|version| version.as_str() == supported_state_version())
            .cloned()
            .collect();
        let assessment = durable::assess_recovery(&task, &supported);
        if assessment == durable::RecoveryAssessment::BlockedUpgrade
            && task.status != durable::TaskStatus::UpgradeRequired
        {
            task.status = durable::TaskStatus::UpgradeRequired;
            task.revision = next_revision(&task.revision);
            task.terminal_reason_code = Some(Some("state_version_not_supported".to_owned()));
            store.tasks.insert(task.task_id.clone(), task.clone());
            self.persist(&store).map_err(PluginError::runtime)?;
        }
        let action = durable::RecoveryAction::from(assessment);
        let message = match action {
            durable::RecoveryAction::Resume => {
                "serializable state may resume under the selected artifact"
            }
            durable::RecoveryAction::Wait => "task remains durably waiting for a future signal",
            durable::RecoveryAction::ObserveOnly => "terminal task is readable but not resumable",
            durable::RecoveryAction::BlockedUpgrade => {
                "state version is not supported by this artifact"
            }
            durable::RecoveryAction::BlockedUnknownRequiredState => {
                "unknown required state blocks recovery"
            }
            durable::RecoveryAction::BlockedUncertainExternalEffect => {
                "external effect outcome is uncertain and will not be retried"
            }
        }
        .to_owned();
        Ok(durable::RecoverResponse {
            task,
            action,
            message,
        })
    }

    async fn wait(
        &self,
        context: Ctx,
        request: durable::WaitOpen,
    ) -> PluginResult<ProviderStream<durable::DurableTaskWait>, durable::WaitError> {
        let owner = context
            .caller_instance()
            .ok_or_else(|| PluginError::domain(durable::WaitError::OwnerMismatch))?;
        let store = self.load().map_err(PluginError::runtime)?;
        let task =
            Self::task_for_owner(&store, &request.task_id, owner).map_err(|error| match error {
                durable::ObserveError::NotFound => {
                    PluginError::domain(durable::WaitError::NotFound)
                }
                durable::ObserveError::OwnerMismatch => {
                    PluginError::domain(durable::WaitError::OwnerMismatch)
                }
                _ => PluginError::domain(durable::WaitError::InvalidTask),
            })?;
        let after = request.after_revision.parse::<u64>().unwrap_or(u64::MAX);
        let tasks = self.tasks.clone();
        let (stream, mut channel) =
            ProviderStream::<durable::DurableTaskWait>::channel(&context, 1);
        tasks
            .spawn_local(async move {
                let result = match channel.receive().await {
                    Ok(StreamInput::PeerHalfClosed) => {
                        if task
                            .revision
                            .parse::<u64>()
                            .is_ok_and(|revision| revision > after)
                        {
                            if let Err(error) = channel.send(durable::TaskUpdate { task }).await {
                                Err(PluginError::runtime(error))
                            } else {
                                Ok(())
                            }
                        } else {
                            Ok(())
                        }
                    }
                    Ok(StreamInput::Message(_)) => {
                        Err(PluginError::runtime(RuntimeFailure::ProtocolViolation {
                            capability: durable::CAPABILITY_ID,
                        }))
                    }
                    Err(error) => Err(PluginError::runtime(error)),
                };
                let _ = channel.complete(result).await;
            })
            .map_err(|error| {
                PluginError::runtime(RuntimeFailure::PluginFailure {
                    detail: format!("external durable-state wait task failed to start: {error:?}"),
                })
            })?;
        Ok(stream)
    }
}

fn task_is_active(task: &durable::TaskSnapshot) -> bool {
    matches!(
        task.status,
        durable::TaskStatus::Ready
            | durable::TaskStatus::Running
            | durable::TaskStatus::WaitingForSignal
    )
}

// This fixture settles deadlines at every admission/read, including restart.
// It does not promise a background timer while its process is stopped.
fn settle_task_policy(store: &mut Store) -> bool {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_millis();
    let mut changed = false;
    for task in store.tasks.values_mut() {
        if task_is_active(task)
            && task
                .deadline_unix_ms
                .as_ref()
                .and_then(Option::as_ref)
                .and_then(|value| value.parse::<u128>().ok())
                .is_some_and(|deadline| deadline <= now)
        {
            task.status = durable::TaskStatus::TimedOut;
            task.revision = next_revision(&task.revision);
            task.terminal_reason_code = Some(Some("deadline_elapsed".to_owned()));
            changed = true;
        }
    }
    loop {
        let cancelled_parents: BTreeSet<_> = store
            .tasks
            .values()
            .filter(|task| {
                matches!(
                    task.status,
                    durable::TaskStatus::Cancelled | durable::TaskStatus::TimedOut
                )
            })
            .map(|task| task.task_id.clone())
            .collect();
        let mut propagated = false;
        for task in store.tasks.values_mut() {
            if task_is_active(task)
                && task
                    .parent_task_id
                    .as_ref()
                    .and_then(Option::as_ref)
                    .is_some_and(|parent| cancelled_parents.contains(parent))
            {
                task.status = durable::TaskStatus::Cancelled;
                task.revision = next_revision(&task.revision);
                task.terminal_reason_code = Some(Some("parent_stopped".to_owned()));
                propagated = true;
            }
        }
        if !propagated {
            break;
        }
        changed = true;
    }
    changed
}

fn next_revision(value: &str) -> String {
    value
        .parse::<u64>()
        .unwrap_or(u64::MAX)
        .saturating_add(1)
        .to_string()
}

pub fn link() {
    link_plugin();
}
