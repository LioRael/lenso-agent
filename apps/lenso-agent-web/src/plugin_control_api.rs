use std::collections::{BTreeMap, BTreeSet, VecDeque};

use lenso_agent_host::generation::{
    OnlineGenerationEvent, OnlineGenerationEventPage, OnlineGenerationRejectionObservation,
    OnlineGenerationSelection, OnlineGenerationSnapshot,
};
use lenso_app_authoring::PluginConfigurationAuthoritySource;
use lenso_app_plan::ResolvedAppPlan;
use serde::{Serialize, Serializer};
use tokio::sync::oneshot;

const MAX_PLUGIN_OPERATIONS: usize = 64;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PluginInventoryResponse {
    pub(super) applied_revision: Option<String>,
    pub(super) active: ActivePluginSelection,
    pub(super) configuration_authority: Option<PluginConfigurationAuthorityResponse>,
    pub(super) configuration_status: &'static str,
    pub(super) cursor: String,
    pub(super) desired: DesiredPluginSelection,
    pub(super) desired_revision: Option<String>,
    pub(super) events: Vec<PluginGenerationEvent>,
    pub(super) preparing: Option<PreparingPluginSelection>,
    pub(super) schema: &'static str,
    pub(super) stream_id: String,
    pub(super) truncated: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PluginConfigurationAuthorityResponse {
    pub(super) kind: String,
    pub(super) publication_history: bool,
    pub(super) reference: String,
    pub(super) rollback_proposals: bool,
}

impl From<PluginConfigurationAuthoritySource> for PluginConfigurationAuthorityResponse {
    fn from(source: PluginConfigurationAuthoritySource) -> Self {
        Self {
            kind: source.kind().to_owned(),
            publication_history: false,
            reference: source.reference().to_owned(),
            rollback_proposals: false,
        }
    }
}

impl PluginConfigurationAuthorityResponse {
    pub(super) fn with_history(mut self, available: bool) -> Self {
        self.publication_history = available;
        self.rollback_proposals = available;
        self
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ActivePluginSelection {
    generation_spec_digest: String,
    plan_digest: String,
    plugin_root_revision: String,
    plugins: Vec<PluginSelectionItem>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DesiredPluginSelection {
    desired_state_digest: String,
    plan_digest: String,
    plugin_root_revision: String,
    plugins: Vec<PluginSelectionItem>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PreparingPluginSelection {
    desired_state_digest: String,
    generation_spec_digest: String,
    plan_digest: String,
    plugin_root_revision: String,
    plugins: Vec<PluginSelectionItem>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginSelectionItem {
    disableable: bool,
    entrypoint: String,
    execution_class: String,
    instance_key: String,
    package_id: String,
    package_revision: String,
    provided_capabilities: Vec<String>,
    required_capabilities: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PluginGenerationEvent {
    cursor: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    desired_state_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_spec_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    plan_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    plugin_root_revision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_generation_spec_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    restored_generation_spec_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    routing_epoch: Option<String>,
    status: &'static str,
}

impl PluginGenerationEvent {
    #[cfg(test)]
    pub(super) const fn status(&self) -> &'static str {
        self.status
    }
}

impl PluginInventoryResponse {
    fn new(
        snapshot: &OnlineGenerationSnapshot,
        page: &OnlineGenerationEventPage,
        disableable: &BTreeSet<String>,
        accepted_desired: Option<&AcceptedDesiredState>,
        rejected_desired: Option<&RejectedDesiredState>,
        configuration_authority: Option<PluginConfigurationAuthorityResponse>,
        stream_id: &str,
    ) -> Self {
        let snapshot_desired_identity = PluginDesiredIdentity::from_selection(snapshot.desired());
        let accepted_desired = accepted_desired.filter(|accepted| {
            accepted_desired_overlay_is_current(
                snapshot.desired_epoch(),
                &snapshot_desired_identity,
                snapshot
                    .desired_rejection()
                    .map(OnlineGenerationRejectionObservation::cursor),
                accepted,
            )
        });
        let applied_revision = snapshot.active().plugin_root_revision().to_owned();
        let active_identity = PluginDesiredIdentity::from_selection(snapshot.active());
        let desired_identity =
            accepted_desired.map_or(&snapshot_desired_identity, |accepted| &accepted.identity);
        let preparing_identity = snapshot
            .preparing()
            .map(PluginDesiredIdentity::from_selection)
            .filter(|preparing| preparing == desired_identity);
        let configuration_status = configuration_status(
            &active_identity,
            desired_identity,
            preparing_identity.as_ref(),
            accepted_desired.map(|accepted| &accepted.identity),
            rejected_desired,
        );
        let desired = accepted_desired.map_or_else(
            || desired_selection(snapshot.desired(), disableable),
            |accepted| accepted.selection.clone(),
        );
        Self {
            applied_revision: Some(applied_revision),
            active: ActivePluginSelection {
                generation_spec_digest: snapshot.active().generation_spec_digest().to_owned(),
                plan_digest: snapshot.active().plan_digest().to_owned(),
                plugin_root_revision: snapshot.active().plugin_root_revision().to_owned(),
                plugins: selection_items(snapshot.active().plan(), disableable),
            },
            configuration_authority,
            configuration_status,
            cursor: page.cursor().to_string(),
            desired_revision: Some(desired_identity.plugin_root_revision.clone()),
            desired,
            events: page
                .events()
                .iter()
                .map(|record| generation_event(record.cursor(), record.event()))
                .collect(),
            preparing: snapshot
                .preparing()
                .filter(|selection| {
                    &PluginDesiredIdentity::from_selection(selection) == desired_identity
                })
                .map(|selection| PreparingPluginSelection {
                    desired_state_digest: selection.desired_state_digest().to_owned(),
                    generation_spec_digest: selection.generation_spec_digest().to_owned(),
                    plan_digest: selection.plan_digest().to_owned(),
                    plugin_root_revision: selection.plugin_root_revision().to_owned(),
                    plugins: selection_items(selection.plan(), disableable),
                }),
            schema: "lenso.agent.plugin-inventory.v2",
            stream_id: stream_id.to_owned(),
            truncated: page.truncated(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RejectedDesiredState {
    identity: Option<PluginDesiredIdentity>,
    plugin_root_revision: String,
}

impl RejectedDesiredState {
    fn from_event(event: &OnlineGenerationEvent) -> Option<Self> {
        Some(Self {
            identity: PluginDesiredIdentity::from_event(event),
            plugin_root_revision: event.plugin_root_revision()?.to_owned(),
        })
    }

    fn matches(&self, desired: &PluginDesiredIdentity) -> bool {
        self.identity.as_ref().map_or(
            self.plugin_root_revision == desired.plugin_root_revision,
            |identity| identity == desired,
        )
    }

    fn from_identity(identity: PluginDesiredIdentity) -> Self {
        Self {
            plugin_root_revision: identity.plugin_root_revision.clone(),
            identity: Some(identity),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PluginDesiredIdentity {
    desired_state_digest: String,
    plan_digest: String,
    plugin_root_revision: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AcceptedDesiredState {
    identity: PluginDesiredIdentity,
    observe_after_cursor: u64,
    accepted_after_desired_epoch: u64,
    selection: DesiredPluginSelection,
}

fn rejection_is_newer_than_acceptance(
    rejected: &PluginDesiredIdentity,
    rejected_cursor: Option<u64>,
    accepted: Option<&AcceptedDesiredState>,
) -> bool {
    accepted.is_none_or(|accepted| {
        accepted.identity != *rejected
            || rejected_cursor.is_some_and(|cursor| cursor > accepted.observe_after_cursor)
    })
}

fn rejected_snapshot_is_newer(rejected_cursor: Option<u64>, observe_after_cursor: u64) -> bool {
    rejected_cursor.is_some_and(|cursor| cursor > observe_after_cursor)
}

fn accepted_desired_is_applied(
    active: &PluginDesiredIdentity,
    desired: &PluginDesiredIdentity,
    accepted: Option<&AcceptedDesiredState>,
) -> bool {
    active == desired && accepted.is_some_and(|accepted| &accepted.identity == desired)
}

fn accepted_desired_overlay_is_current(
    snapshot_desired_epoch: u64,
    snapshot_desired: &PluginDesiredIdentity,
    desired_rejection_cursor: Option<u64>,
    accepted: &AcceptedDesiredState,
) -> bool {
    snapshot_desired_epoch <= accepted.accepted_after_desired_epoch
        || (snapshot_desired == &accepted.identity
            && desired_rejection_cursor
                .is_none_or(|cursor| cursor <= accepted.observe_after_cursor))
}

impl PluginDesiredIdentity {
    fn from_selection(selection: &OnlineGenerationSelection) -> Self {
        Self {
            desired_state_digest: selection.desired_state_digest().to_owned(),
            plan_digest: selection.plan_digest().to_owned(),
            plugin_root_revision: selection.plugin_root_revision().to_owned(),
        }
    }

    fn from_event(event: &OnlineGenerationEvent) -> Option<Self> {
        Some(Self {
            desired_state_digest: event.desired_state_digest()?.to_owned(),
            plan_digest: event.plan_digest()?.to_owned(),
            plugin_root_revision: event.plugin_root_revision()?.to_owned(),
        })
    }
}

fn configuration_status(
    active: &PluginDesiredIdentity,
    desired: &PluginDesiredIdentity,
    preparing: Option<&PluginDesiredIdentity>,
    accepted: Option<&PluginDesiredIdentity>,
    rejected: Option<&RejectedDesiredState>,
) -> &'static str {
    if preparing == Some(desired) {
        "pending"
    } else if rejected.is_some_and(|rejected| rejected.identity.as_ref() == Some(desired)) {
        "rejected"
    } else if accepted == Some(desired) {
        "pending"
    } else if active == desired {
        "applied"
    } else if rejected.is_some_and(|rejected| rejected.matches(desired)) {
        "rejected"
    } else {
        "pending"
    }
}

pub(super) fn desired_selection(
    selection: &OnlineGenerationSelection,
    disableable: &BTreeSet<String>,
) -> DesiredPluginSelection {
    DesiredPluginSelection {
        desired_state_digest: selection.desired_state_digest().to_owned(),
        plan_digest: selection.plan_digest().to_owned(),
        plugin_root_revision: selection.plugin_root_revision().to_owned(),
        plugins: selection_items(selection.plan(), disableable),
    }
}

pub(super) fn desired_plan_selection(
    plan: &ResolvedAppPlan,
    plugin_root_revision: String,
    desired_state_digest: String,
    plan_digest: String,
    disableable: &BTreeSet<String>,
) -> DesiredPluginSelection {
    DesiredPluginSelection {
        desired_state_digest,
        plan_digest,
        plugin_root_revision,
        plugins: selection_items(plan, disableable),
    }
}

fn selection_items(
    plan: &ResolvedAppPlan,
    disableable: &BTreeSet<String>,
) -> Vec<PluginSelectionItem> {
    let mut plugins = plan
        .plugin_instances()
        .iter()
        .map(|plugin| PluginSelectionItem {
            disableable: disableable.contains(plugin.instance_key()),
            entrypoint: plugin.entrypoint().to_owned(),
            execution_class: plugin.execution_class().as_str().to_owned(),
            instance_key: plugin.instance_key().to_owned(),
            package_id: plugin.package_id().to_owned(),
            package_revision: plugin.package_revision().to_owned(),
            provided_capabilities: plugin
                .provided_capabilities()
                .iter()
                .map(|capability| capability.capability_id().to_owned())
                .collect(),
            required_capabilities: plugin
                .required_capabilities()
                .iter()
                .map(|capability| capability.capability_id().to_owned())
                .collect(),
        })
        .collect::<Vec<_>>();
    plugins.sort_by(|left, right| left.instance_key.cmp(&right.instance_key));
    plugins
}

#[cfg(test)]
pub(super) fn fixture_desired_selection() -> DesiredPluginSelection {
    DesiredPluginSelection {
        desired_state_digest: "sha256:desired-next".to_owned(),
        plan_digest: "sha256:plan-next".to_owned(),
        plugin_root_revision: "sha256:root-next".to_owned(),
        plugins: vec![PluginSelectionItem {
            disableable: true,
            entrypoint: "native".to_owned(),
            execution_class: "lenso.native-rust@1".to_owned(),
            instance_key: "example.echo/default".to_owned(),
            package_id: "example.echo".to_owned(),
            package_revision: "1.0.0".to_owned(),
            provided_capabilities: vec!["example.echo@1".to_owned()],
            required_capabilities: Vec::new(),
        }],
    }
}

fn generation_event(cursor: u64, event: &OnlineGenerationEvent) -> PluginGenerationEvent {
    let mut projection = PluginGenerationEvent {
        cursor: cursor.to_string(),
        desired_state_digest: event.desired_state_digest().map(str::to_owned),
        detail: None,
        generation_spec_digest: None,
        plan_digest: event.plan_digest().map(str::to_owned),
        plugin_root_revision: event.plugin_root_revision().map(str::to_owned),
        previous_generation_spec_digest: None,
        restored_generation_spec_digest: None,
        routing_epoch: None,
        status: "watch_degraded",
    };
    match event {
        OnlineGenerationEvent::Preparing {
            generation_spec_digest,
            previous_generation_spec_digest,
            ..
        } => {
            projection.status = "preparing";
            projection.generation_spec_digest = Some(generation_spec_digest.clone());
            projection.previous_generation_spec_digest =
                Some(previous_generation_spec_digest.clone());
        }
        OnlineGenerationEvent::Switched {
            generation_spec_digest,
            previous_generation_spec_digest,
            routing_epoch,
            ..
        } => {
            projection.status = "switched";
            projection.generation_spec_digest = Some(generation_spec_digest.clone());
            projection.previous_generation_spec_digest =
                Some(previous_generation_spec_digest.clone());
            projection.routing_epoch = Some(routing_epoch.to_string());
        }
        OnlineGenerationEvent::Rejected { detail, .. } => {
            projection.status = "rejected";
            projection.detail = Some(detail.clone());
        }
        OnlineGenerationEvent::RolledBack {
            failed_generation_spec_digest,
            restored_generation_spec_digest,
            routing_epoch,
            detail,
        } => {
            projection.status = "rolled_back";
            projection.generation_spec_digest = Some(failed_generation_spec_digest.clone());
            projection.restored_generation_spec_digest =
                Some(restored_generation_spec_digest.clone());
            projection.routing_epoch = Some(routing_epoch.to_string());
            projection.detail = Some(detail.clone());
        }
        OnlineGenerationEvent::Failed {
            generation_spec_digest,
            detail,
        } => {
            projection.status = "rejected";
            projection.generation_spec_digest = Some(generation_spec_digest.clone());
            projection.detail = Some(detail.clone());
        }
        OnlineGenerationEvent::WatchDegraded { detail } => {
            projection.detail = Some(detail.clone());
        }
    }
    projection
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PluginMutationResponse {
    pub(super) desired: Option<DesiredPluginSelection>,
    pub(super) operation: PluginOperation,
    pub(super) schema: &'static str,
    pub(super) stream_id: String,
}

impl PluginMutationResponse {
    pub(super) fn new(
        operation: PluginOperation,
        desired: Option<DesiredPluginSelection>,
        stream_id: &str,
    ) -> Self {
        Self {
            desired,
            operation,
            schema: "lenso.agent.plugin-operation.v1",
            stream_id: stream_id.to_owned(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PluginOperationResponse {
    pub(super) operation: PluginOperation,
    pub(super) schema: &'static str,
    pub(super) stream_id: String,
}

#[derive(Debug)]
pub(super) enum PluginRuntimeCommand {
    Inventory {
        after: Option<u64>,
        reply: oneshot::Sender<Result<PluginInventoryResponse, String>>,
    },
    ObservationFence {
        reply: oneshot::Sender<PluginObservationFence>,
    },
    ValidateStream {
        expected_stream_id: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Operation {
        operation_id: String,
        reply: oneshot::Sender<Option<PluginOperationResponse>>,
    },
    RegisterMutation {
        accepted_after_cursor: u64,
        observe_after_cursor: u64,
        accepted_after_desired_epoch: u64,
        desired: Box<Result<lenso_agent_host::DesiredPluginRootSnapshot, String>>,
        reply: oneshot::Sender<Result<PluginMutationResponse, String>>,
    },
}

#[derive(Clone, Copy, Debug)]
pub(super) struct PluginObservationFence {
    pub(super) cursor: u64,
    pub(super) desired_epoch: u64,
}

#[derive(Clone, Debug)]
pub(super) struct PluginOperation {
    accepted_after_cursor: u64,
    accepted_after_desired_epoch: u64,
    cursor: u64,
    detail: Option<String>,
    generation_spec_digest: Option<String>,
    pub(super) id: String,
    identity: Option<PluginDesiredIdentity>,
    rollback_observable: bool,
    status: PluginOperationStatus,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PluginOperationWire<'a> {
    accepted_after_cursor: String,
    cursor: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    desired_state_digest: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_spec_digest: Option<&'a str>,
    id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    plan_digest: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    plugin_root_revision: Option<&'a str>,
    status: PluginOperationStatus,
}

impl Serialize for PluginOperation {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        PluginOperationWire {
            accepted_after_cursor: self.accepted_after_cursor.to_string(),
            cursor: self.cursor.to_string(),
            desired_state_digest: self
                .identity
                .as_ref()
                .map(|identity| identity.desired_state_digest.as_str()),
            detail: self.detail.as_deref(),
            generation_spec_digest: self.generation_spec_digest.as_deref(),
            id: &self.id,
            plan_digest: self
                .identity
                .as_ref()
                .map(|identity| identity.plan_digest.as_str()),
            plugin_root_revision: self
                .identity
                .as_ref()
                .map(|identity| identity.plugin_root_revision.as_str()),
            status: self.status,
        }
        .serialize(serializer)
    }
}

impl PluginOperation {
    pub(super) const fn configuration_status(&self) -> &'static str {
        match self.status {
            PluginOperationStatus::Switched => "applied",
            PluginOperationStatus::Rejected | PluginOperationStatus::RolledBack => "rejected",
            PluginOperationStatus::Accepted | PluginOperationStatus::Preparing => "pending",
        }
    }

    pub(super) const fn is_rejected(&self) -> bool {
        matches!(
            self.status,
            PluginOperationStatus::Rejected | PluginOperationStatus::RolledBack
        )
    }

    #[cfg(test)]
    pub(super) fn fixture_switched(id: &str, cursor: u64) -> Self {
        Self::fixture_with_status(id, cursor, PluginOperationStatus::Switched)
    }

    #[cfg(test)]
    pub(super) fn fixture_accepted(id: &str, cursor: u64) -> Self {
        Self::fixture_with_status(id, cursor, PluginOperationStatus::Accepted)
    }

    #[cfg(test)]
    fn fixture_with_status(id: &str, cursor: u64, status: PluginOperationStatus) -> Self {
        Self {
            accepted_after_cursor: cursor,
            accepted_after_desired_epoch: 0,
            cursor,
            detail: None,
            generation_spec_digest: Some("sha256:generation-next".to_owned()),
            id: id.to_owned(),
            identity: Some(PluginDesiredIdentity {
                desired_state_digest: "sha256:desired-next".to_owned(),
                plan_digest: "sha256:plan-next".to_owned(),
                plugin_root_revision: "sha256:root-next".to_owned(),
            }),
            rollback_observable: true,
            status,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum PluginOperationStatus {
    Accepted,
    Preparing,
    Switched,
    Rejected,
    RolledBack,
}

impl PluginOperationStatus {
    const fn is_terminal(self) -> bool {
        matches!(self, Self::Rejected | Self::RolledBack)
    }
}

#[derive(Debug, Default)]
pub(super) struct PluginOperationLedger {
    operations: BTreeMap<String, PluginOperation>,
    order: VecDeque<String>,
}

#[derive(Debug)]
pub(super) struct PluginRuntimeState {
    accepted_desired: Option<AcceptedDesiredState>,
    configuration_authority: Option<PluginConfigurationAuthorityResponse>,
    disableable: BTreeSet<String>,
    disableable_error: Option<String>,
    operations: PluginOperationLedger,
    processed_rejection_cursor: u64,
    processed_selection_cursor: u64,
    rejected_desired: Option<RejectedDesiredState>,
    stream_id: String,
}

impl PluginRuntimeState {
    pub(super) fn new(
        app: &lenso_agent_host::generation::AgentApp,
        configuration_authority: Option<PluginConfigurationAuthorityResponse>,
    ) -> Self {
        let mut state = Self {
            accepted_desired: None,
            configuration_authority,
            disableable: BTreeSet::new(),
            disableable_error: None,
            operations: PluginOperationLedger::default(),
            processed_rejection_cursor: 0,
            processed_selection_cursor: 0,
            rejected_desired: None,
            stream_id: uuid::Uuid::new_v4().to_string(),
        };
        if let Err(error) = state.refresh_disableable(app) {
            state.disableable_error = Some(error);
        }
        state
    }

    pub(super) fn dispatch(
        &mut self,
        app: &lenso_agent_host::generation::AgentApp,
        command: PluginRuntimeCommand,
    ) {
        match command {
            PluginRuntimeCommand::Inventory { after, reply } => {
                let _ = reply.send(self.inventory(app, after));
            }
            PluginRuntimeCommand::ObservationFence { reply } => {
                let _ = reply.send(PluginObservationFence {
                    cursor: app.online_generation_events(None).cursor(),
                    desired_epoch: app.online_generation_snapshot().desired_epoch(),
                });
            }
            PluginRuntimeCommand::ValidateStream {
                expected_stream_id,
                reply,
            } => {
                let result = if expected_stream_id == self.stream_id {
                    Ok(())
                } else {
                    Err(format!(
                        "Plugin control stream conflict: expected {expected_stream_id}, current {}",
                        self.stream_id
                    ))
                };
                let _ = reply.send(result);
            }
            PluginRuntimeCommand::Operation {
                operation_id,
                reply,
            } => {
                self.refresh_operations(app);
                let operation =
                    self.operations
                        .get(&operation_id)
                        .map(|operation| PluginOperationResponse {
                            operation,
                            schema: "lenso.agent.plugin-operation.v1",
                            stream_id: self.stream_id.clone(),
                        });
                let _ = reply.send(operation);
            }
            PluginRuntimeCommand::RegisterMutation {
                accepted_after_cursor,
                observe_after_cursor,
                accepted_after_desired_epoch,
                desired,
                reply,
            } => {
                let response = self.register_mutation(
                    app,
                    accepted_after_cursor,
                    observe_after_cursor,
                    accepted_after_desired_epoch,
                    *desired,
                );
                let _ = reply.send(Ok(response));
            }
        }
    }

    fn register_mutation(
        &mut self,
        app: &lenso_agent_host::generation::AgentApp,
        accepted_after_cursor: u64,
        observe_after_cursor: u64,
        accepted_after_desired_epoch: u64,
        desired: Result<lenso_agent_host::DesiredPluginRootSnapshot, String>,
    ) -> PluginMutationResponse {
        let accepted_identity = desired.as_ref().ok().map(|desired| PluginDesiredIdentity {
            desired_state_digest: desired.desired_state_digest().to_owned(),
            plan_digest: desired.plan_digest().to_owned(),
            plugin_root_revision: desired.plugin_root_revision().to_owned(),
        });
        // Observe everything already published by the Host before a newer
        // mutation is allowed to supersede an older pending receipt.
        self.refresh_operations(app);
        let operation = self.operations.accept_observing_from(
            accepted_after_cursor,
            observe_after_cursor,
            accepted_after_desired_epoch,
            desired
                .as_ref()
                .map(|desired| {
                    (
                        desired.plugin_root_revision().to_owned(),
                        desired.desired_state_digest().to_owned(),
                        desired.plan_digest().to_owned(),
                    )
                })
                .map_err(Clone::clone),
        );
        let desired = desired.ok().map(|desired| {
            self.disableable = desired
                .disableable_instance_keys()
                .iter()
                .cloned()
                .collect();
            self.disableable_error = None;
            desired_plan_selection(
                desired.plan(),
                desired.plugin_root_revision().to_owned(),
                desired.desired_state_digest().to_owned(),
                desired.plan_digest().to_owned(),
                &self.disableable,
            )
        });
        if let (Some(identity), Some(selection)) = (accepted_identity.clone(), desired.as_ref()) {
            self.observe_accepted_desired(
                identity,
                selection.clone(),
                observe_after_cursor,
                accepted_after_desired_epoch,
            );
        }
        self.refresh_operations(app);
        let operation = self.operations.get(&operation.id).unwrap_or(operation);
        if operation.is_rejected()
            && let Some(identity) = accepted_identity.clone()
        {
            self.accepted_desired = None;
            self.rejected_desired = Some(RejectedDesiredState::from_identity(identity));
        } else if accepted_identity.is_some()
            && let Err(detail) = app.reopen_plugin_reconciliation()
        {
            app.report_plugin_watch_degraded(format!(
                "committed Plugin mutation could not reopen reconciliation: {detail}"
            ));
        }
        PluginMutationResponse::new(operation, desired, &self.stream_id)
    }

    fn inventory(
        &mut self,
        app: &lenso_agent_host::generation::AgentApp,
        after: Option<u64>,
    ) -> Result<PluginInventoryResponse, String> {
        let page = app.online_generation_events(after);
        let selection_page = app.online_generation_events(Some(self.processed_selection_cursor));
        let selection_changed = selection_page.truncated()
            || selection_page.events().iter().any(|record| {
                matches!(
                    record.event(),
                    OnlineGenerationEvent::Preparing { .. }
                        | OnlineGenerationEvent::Switched { .. }
                        | OnlineGenerationEvent::Rejected { .. }
                        | OnlineGenerationEvent::RolledBack { .. }
                        | OnlineGenerationEvent::Failed { .. }
                )
            });
        self.processed_selection_cursor = selection_page.cursor();
        if selection_changed || self.disableable_error.is_some() {
            self.refresh_disableable(app)?;
        }
        let snapshot = app.online_generation_snapshot();
        // Operation cursors are independent from the inventory consumer's cursor.
        // Always refresh the ledger from the complete retained Host event window.
        self.refresh_operations(app);
        self.refresh_rejected_identity(app, &snapshot);
        Ok(PluginInventoryResponse::new(
            &snapshot,
            &page,
            &self.disableable,
            self.accepted_desired.as_ref(),
            self.rejected_desired.as_ref(),
            self.configuration_authority.clone(),
            &self.stream_id,
        ))
    }

    fn refresh_disableable(
        &mut self,
        app: &lenso_agent_host::generation::AgentApp,
    ) -> Result<(), String> {
        match app.disableable_plugin_instances() {
            Ok(instances) => {
                self.disableable = instances
                    .into_iter()
                    .map(|instance| instance.to_string())
                    .collect();
                self.disableable_error = None;
                Ok(())
            }
            Err(error) => {
                self.disableable_error = Some(error.clone());
                Err(error)
            }
        }
    }

    fn refresh_operations(&mut self, app: &lenso_agent_host::generation::AgentApp) {
        let page = app.online_generation_events(None);
        let snapshot = app.online_generation_snapshot();
        self.refresh_operations_with(&page, &snapshot);
    }

    fn refresh_rejected_identity(
        &mut self,
        app: &lenso_agent_host::generation::AgentApp,
        snapshot: &OnlineGenerationSnapshot,
    ) {
        let page = app.online_generation_events(Some(self.processed_rejection_cursor));
        if page.truncated()
            && let Some(rejected) = snapshot.rejected()
        {
            let identity = PluginDesiredIdentity::from_selection(rejected);
            if rejection_is_newer_than_acceptance(
                &identity,
                snapshot.rejected_cursor(),
                self.accepted_desired.as_ref(),
            ) {
                self.rejected_desired = Some(RejectedDesiredState::from_identity(identity));
                self.accepted_desired = None;
            }
        }
        refresh_rejected_desired(
            &mut self.rejected_desired,
            &self.operations,
            page.events()
                .iter()
                .map(lenso_agent_host::generation::OnlineGenerationEventRecord::event),
        );
        self.processed_rejection_cursor = page.cursor();

        let active = PluginDesiredIdentity::from_selection(snapshot.active());
        let desired = PluginDesiredIdentity::from_selection(snapshot.desired());
        let preparing_desired = snapshot
            .preparing()
            .map(PluginDesiredIdentity::from_selection)
            .is_some_and(|preparing| preparing == desired);
        let snapshot_rejection = snapshot
            .rejected()
            .map(PluginDesiredIdentity::from_selection)
            .filter(|rejected| rejected == &desired);
        let exact_rejection = snapshot_rejection.clone().filter(|rejected| {
            rejection_is_newer_than_acceptance(
                rejected,
                snapshot.rejected_cursor(),
                self.accepted_desired.as_ref(),
            )
        });
        let old_exact_rejection_blocks_active = snapshot_rejection.is_some()
            && exact_rejection.is_none()
            && self
                .accepted_desired
                .as_ref()
                .is_some_and(|accepted| accepted.identity == desired);
        if preparing_desired {
            self.rejected_desired = None;
        } else if let Some(rejected) = exact_rejection {
            self.rejected_desired = Some(RejectedDesiredState::from_identity(rejected));
            self.accepted_desired = None;
        } else if active == desired {
            self.rejected_desired = None;
            if !old_exact_rejection_blocks_active
                && accepted_desired_is_applied(&active, &desired, self.accepted_desired.as_ref())
            {
                self.accepted_desired = None;
            }
        } else if self.accepted_desired.as_ref().is_some_and(|accepted| {
            self.rejected_desired
                .as_ref()
                .is_some_and(|rejected| rejected.matches(&accepted.identity))
        }) {
            self.accepted_desired = None;
        }
        if self.accepted_desired.as_ref().is_some_and(|accepted| {
            !accepted_desired_overlay_is_current(
                snapshot.desired_epoch(),
                &desired,
                snapshot
                    .desired_rejection()
                    .map(OnlineGenerationRejectionObservation::cursor),
                accepted,
            )
        }) {
            // The Host has observed a later Desired selection. Its snapshot is
            // now authoritative even when no lifecycle event carries a
            // complete identity. A same-identity retry remains overlaid until
            // it prepares, switches, or receives a newer rejection.
            self.accepted_desired = None;
        }
    }

    fn refresh_operations_with(
        &mut self,
        page: &OnlineGenerationEventPage,
        snapshot: &OnlineGenerationSnapshot,
    ) {
        self.operations.refresh(page, snapshot);
    }

    fn observe_accepted_desired(
        &mut self,
        identity: PluginDesiredIdentity,
        selection: DesiredPluginSelection,
        cursor: u64,
        accepted_after_desired_epoch: u64,
    ) {
        // This accepted full identity is newer than every terminal observation
        // at its fence cursor. Do not let a retained same-root partial
        // rejection color the new attempt.
        self.accepted_desired = Some(AcceptedDesiredState {
            identity,
            observe_after_cursor: cursor,
            accepted_after_desired_epoch,
            selection,
        });
        self.rejected_desired = None;
        self.processed_rejection_cursor = self.processed_rejection_cursor.max(cursor);
    }
}

impl PluginOperationLedger {
    #[cfg(test)]
    pub(super) fn accept(
        &mut self,
        accepted_after_cursor: u64,
        identity: Result<(String, String, String), String>,
    ) -> PluginOperation {
        self.accept_observing_from(accepted_after_cursor, accepted_after_cursor, 0, identity)
    }

    pub(super) fn accept_observing_from(
        &mut self,
        accepted_after_cursor: u64,
        observe_after_cursor: u64,
        accepted_after_desired_epoch: u64,
        identity: Result<(String, String, String), String>,
    ) -> PluginOperation {
        debug_assert!(observe_after_cursor <= accepted_after_cursor);
        if let Ok((plugin_root_revision, desired_state_digest, plan_digest)) = identity.as_ref() {
            self.supersede_pending(
                accepted_after_cursor,
                (plugin_root_revision, desired_state_digest, plan_digest),
            );
        }
        let (identity, status, detail) = match identity {
            Ok((plugin_root_revision, desired_state_digest, plan_digest)) => (
                Some(PluginDesiredIdentity {
                    desired_state_digest,
                    plan_digest,
                    plugin_root_revision,
                }),
                PluginOperationStatus::Accepted,
                None,
            ),
            Err(detail) => (None, PluginOperationStatus::Rejected, Some(detail)),
        };
        let operation = PluginOperation {
            accepted_after_cursor,
            accepted_after_desired_epoch,
            cursor: observe_after_cursor,
            detail,
            generation_spec_digest: None,
            id: uuid::Uuid::new_v4().to_string(),
            identity,
            rollback_observable: true,
            status,
        };
        if self.order.len() == MAX_PLUGIN_OPERATIONS
            && let Some(expired) = self.order.pop_front()
        {
            self.operations.remove(&expired);
        }
        self.order.push_back(operation.id.clone());
        self.operations
            .insert(operation.id.clone(), operation.clone());
        operation
    }

    pub(super) fn refresh(
        &mut self,
        page: &OnlineGenerationEventPage,
        snapshot: &OnlineGenerationSnapshot,
    ) {
        if page.truncated()
            && let Some(oldest_cursor) = page
                .events()
                .first()
                .map(lenso_agent_host::generation::OnlineGenerationEventRecord::cursor)
        {
            for operation in self.operations.values_mut() {
                let observed_cursor = operation.cursor;
                if observed_cursor.saturating_add(1) < oldest_cursor
                    && settle_operation_from_snapshot(operation, observed_cursor, page, snapshot)
                {
                    operation.cursor = page.cursor();
                }
            }
            self.expire_before(oldest_cursor);
        }
        for operation in self.operations.values_mut() {
            if operation.status.is_terminal() {
                continue;
            }
            let observed_cursor = operation.cursor;
            for record in page
                .events()
                .iter()
                .filter(|record| record.cursor() > observed_cursor)
            {
                apply_operation_event(operation, record.cursor(), record.event());
            }
            operation.cursor = page.cursor();
            settle_operation_from_snapshot(operation, observed_cursor, page, snapshot);
            let desired = PluginDesiredIdentity::from_selection(snapshot.desired());
            if later_desired_selection_supersedes_operation(
                operation,
                snapshot.desired_epoch(),
                &desired,
            ) {
                operation.status = PluginOperationStatus::Rejected;
                operation.detail = Some("superseded by a newer Plugin Root mutation".to_owned());
            }
        }
    }

    pub(super) fn get(&self, id: &str) -> Option<PluginOperation> {
        self.operations.get(id).cloned()
    }

    fn desired_identity_for_generation(
        &self,
        generation_spec_digest: &str,
    ) -> Option<PluginDesiredIdentity> {
        self.order.iter().rev().find_map(|id| {
            let operation = self.operations.get(id)?;
            if operation.generation_spec_digest.as_deref() != Some(generation_spec_digest) {
                return None;
            }
            operation.identity.clone()
        })
    }

    fn expire_before(&mut self, oldest_cursor: u64) {
        let expired = self
            .operations
            .iter()
            .filter_map(|(id, operation)| {
                let cursor = operation.cursor;
                (cursor.saturating_add(1) < oldest_cursor).then(|| id.clone())
            })
            .collect::<BTreeSet<_>>();
        self.order.retain(|id| !expired.contains(id));
        for id in expired {
            self.operations.remove(&id);
        }
    }

    fn supersede_pending(&mut self, cursor: u64, newer: (&str, &str, &str)) {
        for operation in self.operations.values_mut().filter(|operation| {
            matches!(
                operation.status,
                PluginOperationStatus::Accepted | PluginOperationStatus::Preparing
            ) && !operation_identity_matches(operation, newer.0, newer.1, newer.2)
        }) {
            operation.status = PluginOperationStatus::Rejected;
            operation.detail = Some("superseded by a newer Plugin Root mutation".to_owned());
            operation.cursor = operation.cursor.max(cursor);
        }
    }
}

fn later_desired_selection_supersedes_operation(
    operation: &PluginOperation,
    desired_epoch: u64,
    desired: &PluginDesiredIdentity,
) -> bool {
    matches!(
        operation.status,
        PluginOperationStatus::Accepted | PluginOperationStatus::Preparing
    ) && desired_epoch > operation.accepted_after_desired_epoch
        && operation.identity.as_ref() != Some(desired)
}

fn refresh_rejected_desired<'a>(
    rejected: &mut Option<RejectedDesiredState>,
    operations: &PluginOperationLedger,
    events: impl IntoIterator<Item = &'a OnlineGenerationEvent>,
) {
    let mut generation_identities = BTreeMap::<String, PluginDesiredIdentity>::new();
    for event in events {
        match event {
            OnlineGenerationEvent::Preparing {
                generation_spec_digest,
                ..
            }
            | OnlineGenerationEvent::Switched {
                generation_spec_digest,
                ..
            } => {
                if let Some(identity) = PluginDesiredIdentity::from_event(event) {
                    generation_identities.insert(generation_spec_digest.clone(), identity);
                }
                *rejected = None;
            }
            event @ OnlineGenerationEvent::Rejected { .. } => {
                if let Some(identity) = RejectedDesiredState::from_event(event) {
                    *rejected = Some(identity);
                }
            }
            OnlineGenerationEvent::RolledBack {
                failed_generation_spec_digest,
                ..
            } => {
                let identity = generation_identities
                    .get(failed_generation_spec_digest)
                    .cloned()
                    .or_else(|| {
                        operations.desired_identity_for_generation(failed_generation_spec_digest)
                    });
                if let Some(identity) = identity {
                    *rejected = Some(RejectedDesiredState::from_identity(identity));
                }
            }
            OnlineGenerationEvent::Failed {
                generation_spec_digest,
                ..
            } => {
                let identity = generation_identities
                    .get(generation_spec_digest)
                    .cloned()
                    .or_else(|| operations.desired_identity_for_generation(generation_spec_digest));
                if let Some(identity) = identity {
                    *rejected = Some(RejectedDesiredState::from_identity(identity));
                }
            }
            OnlineGenerationEvent::WatchDegraded { .. } => {}
        }
    }
}

fn active_selection_matches(
    operation: &PluginOperation,
    snapshot: &OnlineGenerationSnapshot,
) -> bool {
    operation_identity_matches(
        operation,
        snapshot.active().plugin_root_revision(),
        snapshot.active().desired_state_digest(),
        snapshot.active().plan_digest(),
    )
}

fn settle_operation_from_snapshot(
    operation: &mut PluginOperation,
    observe_after_cursor: u64,
    page: &OnlineGenerationEventPage,
    snapshot: &OnlineGenerationSnapshot,
) -> bool {
    if operation.status.is_terminal() {
        return true;
    }
    if let Some(preparing) = snapshot
        .preparing()
        .filter(|selection| selection_matches_operation(selection, operation))
    {
        operation.status = PluginOperationStatus::Preparing;
        operation.generation_spec_digest = Some(preparing.generation_spec_digest().to_owned());
        return true;
    }
    let exact_rejected = snapshot
        .rejected()
        .filter(|selection| selection_matches_operation(selection, operation));
    match rejection_snapshot_settlement(
        snapshot
            .desired_rejection()
            .map(OnlineGenerationRejectionObservation::cursor),
        exact_rejected.is_some(),
        snapshot.rejected_cursor(),
        operation.accepted_after_cursor,
        observe_after_cursor,
    ) {
        RejectionSnapshotSettlement::LatestDesired => {
            let observation = snapshot
                .desired_rejection()
                .expect("latest Desired rejection settlement has an observation");
            let detail = desired_rejection_detail_for_operation(observation.event(), operation)
                .expect("Desired rejection observation contains a Rejected event");
            operation.status = PluginOperationStatus::Rejected;
            operation.detail = Some(detail);
            if let Some(rejected) = exact_rejected {
                operation.generation_spec_digest =
                    Some(rejected.generation_spec_digest().to_owned());
            }
            return true;
        }
        RejectionSnapshotSettlement::Exact => {
            let rejected = exact_rejected.expect("exact rejection settlement has a selection");
            let terminal = page.events().iter().rev().find_map(|record| {
                terminal_outcome_for_rejected_selection(record.event(), rejected)
            });
            settle_operation_from_rejected_snapshot(
                operation,
                rejected.generation_spec_digest(),
                terminal,
            );
            return true;
        }
        RejectionSnapshotSettlement::StaleExact => {
            // This exact rejection predates the retry. It blocks the stale
            // active fallback without terminalizing the new receipt.
            return true;
        }
        RejectionSnapshotSettlement::None => {}
    }
    if active_selection_matches(operation, snapshot) {
        operation.status = PluginOperationStatus::Switched;
        operation.identity = Some(PluginDesiredIdentity::from_selection(snapshot.active()));
        operation.generation_spec_digest =
            Some(snapshot.active().generation_spec_digest().to_owned());
        return true;
    }
    false
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RejectionSnapshotSettlement {
    LatestDesired,
    Exact,
    StaleExact,
    None,
}

fn rejection_snapshot_settlement(
    latest_desired_rejection_cursor: Option<u64>,
    exact_rejection_matches: bool,
    exact_rejection_cursor: Option<u64>,
    accepted_after_cursor: u64,
    observe_after_cursor: u64,
) -> RejectionSnapshotSettlement {
    if latest_desired_rejection_cursor.is_some_and(|cursor| cursor > accepted_after_cursor) {
        RejectionSnapshotSettlement::LatestDesired
    } else if exact_rejection_matches
        && rejected_snapshot_is_newer(exact_rejection_cursor, observe_after_cursor)
    {
        RejectionSnapshotSettlement::Exact
    } else if exact_rejection_matches {
        RejectionSnapshotSettlement::StaleExact
    } else {
        RejectionSnapshotSettlement::None
    }
}

fn desired_rejection_detail_for_operation(
    event: &OnlineGenerationEvent,
    operation: &PluginOperation,
) -> Option<String> {
    let OnlineGenerationEvent::Rejected { detail, .. } = event else {
        return None;
    };
    let rejected_identity = PluginDesiredIdentity::from_event(event);
    Some(
        if rejected_identity
            .as_ref()
            .is_some_and(|identity| operation.identity.as_ref() != Some(identity))
        {
            "superseded by a newer Plugin Root mutation".to_owned()
        } else {
            detail.clone()
        },
    )
}

fn settle_operation_from_rejected_snapshot(
    operation: &mut PluginOperation,
    generation_spec_digest: &str,
    terminal: Option<(PluginOperationStatus, String)>,
) {
    let (status, detail) = terminal.unwrap_or((
        PluginOperationStatus::Rejected,
        "App Generation reached a terminal rejection before its operation receipt was registered"
            .to_owned(),
    ));
    operation.status = status;
    operation.detail = Some(detail);
    operation.generation_spec_digest = Some(generation_spec_digest.to_owned());
}

fn selection_matches_operation(
    selection: &OnlineGenerationSelection,
    operation: &PluginOperation,
) -> bool {
    operation_identity_matches(
        operation,
        selection.plugin_root_revision(),
        selection.desired_state_digest(),
        selection.plan_digest(),
    )
}

fn terminal_outcome_for_rejected_selection(
    event: &OnlineGenerationEvent,
    rejected: &OnlineGenerationSelection,
) -> Option<(PluginOperationStatus, String)> {
    match event {
        OnlineGenerationEvent::Rejected { detail, .. }
            if PluginDesiredIdentity::from_event(event).as_ref()
                == Some(&PluginDesiredIdentity::from_selection(rejected)) =>
        {
            Some((PluginOperationStatus::Rejected, detail.clone()))
        }
        OnlineGenerationEvent::RolledBack {
            failed_generation_spec_digest,
            detail,
            ..
        } if failed_generation_spec_digest == rejected.generation_spec_digest() => {
            Some((PluginOperationStatus::RolledBack, detail.clone()))
        }
        OnlineGenerationEvent::Failed {
            generation_spec_digest,
            detail,
        } if generation_spec_digest == rejected.generation_spec_digest() => {
            Some((PluginOperationStatus::Rejected, detail.clone()))
        }
        _ => None,
    }
}

fn operation_identity_matches(
    operation: &PluginOperation,
    plugin_root_revision: &str,
    desired_state_digest: &str,
    plan_digest: &str,
) -> bool {
    operation.identity.as_ref().is_some_and(|identity| {
        identity.plugin_root_revision == plugin_root_revision
            && identity.desired_state_digest == desired_state_digest
            && identity.plan_digest == plan_digest
    })
}

fn apply_operation_event(
    operation: &mut PluginOperation,
    cursor: u64,
    event: &OnlineGenerationEvent,
) {
    if operation.status.is_terminal() {
        return;
    }
    let event_identity = event
        .plugin_root_revision()
        .zip(event.desired_state_digest())
        .zip(event.plan_digest())
        .map(
            |((plugin_root_revision, desired_state_digest), plan_digest)| {
                (plugin_root_revision, desired_state_digest, plan_digest)
            },
        );
    let identity_matches = event_identity.is_some_and(
        |(plugin_root_revision, desired_state_digest, plan_digest)| {
            operation_identity_matches(
                operation,
                plugin_root_revision,
                desired_state_digest,
                plan_digest,
            )
        },
    );
    let pending = matches!(
        operation.status,
        PluginOperationStatus::Accepted | PluginOperationStatus::Preparing
    );
    let after_acceptance = cursor > operation.accepted_after_cursor;
    match event {
        OnlineGenerationEvent::Preparing {
            generation_spec_digest,
            ..
        } if identity_matches && pending => {
            operation.status = PluginOperationStatus::Preparing;
            operation.generation_spec_digest = Some(generation_spec_digest.clone());
        }
        OnlineGenerationEvent::Switched {
            generation_spec_digest,
            ..
        } if identity_matches && (pending || operation.rollback_observable) => {
            operation.status = PluginOperationStatus::Switched;
            operation.generation_spec_digest = Some(generation_spec_digest.clone());
            operation.rollback_observable = true;
        }
        OnlineGenerationEvent::Switched { .. }
            if event_identity.is_some()
                && !identity_matches
                && operation.status == PluginOperationStatus::Switched =>
        {
            // A later activation closes this receipt's rollback window. This
            // also disambiguates a future re-activation of the same identity.
            operation.rollback_observable = false;
        }
        OnlineGenerationEvent::Rejected { detail, .. } if identity_matches && pending => {
            operation.status = PluginOperationStatus::Rejected;
            operation.detail = Some(detail.clone());
        }
        event @ OnlineGenerationEvent::Rejected { .. }
            if event_identity.is_none() && pending && after_acceptance =>
        {
            operation.status = PluginOperationStatus::Rejected;
            operation.detail = desired_rejection_detail_for_operation(event, operation);
        }
        OnlineGenerationEvent::Preparing { .. }
        | OnlineGenerationEvent::Switched { .. }
        | OnlineGenerationEvent::Rejected { .. }
            if event_identity.is_some() && !identity_matches && pending && after_acceptance =>
        {
            operation.status = PluginOperationStatus::Rejected;
            operation.detail = Some("superseded by a newer Plugin Root mutation".to_owned());
        }
        OnlineGenerationEvent::RolledBack {
            failed_generation_spec_digest,
            detail,
            ..
        } if operation.generation_spec_digest.as_deref()
            == Some(failed_generation_spec_digest.as_str())
            && operation.rollback_observable =>
        {
            operation.status = PluginOperationStatus::RolledBack;
            operation.detail = Some(detail.clone());
        }
        OnlineGenerationEvent::Failed {
            generation_spec_digest,
            detail,
        } if operation.generation_spec_digest.as_deref()
            == Some(generation_spec_digest.as_str())
            && operation.rollback_observable =>
        {
            operation.status = PluginOperationStatus::Rejected;
            operation.detail = Some(detail.clone());
        }
        OnlineGenerationEvent::Preparing { .. }
        | OnlineGenerationEvent::Switched { .. }
        | OnlineGenerationEvent::Rejected { .. }
        | OnlineGenerationEvent::RolledBack { .. }
        | OnlineGenerationEvent::Failed { .. }
        | OnlineGenerationEvent::WatchDegraded { .. } => {}
    }
    operation.cursor = cursor;
}

#[cfg(test)]
mod tests {
    use super::*;
    use lenso_agent_host::generation::OnlineGenerationEvent;

    const FIXTURE_STREAM_ID: &str = "018f0f5f-8b8a-7c3e-9b34-7f7f8d3f6b20";

    fn fixture_plugin() -> PluginSelectionItem {
        PluginSelectionItem {
            disableable: true,
            entrypoint: "native".to_owned(),
            execution_class: "lenso.native-rust@1".to_owned(),
            instance_key: "example.echo/default".to_owned(),
            package_id: "example.echo".to_owned(),
            package_revision: "1.0.0".to_owned(),
            provided_capabilities: vec!["example.echo@1".to_owned()],
            required_capabilities: Vec::new(),
        }
    }

    fn fixture_desired(identity: &PluginDesiredIdentity) -> DesiredPluginSelection {
        DesiredPluginSelection {
            desired_state_digest: identity.desired_state_digest.clone(),
            plan_digest: identity.plan_digest.clone(),
            plugin_root_revision: identity.plugin_root_revision.clone(),
            plugins: vec![fixture_plugin()],
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)] // One golden keeps the complete cursor envelope adjacent.
    fn serialized_plugin_control_contract_matches_the_consumer_fixture() {
        let inventory = PluginInventoryResponse {
            applied_revision: Some("sha256:root-active".to_owned()),
            active: ActivePluginSelection {
                generation_spec_digest: "sha256:generation-active".to_owned(),
                plan_digest: "sha256:plan-active".to_owned(),
                plugin_root_revision: "sha256:root-active".to_owned(),
                plugins: vec![fixture_plugin()],
            },
            configuration_authority: Some(PluginConfigurationAuthorityResponse {
                kind: "local_plugin_root".to_owned(),
                publication_history: false,
                reference: "app".to_owned(),
                rollback_proposals: false,
            }),
            configuration_status: "pending",
            cursor: "9007199254740993".to_owned(),
            desired: DesiredPluginSelection {
                desired_state_digest: "sha256:desired-next".to_owned(),
                plan_digest: "sha256:plan-next".to_owned(),
                plugin_root_revision: "sha256:root-next".to_owned(),
                plugins: vec![fixture_plugin()],
            },
            desired_revision: Some("sha256:root-next".to_owned()),
            events: vec![
                PluginGenerationEvent {
                    cursor: "9007199254740992".to_owned(),
                    desired_state_digest: None,
                    detail: Some("older candidate failed after switching".to_owned()),
                    generation_spec_digest: Some("sha256:generation-old".to_owned()),
                    plan_digest: None,
                    plugin_root_revision: None,
                    previous_generation_spec_digest: None,
                    restored_generation_spec_digest: Some("sha256:generation-active".to_owned()),
                    routing_epoch: Some("9007199254740994".to_owned()),
                    status: "rolled_back",
                },
                PluginGenerationEvent {
                    cursor: "9007199254740993".to_owned(),
                    desired_state_digest: Some("sha256:desired-next".to_owned()),
                    detail: None,
                    generation_spec_digest: Some("sha256:generation-next".to_owned()),
                    plan_digest: Some("sha256:plan-next".to_owned()),
                    plugin_root_revision: Some("sha256:root-next".to_owned()),
                    previous_generation_spec_digest: Some("sha256:generation-active".to_owned()),
                    restored_generation_spec_digest: None,
                    routing_epoch: None,
                    status: "preparing",
                },
            ],
            preparing: Some(PreparingPluginSelection {
                desired_state_digest: "sha256:desired-next".to_owned(),
                generation_spec_digest: "sha256:generation-next".to_owned(),
                plan_digest: "sha256:plan-next".to_owned(),
                plugin_root_revision: "sha256:root-next".to_owned(),
                plugins: vec![fixture_plugin()],
            }),
            schema: "lenso.agent.plugin-inventory.v2",
            stream_id: FIXTURE_STREAM_ID.to_owned(),
            truncated: true,
        };
        let mutation = PluginMutationResponse::new(
            PluginOperation {
                accepted_after_cursor: 9_007_199_254_740_993,
                accepted_after_desired_epoch: 0,
                cursor: 9_007_199_254_740_993,
                detail: Some("selected Profile no longer resolves".to_owned()),
                generation_spec_digest: None,
                id: "018f0f5f-8b8a-7c3e-9b34-7f7f8d3f6b21".to_owned(),
                identity: None,
                rollback_observable: true,
                status: PluginOperationStatus::Rejected,
            },
            None,
            FIXTURE_STREAM_ID,
        );
        let operation = PluginOperationResponse {
            operation: PluginOperation {
                accepted_after_cursor: 9_007_199_254_740_991,
                accepted_after_desired_epoch: 0,
                cursor: 9_007_199_254_740_992,
                detail: None,
                generation_spec_digest: Some("sha256:generation-next".to_owned()),
                id: "018f0f5f-8b8a-7c3e-9b34-7f7f8d3f6b22".to_owned(),
                identity: Some(PluginDesiredIdentity {
                    desired_state_digest: "sha256:desired-next".to_owned(),
                    plan_digest: "sha256:plan-next".to_owned(),
                    plugin_root_revision: "sha256:root-next".to_owned(),
                }),
                rollback_observable: true,
                status: PluginOperationStatus::Preparing,
            },
            schema: "lenso.agent.plugin-operation.v1",
            stream_id: FIXTURE_STREAM_ID.to_owned(),
        };
        let actual = serde_json::json!({
            "inventory": inventory,
            "mutation": mutation,
            "operation": operation,
        });
        let expected: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/plugin-control-contract.json"
        ))
        .unwrap();

        for key in ["inventory", "mutation", "operation"] {
            assert_eq!(actual[key], expected[key], "contract fixture key {key}");
        }
    }

    #[test]
    fn operation_tracks_preparing_and_switch() {
        let mut operation = PluginOperationLedger::default().accept(
            0,
            Ok((
                "sha256:root".to_owned(),
                "sha256:desired".to_owned(),
                "sha256:plan".to_owned(),
            )),
        );
        apply_operation_event(
            &mut operation,
            1,
            &OnlineGenerationEvent::Preparing {
                plugin_root_revision: "sha256:root".to_owned(),
                resolution_authority_digest: "sha256:authority".to_owned(),
                desired_state_digest: "sha256:desired".to_owned(),
                plan_digest: "sha256:plan".to_owned(),
                generation_spec_digest: "sha256:generation".to_owned(),
                previous_generation_spec_digest: "sha256:previous".to_owned(),
            },
        );
        assert_eq!(operation.status, PluginOperationStatus::Preparing);

        apply_operation_event(
            &mut operation,
            2,
            &OnlineGenerationEvent::Switched {
                plugin_root_revision: "sha256:root".to_owned(),
                resolution_authority_digest: "sha256:authority".to_owned(),
                desired_state_digest: "sha256:desired".to_owned(),
                plan_digest: "sha256:plan".to_owned(),
                generation_spec_digest: "sha256:generation".to_owned(),
                previous_generation_spec_digest: "sha256:previous".to_owned(),
                routing_epoch: 2,
            },
        );
        assert_eq!(operation.status, PluginOperationStatus::Switched);

        apply_operation_event(
            &mut operation,
            3,
            &OnlineGenerationEvent::RolledBack {
                failed_generation_spec_digest: "sha256:generation".to_owned(),
                restored_generation_spec_digest: "sha256:previous".to_owned(),
                routing_epoch: 3,
                detail: "candidate failed after switching".to_owned(),
            },
        );
        assert_eq!(operation.status, PluginOperationStatus::RolledBack);
        assert_eq!(operation.cursor, 3);
    }

    #[test]
    fn partial_rejection_after_preparing_terminalizes_the_pending_receipt() {
        let mut operation = PluginOperationLedger::default().accept(
            0,
            Ok((
                "sha256:root-b".to_owned(),
                "sha256:desired-b".to_owned(),
                "sha256:plan-b".to_owned(),
            )),
        );
        apply_operation_event(
            &mut operation,
            1,
            &OnlineGenerationEvent::Preparing {
                plugin_root_revision: "sha256:root-b".to_owned(),
                resolution_authority_digest: "sha256:authority".to_owned(),
                desired_state_digest: "sha256:desired-b".to_owned(),
                plan_digest: "sha256:plan-b".to_owned(),
                generation_spec_digest: "sha256:generation-b".to_owned(),
                previous_generation_spec_digest: "sha256:generation-a".to_owned(),
            },
        );
        apply_operation_event(
            &mut operation,
            2,
            &OnlineGenerationEvent::Rejected {
                plugin_root_revision: Some("sha256:root-invalid-c".to_owned()),
                resolution_authority_digest: Some("sha256:authority".to_owned()),
                desired_state_digest: None,
                plan_digest: None,
                detail: "newer Plugin authoring state is invalid".to_owned(),
            },
        );

        assert_eq!(operation.status, PluginOperationStatus::Rejected);
        assert_eq!(
            operation.detail.as_deref(),
            Some("newer Plugin authoring state is invalid")
        );
        assert_eq!(operation.cursor, 2);
    }

    #[test]
    fn retained_new_partial_rejection_precedes_an_old_exact_retry_rejection() {
        assert_eq!(
            rejection_snapshot_settlement(Some(11), true, Some(5), 10, 10),
            RejectionSnapshotSettlement::LatestDesired,
        );
    }

    #[test]
    fn publication_replays_exact_terminal_event_before_receipt_registration() {
        let mut operation = PluginOperationLedger::default().accept_observing_from(
            2,
            0,
            0,
            Ok((
                "sha256:root".to_owned(),
                "sha256:desired".to_owned(),
                "sha256:plan".to_owned(),
            )),
        );
        apply_operation_event(
            &mut operation,
            1,
            &OnlineGenerationEvent::Preparing {
                plugin_root_revision: "sha256:root".to_owned(),
                resolution_authority_digest: "sha256:authority".to_owned(),
                desired_state_digest: "sha256:desired".to_owned(),
                plan_digest: "sha256:plan".to_owned(),
                generation_spec_digest: "sha256:generation".to_owned(),
                previous_generation_spec_digest: "sha256:previous".to_owned(),
            },
        );
        apply_operation_event(
            &mut operation,
            2,
            &OnlineGenerationEvent::Failed {
                generation_spec_digest: "sha256:generation".to_owned(),
                detail: "candidate failed before receipt registration".to_owned(),
            },
        );

        assert_eq!(operation.accepted_after_cursor, 2);
        assert_eq!(operation.status, PluginOperationStatus::Rejected);
        assert_eq!(
            operation.detail.as_deref(),
            Some("candidate failed before receipt registration")
        );
    }

    #[test]
    fn durable_rejected_snapshot_settles_receipt_after_event_window_gap() {
        let mut operation = PluginOperationLedger::default().accept_observing_from(
            80,
            0,
            0,
            Ok((
                "sha256:root".to_owned(),
                "sha256:desired".to_owned(),
                "sha256:plan".to_owned(),
            )),
        );

        settle_operation_from_rejected_snapshot(&mut operation, "sha256:generation", None);

        assert_eq!(operation.status, PluginOperationStatus::Rejected);
        assert_eq!(
            operation.generation_spec_digest.as_deref(),
            Some("sha256:generation")
        );
        assert!(
            operation
                .detail
                .as_deref()
                .unwrap()
                .contains("before its operation receipt was registered")
        );
    }

    #[test]
    fn old_same_identity_rejection_does_not_settle_a_new_retry() {
        let identity = PluginDesiredIdentity {
            desired_state_digest: "sha256:desired".to_owned(),
            plan_digest: "sha256:plan".to_owned(),
            plugin_root_revision: "sha256:root".to_owned(),
        };
        let accepted = AcceptedDesiredState {
            identity: identity.clone(),
            observe_after_cursor: 8,
            accepted_after_desired_epoch: 0,
            selection: fixture_desired(&identity),
        };

        assert!(!rejection_is_newer_than_acceptance(
            &identity,
            Some(7),
            Some(&accepted),
        ));
        assert!(rejection_is_newer_than_acceptance(
            &identity,
            Some(9),
            Some(&accepted),
        ));
        let operation = PluginOperationLedger::default().accept_observing_from(
            8,
            8,
            0,
            Ok((
                identity.plugin_root_revision,
                identity.desired_state_digest,
                identity.plan_digest,
            )),
        );
        assert!(!rejected_snapshot_is_newer(Some(7), 8));
        assert_eq!(operation.status, PluginOperationStatus::Accepted);
    }

    #[test]
    fn resource_only_identity_change_supersedes_the_pending_receipt() {
        let mut operation = PluginOperationLedger::default().accept(
            0,
            Ok((
                "sha256:root".to_owned(),
                "sha256:desired-one".to_owned(),
                "sha256:plan".to_owned(),
            )),
        );

        apply_operation_event(
            &mut operation,
            1,
            &OnlineGenerationEvent::Preparing {
                plugin_root_revision: "sha256:root".to_owned(),
                resolution_authority_digest: "sha256:authority".to_owned(),
                desired_state_digest: "sha256:desired-two".to_owned(),
                plan_digest: "sha256:plan".to_owned(),
                generation_spec_digest: "sha256:generation-two".to_owned(),
                previous_generation_spec_digest: "sha256:previous".to_owned(),
            },
        );

        assert_eq!(operation.status, PluginOperationStatus::Rejected);
        assert_eq!(
            operation.detail.as_deref(),
            Some("superseded by a newer Plugin Root mutation")
        );
        assert_eq!(operation.generation_spec_digest, None);
    }

    #[test]
    fn switched_receipt_ignores_ambiguous_rejection() {
        let mut operation = PluginOperationLedger::default().accept(
            0,
            Ok((
                "sha256:root".to_owned(),
                "sha256:desired".to_owned(),
                "sha256:plan".to_owned(),
            )),
        );
        apply_operation_event(
            &mut operation,
            1,
            &OnlineGenerationEvent::Switched {
                plugin_root_revision: "sha256:root".to_owned(),
                resolution_authority_digest: "sha256:authority".to_owned(),
                desired_state_digest: "sha256:desired".to_owned(),
                plan_digest: "sha256:plan".to_owned(),
                generation_spec_digest: "sha256:generation".to_owned(),
                previous_generation_spec_digest: "sha256:previous".to_owned(),
                routing_epoch: 1,
            },
        );

        apply_operation_event(
            &mut operation,
            2,
            &OnlineGenerationEvent::Rejected {
                plugin_root_revision: Some("sha256:root".to_owned()),
                resolution_authority_digest: Some("sha256:authority".to_owned()),
                desired_state_digest: None,
                plan_digest: None,
                detail: "another attempt was rejected".to_owned(),
            },
        );

        assert_eq!(operation.status, PluginOperationStatus::Switched);
        assert_eq!(operation.detail, None);
    }

    #[test]
    fn later_reactivation_does_not_reopen_an_old_receipts_rollback_window() {
        let mut first_b = PluginOperationLedger::default().accept(
            0,
            Ok((
                "sha256:root-b".to_owned(),
                "sha256:desired-b".to_owned(),
                "sha256:plan-b".to_owned(),
            )),
        );
        apply_operation_event(
            &mut first_b,
            1,
            &OnlineGenerationEvent::Switched {
                plugin_root_revision: "sha256:root-b".to_owned(),
                resolution_authority_digest: "sha256:authority".to_owned(),
                desired_state_digest: "sha256:desired-b".to_owned(),
                plan_digest: "sha256:plan-b".to_owned(),
                generation_spec_digest: "sha256:generation-b".to_owned(),
                previous_generation_spec_digest: "sha256:generation-a".to_owned(),
                routing_epoch: 1,
            },
        );
        apply_operation_event(
            &mut first_b,
            2,
            &OnlineGenerationEvent::Switched {
                plugin_root_revision: "sha256:root-c".to_owned(),
                resolution_authority_digest: "sha256:authority".to_owned(),
                desired_state_digest: "sha256:desired-c".to_owned(),
                plan_digest: "sha256:plan-c".to_owned(),
                generation_spec_digest: "sha256:generation-c".to_owned(),
                previous_generation_spec_digest: "sha256:generation-b".to_owned(),
                routing_epoch: 2,
            },
        );
        apply_operation_event(
            &mut first_b,
            3,
            &OnlineGenerationEvent::Switched {
                plugin_root_revision: "sha256:root-b".to_owned(),
                resolution_authority_digest: "sha256:authority".to_owned(),
                desired_state_digest: "sha256:desired-b".to_owned(),
                plan_digest: "sha256:plan-b".to_owned(),
                generation_spec_digest: "sha256:generation-b".to_owned(),
                previous_generation_spec_digest: "sha256:generation-c".to_owned(),
                routing_epoch: 3,
            },
        );
        apply_operation_event(
            &mut first_b,
            4,
            &OnlineGenerationEvent::RolledBack {
                failed_generation_spec_digest: "sha256:generation-b".to_owned(),
                restored_generation_spec_digest: "sha256:generation-c".to_owned(),
                routing_epoch: 4,
                detail: "second B activation rolled back".to_owned(),
            },
        );

        assert_eq!(first_b.status, PluginOperationStatus::Switched);
        assert_eq!(first_b.detail, None);
        assert!(!first_b.rollback_observable);
    }

    #[test]
    fn event_window_gap_expires_receipt_but_contiguous_window_does_not() {
        let mut ledger = PluginOperationLedger::default();
        let operation = ledger.accept(
            0,
            Ok((
                "sha256:root".to_owned(),
                "sha256:desired".to_owned(),
                "sha256:plan".to_owned(),
            )),
        );

        ledger.expire_before(1);
        assert!(ledger.get(&operation.id).is_some());

        ledger.expire_before(2);
        assert!(ledger.get(&operation.id).is_none());
        assert!(ledger.order.is_empty());
    }

    #[test]
    fn active_fallback_requires_the_complete_desired_identity() {
        let operation = PluginOperationLedger::default().accept(
            0,
            Ok((
                "sha256:root".to_owned(),
                "sha256:desired".to_owned(),
                "sha256:plan".to_owned(),
            )),
        );

        assert!(operation_identity_matches(
            &operation,
            "sha256:root",
            "sha256:desired",
            "sha256:plan"
        ));
        assert!(!operation_identity_matches(
            &operation,
            "sha256:other-root",
            "sha256:desired",
            "sha256:plan"
        ));
        assert!(!operation_identity_matches(
            &operation,
            "sha256:root",
            "sha256:other-desired",
            "sha256:plan"
        ));
    }

    #[test]
    fn configuration_status_distinguishes_same_root_desired_states() {
        let active = PluginDesiredIdentity {
            desired_state_digest: "sha256:active-desired".to_owned(),
            plan_digest: "sha256:plan".to_owned(),
            plugin_root_revision: "sha256:same-root".to_owned(),
        };
        let desired = PluginDesiredIdentity {
            desired_state_digest: "sha256:next-desired".to_owned(),
            plan_digest: "sha256:plan".to_owned(),
            plugin_root_revision: "sha256:same-root".to_owned(),
        };
        let complete_rejection = RejectedDesiredState {
            identity: Some(desired.clone()),
            plugin_root_revision: desired.plugin_root_revision.clone(),
        };
        let same_root_rejection = RejectedDesiredState {
            identity: None,
            plugin_root_revision: desired.plugin_root_revision.clone(),
        };

        assert_eq!(
            configuration_status(&active, &desired, None, None, None),
            "pending"
        );
        assert_eq!(
            configuration_status(&active, &desired, None, None, Some(&complete_rejection),),
            "rejected"
        );
        assert_eq!(
            configuration_status(&desired, &desired, None, None, Some(&same_root_rejection),),
            "applied"
        );
        assert_eq!(
            configuration_status(&desired, &desired, None, None, Some(&complete_rejection),),
            "rejected"
        );
        assert_eq!(
            configuration_status(&desired, &desired, None, Some(&desired), None),
            "pending"
        );
        assert_eq!(
            configuration_status(
                &active,
                &desired,
                Some(&desired),
                None,
                Some(&same_root_rejection),
            ),
            "pending"
        );
    }

    #[test]
    fn same_root_preparing_retry_clears_a_partial_rejection() {
        let active = PluginDesiredIdentity {
            desired_state_digest: "sha256:desired-a".to_owned(),
            plan_digest: "sha256:plan-a".to_owned(),
            plugin_root_revision: "sha256:root".to_owned(),
        };
        let desired = PluginDesiredIdentity {
            desired_state_digest: "sha256:desired-b".to_owned(),
            plan_digest: "sha256:plan-b".to_owned(),
            plugin_root_revision: "sha256:root".to_owned(),
        };
        let events = vec![
            OnlineGenerationEvent::Rejected {
                plugin_root_revision: Some("sha256:root".to_owned()),
                resolution_authority_digest: Some("sha256:authority".to_owned()),
                desired_state_digest: None,
                plan_digest: None,
                detail: "first attempt was rejected".to_owned(),
            },
            OnlineGenerationEvent::Preparing {
                plugin_root_revision: "sha256:root".to_owned(),
                resolution_authority_digest: "sha256:authority".to_owned(),
                desired_state_digest: desired.desired_state_digest.clone(),
                plan_digest: desired.plan_digest.clone(),
                generation_spec_digest: "sha256:generation-b".to_owned(),
                previous_generation_spec_digest: "sha256:generation-a".to_owned(),
            },
        ];
        let mut rejected = None;
        refresh_rejected_desired(&mut rejected, &PluginOperationLedger::default(), &events);

        assert_eq!(rejected, None);
        assert_eq!(
            configuration_status(&active, &desired, None, None, None),
            "pending"
        );
    }

    #[test]
    fn full_identity_accept_clears_an_older_same_root_partial_rejection() {
        let accepted = PluginDesiredIdentity {
            desired_state_digest: "sha256:desired-b".to_owned(),
            plan_digest: "sha256:plan-b".to_owned(),
            plugin_root_revision: "sha256:root".to_owned(),
        };
        let mut state = PluginRuntimeState {
            accepted_desired: None,
            configuration_authority: None,
            disableable: BTreeSet::new(),
            disableable_error: None,
            operations: PluginOperationLedger::default(),
            processed_rejection_cursor: 3,
            processed_selection_cursor: 0,
            rejected_desired: Some(RejectedDesiredState {
                identity: None,
                plugin_root_revision: "sha256:root".to_owned(),
            }),
            stream_id: FIXTURE_STREAM_ID.to_owned(),
        };

        state.observe_accepted_desired(accepted.clone(), fixture_desired(&accepted), 7, 11);

        assert_eq!(state.rejected_desired, None);
        assert_eq!(
            state.accepted_desired,
            Some(AcceptedDesiredState {
                identity: accepted.clone(),
                observe_after_cursor: 7,
                accepted_after_desired_epoch: 11,
                selection: fixture_desired(&accepted),
            })
        );
        assert_eq!(state.processed_rejection_cursor, 7);
    }

    #[test]
    fn accepted_desired_survives_an_unchanged_old_snapshot_until_it_is_applied() {
        let old = PluginDesiredIdentity {
            desired_state_digest: "sha256:desired-old".to_owned(),
            plan_digest: "sha256:plan-old".to_owned(),
            plugin_root_revision: "sha256:root-old".to_owned(),
        };
        let accepted_identity = PluginDesiredIdentity {
            desired_state_digest: "sha256:desired-next".to_owned(),
            plan_digest: "sha256:plan-next".to_owned(),
            plugin_root_revision: "sha256:root-next".to_owned(),
        };
        let accepted = AcceptedDesiredState {
            identity: accepted_identity.clone(),
            observe_after_cursor: 12,
            accepted_after_desired_epoch: 0,
            selection: fixture_desired(&accepted_identity),
        };

        assert!(!accepted_desired_is_applied(&old, &old, Some(&accepted)));
        assert!(accepted_desired_is_applied(
            &accepted_identity,
            &accepted_identity,
            Some(&accepted),
        ));
    }

    #[test]
    fn newer_out_of_band_desired_supersedes_the_accepted_overlay_and_receipt() {
        let accepted_identity = PluginDesiredIdentity {
            desired_state_digest: "sha256:desired-b".to_owned(),
            plan_digest: "sha256:plan-b".to_owned(),
            plugin_root_revision: "sha256:root-b".to_owned(),
        };
        let newer_snapshot_identity = PluginDesiredIdentity {
            desired_state_digest: "sha256:desired-c".to_owned(),
            plan_digest: "sha256:plan-c".to_owned(),
            plugin_root_revision: "sha256:root-c".to_owned(),
        };
        let accepted = AcceptedDesiredState {
            identity: accepted_identity.clone(),
            observe_after_cursor: 4,
            accepted_after_desired_epoch: 11,
            selection: fixture_desired(&accepted_identity),
        };
        let projected =
            if accepted_desired_overlay_is_current(12, &newer_snapshot_identity, None, &accepted) {
                accepted.identity.clone()
            } else {
                newer_snapshot_identity.clone()
            };
        let operation = PluginOperationLedger::default().accept_observing_from(
            4,
            4,
            11,
            Ok((
                accepted_identity.plugin_root_revision,
                accepted_identity.desired_state_digest,
                accepted_identity.plan_digest,
            )),
        );

        // A reconcile that completed before materialization is part of the
        // accepted fence and must not supersede the new receipt.
        assert!(accepted_desired_overlay_is_current(
            11,
            &newer_snapshot_identity,
            None,
            &accepted,
        ));
        assert!(!later_desired_selection_supersedes_operation(
            &operation,
            11,
            &newer_snapshot_identity,
        ));
        // A later valid or rejected Desired observation closes the overlay,
        // even when no lifecycle event can carry a complete identity.
        assert_eq!(projected, newer_snapshot_identity);
        assert!(later_desired_selection_supersedes_operation(
            &operation,
            12,
            &newer_snapshot_identity,
        ));
    }

    #[test]
    fn same_identity_exact_retry_keeps_inventory_pending_until_progress() {
        let identity = PluginDesiredIdentity {
            desired_state_digest: "sha256:desired-b".to_owned(),
            plan_digest: "sha256:plan-b".to_owned(),
            plugin_root_revision: "sha256:root-b".to_owned(),
        };
        let accepted = AcceptedDesiredState {
            identity: identity.clone(),
            observe_after_cursor: 10,
            accepted_after_desired_epoch: 5,
            selection: fixture_desired(&identity),
        };

        assert!(accepted_desired_overlay_is_current(
            6, &identity, None, &accepted,
        ));
        assert_eq!(
            configuration_status(&identity, &identity, None, Some(&accepted.identity), None,),
            "pending"
        );
        assert!(!accepted_desired_overlay_is_current(
            7,
            &identity,
            Some(11),
            &accepted,
        ));
    }

    #[test]
    fn identity_free_rejection_does_not_erase_prior_terminal_evidence() {
        let identity = PluginDesiredIdentity {
            desired_state_digest: "sha256:desired".to_owned(),
            plan_digest: "sha256:plan".to_owned(),
            plugin_root_revision: "sha256:root".to_owned(),
        };
        let mut rejected = Some(RejectedDesiredState::from_identity(identity.clone()));
        let events = vec![OnlineGenerationEvent::Rejected {
            plugin_root_revision: None,
            resolution_authority_digest: None,
            desired_state_digest: None,
            plan_digest: None,
            detail: "Controller state could not be inspected".to_owned(),
        }];
        refresh_rejected_desired(&mut rejected, &PluginOperationLedger::default(), &events);

        assert_eq!(rejected.and_then(|state| state.identity), Some(identity));
    }

    #[test]
    fn rollback_and_failed_generation_mark_their_exact_desired_identity_rejected() {
        let mut rejected = None;
        let operations = PluginOperationLedger::default();
        let rolled_back = vec![
            OnlineGenerationEvent::Preparing {
                plugin_root_revision: "sha256:root-b".to_owned(),
                resolution_authority_digest: "sha256:authority".to_owned(),
                desired_state_digest: "sha256:desired-b".to_owned(),
                plan_digest: "sha256:plan-b".to_owned(),
                generation_spec_digest: "sha256:generation-b".to_owned(),
                previous_generation_spec_digest: "sha256:generation-a".to_owned(),
            },
            OnlineGenerationEvent::Switched {
                plugin_root_revision: "sha256:root-b".to_owned(),
                resolution_authority_digest: "sha256:authority".to_owned(),
                desired_state_digest: "sha256:desired-b".to_owned(),
                plan_digest: "sha256:plan-b".to_owned(),
                generation_spec_digest: "sha256:generation-b".to_owned(),
                previous_generation_spec_digest: "sha256:generation-a".to_owned(),
                routing_epoch: 1,
            },
            OnlineGenerationEvent::RolledBack {
                failed_generation_spec_digest: "sha256:generation-b".to_owned(),
                restored_generation_spec_digest: "sha256:generation-a".to_owned(),
                routing_epoch: 2,
                detail: "candidate failed after switching".to_owned(),
            },
        ];
        refresh_rejected_desired(&mut rejected, &operations, &rolled_back);
        assert_eq!(
            rejected.as_ref().and_then(|state| state.identity.as_ref()),
            Some(&PluginDesiredIdentity {
                desired_state_digest: "sha256:desired-b".to_owned(),
                plan_digest: "sha256:plan-b".to_owned(),
                plugin_root_revision: "sha256:root-b".to_owned(),
            })
        );

        let failed = vec![
            OnlineGenerationEvent::Preparing {
                plugin_root_revision: "sha256:root-c".to_owned(),
                resolution_authority_digest: "sha256:authority".to_owned(),
                desired_state_digest: "sha256:desired-c".to_owned(),
                plan_digest: "sha256:plan-c".to_owned(),
                generation_spec_digest: "sha256:generation-c".to_owned(),
                previous_generation_spec_digest: "sha256:generation-a".to_owned(),
            },
            OnlineGenerationEvent::Failed {
                generation_spec_digest: "sha256:generation-c".to_owned(),
                detail: "candidate preparation failed".to_owned(),
            },
        ];
        refresh_rejected_desired(&mut rejected, &operations, &failed);
        assert_eq!(
            rejected.as_ref().and_then(|state| state.identity.as_ref()),
            Some(&PluginDesiredIdentity {
                desired_state_digest: "sha256:desired-c".to_owned(),
                plan_digest: "sha256:plan-c".to_owned(),
                plugin_root_revision: "sha256:root-c".to_owned(),
            })
        );
        let failed_identity = rejected
            .as_ref()
            .and_then(|state| state.identity.as_ref())
            .unwrap();
        assert_eq!(
            configuration_status(
                failed_identity,
                failed_identity,
                None,
                None,
                rejected.as_ref(),
            ),
            "rejected"
        );
    }

    #[test]
    fn newer_mutation_supersedes_pending_receipt_without_rewinding_its_cursor() {
        let mut ledger = PluginOperationLedger::default();
        let first = ledger.accept(
            3,
            Ok((
                "sha256:root-1".to_owned(),
                "sha256:desired-1".to_owned(),
                "sha256:plan-1".to_owned(),
            )),
        );
        ledger.operations.get_mut(&first.id).unwrap().cursor = 9;

        ledger.accept(
            7,
            Ok((
                "sha256:root-2".to_owned(),
                "sha256:desired-2".to_owned(),
                "sha256:plan-2".to_owned(),
            )),
        );

        let superseded = ledger.get(&first.id).unwrap();
        assert_eq!(superseded.status, PluginOperationStatus::Rejected);
        assert_eq!(superseded.cursor, 9);
        assert_eq!(
            superseded.detail.as_deref(),
            Some("superseded by a newer Plugin Root mutation")
        );
    }

    #[test]
    fn duplicate_receipt_does_not_reject_the_same_pending_desired_state() {
        let mut ledger = PluginOperationLedger::default();
        let identity = (
            "sha256:root".to_owned(),
            "sha256:desired".to_owned(),
            "sha256:plan".to_owned(),
        );
        let first = ledger.accept(3, Ok(identity.clone()));
        ledger.accept(4, Ok(identity));

        assert_eq!(
            ledger.get(&first.id).unwrap().status,
            PluginOperationStatus::Accepted
        );
    }
}
