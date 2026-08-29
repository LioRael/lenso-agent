use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    rc::Rc,
};

use lenso_app_plan::ResolvedAppPlan;

const MAX_ONLINE_GENERATION_EVENTS: usize = 64;
const MAX_RETAINED_GENERATION_PLANS: usize = 8;

/// One operator-visible lifecycle change from live Plugin reconciliation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OnlineGenerationEvent {
    Preparing {
        plugin_root_revision: String,
        resolution_authority_digest: String,
        desired_state_digest: String,
        plan_digest: String,
        generation_spec_digest: String,
        previous_generation_spec_digest: String,
    },
    Switched {
        plugin_root_revision: String,
        resolution_authority_digest: String,
        desired_state_digest: String,
        plan_digest: String,
        generation_spec_digest: String,
        previous_generation_spec_digest: String,
        routing_epoch: u64,
    },
    Rejected {
        plugin_root_revision: Option<String>,
        resolution_authority_digest: Option<String>,
        desired_state_digest: Option<String>,
        plan_digest: Option<String>,
        detail: String,
    },
    RolledBack {
        failed_generation_spec_digest: String,
        restored_generation_spec_digest: String,
        routing_epoch: u64,
        detail: String,
    },
    Failed {
        generation_spec_digest: String,
        detail: String,
    },
    WatchDegraded {
        detail: String,
    },
}

impl OnlineGenerationEvent {
    pub fn plugin_root_revision(&self) -> Option<&str> {
        match self {
            Self::Preparing {
                plugin_root_revision,
                ..
            }
            | Self::Switched {
                plugin_root_revision,
                ..
            } => Some(plugin_root_revision),
            Self::Rejected {
                plugin_root_revision,
                ..
            } => plugin_root_revision.as_deref(),
            Self::RolledBack { .. } | Self::Failed { .. } | Self::WatchDegraded { .. } => None,
        }
    }

    pub fn desired_state_digest(&self) -> Option<&str> {
        match self {
            Self::Preparing {
                desired_state_digest,
                ..
            }
            | Self::Switched {
                desired_state_digest,
                ..
            } => Some(desired_state_digest),
            Self::Rejected {
                desired_state_digest,
                ..
            } => desired_state_digest.as_deref(),
            Self::RolledBack { .. } | Self::Failed { .. } | Self::WatchDegraded { .. } => None,
        }
    }

    pub fn plan_digest(&self) -> Option<&str> {
        match self {
            Self::Preparing { plan_digest, .. } | Self::Switched { plan_digest, .. } => {
                Some(plan_digest)
            }
            Self::Rejected { plan_digest, .. } => plan_digest.as_deref(),
            Self::RolledBack { .. } | Self::Failed { .. } | Self::WatchDegraded { .. } => None,
        }
    }
}

/// One immutable event together with its monotonic process-local cursor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OnlineGenerationEventRecord {
    cursor: u64,
    event: OnlineGenerationEvent,
}

impl OnlineGenerationEventRecord {
    pub const fn cursor(&self) -> u64 {
        self.cursor
    }

    pub const fn event(&self) -> &OnlineGenerationEvent {
        &self.event
    }

    pub fn into_event(self) -> OnlineGenerationEvent {
        self.event
    }
}

/// Bounded non-destructive event projection after one caller-owned cursor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OnlineGenerationEventPage {
    cursor: u64,
    events: Vec<OnlineGenerationEventRecord>,
    truncated: bool,
}

impl OnlineGenerationEventPage {
    pub const fn cursor(&self) -> u64 {
        self.cursor
    }

    pub fn events(&self) -> &[OnlineGenerationEventRecord] {
        &self.events
    }

    pub const fn truncated(&self) -> bool {
        self.truncated
    }
}

#[derive(Debug, Default)]
pub(crate) struct OnlineGenerationEventLog {
    next_cursor: u64,
    records: VecDeque<OnlineGenerationEventRecord>,
}

impl OnlineGenerationEventLog {
    pub(crate) fn push(&mut self, event: OnlineGenerationEvent) -> u64 {
        self.next_cursor = self.next_cursor.saturating_add(1);
        if self.records.len() == MAX_ONLINE_GENERATION_EVENTS {
            self.records.pop_front();
        }
        self.records.push_back(OnlineGenerationEventRecord {
            cursor: self.next_cursor,
            event,
        });
        self.next_cursor
    }

    pub(crate) fn after(&self, after: Option<u64>) -> OnlineGenerationEventPage {
        let after = after.unwrap_or(0);
        let oldest = self
            .records
            .front()
            .map_or(self.next_cursor, |record| record.cursor);
        let truncated = after.saturating_add(1) < oldest;
        let events = self
            .records
            .iter()
            .filter(|record| record.cursor > after)
            .cloned()
            .collect();
        OnlineGenerationEventPage {
            cursor: self.next_cursor,
            events,
            truncated,
        }
    }
}

/// One Plan selected at a distinct point in the online Generation lifecycle.
#[derive(Clone, Debug)]
pub struct OnlineGenerationSelection {
    plugin_root_revision: String,
    desired_state_digest: String,
    generation_spec_digest: String,
    plan_digest: String,
    plan: Rc<ResolvedAppPlan>,
}

impl OnlineGenerationSelection {
    pub(crate) fn new(
        plugin_root_revision: String,
        desired_state_digest: String,
        generation_spec_digest: String,
        plan_digest: String,
        plan: ResolvedAppPlan,
    ) -> Self {
        Self {
            plugin_root_revision,
            desired_state_digest,
            generation_spec_digest,
            plan_digest,
            plan: Rc::new(plan),
        }
    }

    pub fn plugin_root_revision(&self) -> &str {
        &self.plugin_root_revision
    }

    pub fn desired_state_digest(&self) -> &str {
        &self.desired_state_digest
    }

    pub fn generation_spec_digest(&self) -> &str {
        &self.generation_spec_digest
    }

    pub fn plan_digest(&self) -> &str {
        &self.plan_digest
    }

    pub fn plan(&self) -> &ResolvedAppPlan {
        &self.plan
    }
}

/// Truthful Desired/Preparing/Active projection for presentation surfaces.
#[derive(Clone, Debug)]
pub struct OnlineGenerationSnapshot {
    active: OnlineGenerationSelection,
    desired: OnlineGenerationSelection,
    desired_rejection: Option<OnlineGenerationRejectionObservation>,
    preparing: Option<OnlineGenerationSelection>,
    rejected: Option<OnlineGenerationSelection>,
    rejected_cursor: Option<u64>,
    desired_epoch: u64,
}

/// The latest rejected Desired-authority observation, retained independently
/// from the bounded lifecycle event window.
#[derive(Clone, Debug)]
pub struct OnlineGenerationRejectionObservation {
    cursor: u64,
    event: OnlineGenerationEvent,
}

impl OnlineGenerationRejectionObservation {
    pub const fn cursor(&self) -> u64 {
        self.cursor
    }

    pub const fn event(&self) -> &OnlineGenerationEvent {
        &self.event
    }
}

impl OnlineGenerationSnapshot {
    pub fn active(&self) -> &OnlineGenerationSelection {
        &self.active
    }

    pub fn desired(&self) -> &OnlineGenerationSelection {
        &self.desired
    }

    pub fn desired_rejection(&self) -> Option<&OnlineGenerationRejectionObservation> {
        self.desired_rejection.as_ref()
    }

    pub fn preparing(&self) -> Option<&OnlineGenerationSelection> {
        self.preparing.as_ref()
    }

    /// Returns the complete Desired identity of the latest terminal rejection.
    ///
    /// This projection is independent of the bounded event window so a late
    /// observer does not lose a rollback or preparation failure.
    pub fn rejected(&self) -> Option<&OnlineGenerationSelection> {
        self.rejected.as_ref()
    }

    /// Returns the event cursor that established the latest exact rejection.
    pub const fn rejected_cursor(&self) -> Option<u64> {
        self.rejected_cursor
    }

    /// Returns the process-local epoch of the latest Desired-authority
    /// observation, including rejected authoring state and same-Generation
    /// selections that do not produce a lifecycle event.
    pub const fn desired_epoch(&self) -> u64 {
        self.desired_epoch
    }
}

#[derive(Debug)]
pub(crate) struct OnlineGenerationTracker {
    snapshot: OnlineGenerationSnapshot,
    retained: BTreeMap<String, OnlineGenerationSelection>,
    retained_order: VecDeque<String>,
    recovered_rejection_cursor: Option<u64>,
}

impl OnlineGenerationTracker {
    pub(crate) fn new(selection: OnlineGenerationSelection) -> Self {
        let mut retained = BTreeMap::new();
        retained.insert(selection.generation_spec_digest.clone(), selection.clone());
        let retained_order = VecDeque::from([selection.generation_spec_digest.clone()]);
        Self {
            snapshot: OnlineGenerationSnapshot {
                active: selection.clone(),
                desired: selection,
                desired_rejection: None,
                preparing: None,
                rejected: None,
                rejected_cursor: None,
                desired_epoch: 0,
            },
            retained,
            retained_order,
            recovered_rejection_cursor: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn preparing(&mut self, selection: OnlineGenerationSelection) {
        self.observe_desired(selection.clone());
        self.mark_preparing(selection);
    }

    pub(crate) fn mark_preparing(&mut self, selection: OnlineGenerationSelection) {
        self.snapshot.preparing = Some(selection.clone());
        self.snapshot.rejected = None;
        self.snapshot.rejected_cursor = None;
        self.recovered_rejection_cursor = None;
        self.retain(selection);
    }

    pub(crate) fn observe_desired(&mut self, selection: OnlineGenerationSelection) {
        let same_identity = selection_identity_matches(&self.snapshot.desired, &selection);
        let exact_rejection_cursor = self
            .snapshot
            .rejected
            .as_ref()
            .filter(|rejected| selection_identity_matches(rejected, &selection))
            .and(self.snapshot.rejected_cursor);
        let reopens_exact_rejection = exact_rejection_cursor.is_some()
            && exact_rejection_cursor != self.recovered_rejection_cursor;
        let repeats_retryable_attempt =
            same_identity && self.snapshot.desired_rejection.is_none() && !reopens_exact_rejection;
        if !repeats_retryable_attempt {
            self.observe_desired_attempt();
            self.snapshot.preparing = None;
            if exact_rejection_cursor.is_some() {
                self.recovered_rejection_cursor = exact_rejection_cursor;
            } else {
                self.snapshot.rejected = None;
                self.snapshot.rejected_cursor = None;
                self.recovered_rejection_cursor = None;
            }
        }
        self.snapshot.desired = selection.clone();
        self.snapshot.desired_rejection = None;
        self.retain(selection);
    }

    pub(crate) fn switched(&mut self, generation_spec_digest: &str) {
        if let Some(selection) = self.retained.get(generation_spec_digest).cloned() {
            self.snapshot.active = selection;
        }
        self.snapshot.preparing = None;
        self.snapshot.rejected = None;
        self.snapshot.rejected_cursor = None;
        self.recovered_rejection_cursor = None;
    }

    pub(crate) fn synchronize_active(
        &mut self,
        generation_spec_digest: &str,
        failed_generation_spec_digests: &BTreeSet<String>,
    ) -> bool {
        let Some(selection) = self.retained.get(generation_spec_digest).cloned() else {
            return false;
        };
        if let Some(failed) = self.retained_order.iter().rev().find_map(|digest| {
            failed_generation_spec_digests
                .contains(digest)
                .then(|| self.retained.get(digest))
                .flatten()
                .cloned()
        }) {
            self.snapshot.rejected = Some(failed);
            self.snapshot.rejected_cursor = None;
        } else {
            self.snapshot.rejected = None;
            self.snapshot.rejected_cursor = None;
        }
        self.recovered_rejection_cursor = None;
        self.snapshot.active = selection;
        self.snapshot.preparing = None;
        true
    }

    pub(crate) fn observe_resynchronized_rejection(&mut self, cursor: u64) {
        if self.snapshot.rejected.is_some() {
            self.snapshot.rejected_cursor = Some(cursor);
        }
    }

    pub(crate) fn rejected(&mut self, event: &OnlineGenerationEvent, cursor: u64) {
        self.observe_desired_attempt();
        self.snapshot.desired_rejection = Some(OnlineGenerationRejectionObservation {
            cursor,
            event: event.clone(),
        });
        let rejected = match event {
            OnlineGenerationEvent::Rejected {
                plugin_root_revision,
                desired_state_digest,
                plan_digest,
                ..
            } => {
                let desired = &self.snapshot.desired;
                complete_rejected_identity_matches(
                    plugin_root_revision.as_deref(),
                    desired_state_digest.as_deref(),
                    plan_digest.as_deref(),
                    desired.plugin_root_revision(),
                    desired.desired_state_digest(),
                    desired.plan_digest(),
                )
                .then(|| desired.clone())
            }
            _ => None,
        };
        if rejected.is_some() {
            self.snapshot.rejected = rejected;
            self.snapshot.rejected_cursor = Some(cursor);
            self.recovered_rejection_cursor = None;
        }
        self.snapshot.preparing = None;
    }

    pub(crate) fn failed(&mut self, generation_spec_digest: &str, cursor: u64) {
        if let Some(selection) = self.retained.get(generation_spec_digest).cloned() {
            self.snapshot.rejected = Some(selection);
            self.snapshot.rejected_cursor = Some(cursor);
            self.recovered_rejection_cursor = None;
        }
        self.snapshot.preparing = None;
    }

    pub(crate) fn rolled_back(
        &mut self,
        failed_generation_spec_digest: &str,
        restored_generation_spec_digest: &str,
        cursor: u64,
    ) {
        if let Some(selection) = self.retained.get(failed_generation_spec_digest).cloned() {
            self.snapshot.rejected = Some(selection);
            self.snapshot.rejected_cursor = Some(cursor);
            self.recovered_rejection_cursor = None;
        }
        if let Some(selection) = self.retained.get(restored_generation_spec_digest).cloned() {
            self.snapshot.active = selection;
        }
        self.snapshot.preparing = None;
    }

    pub(crate) fn snapshot(&self) -> OnlineGenerationSnapshot {
        self.snapshot.clone()
    }

    fn observe_desired_attempt(&mut self) {
        self.snapshot.desired_epoch = self.snapshot.desired_epoch.saturating_add(1);
    }

    pub(crate) fn retained_plan(
        &self,
        generation_spec_digest: &str,
    ) -> Option<Rc<ResolvedAppPlan>> {
        self.retained
            .get(generation_spec_digest)
            .map(|selection| Rc::clone(&selection.plan))
    }

    fn retain(&mut self, selection: OnlineGenerationSelection) {
        let digest = selection.generation_spec_digest.clone();
        if let Some(position) = self
            .retained_order
            .iter()
            .position(|retained| retained == &digest)
        {
            self.retained_order.remove(position);
        }
        if self.retained.len() == MAX_RETAINED_GENERATION_PLANS
            && !self.retained.contains_key(&digest)
        {
            let active = self.snapshot.active.generation_spec_digest();
            if let Some(position) = self
                .retained_order
                .iter()
                .position(|retained| retained != active)
                && let Some(oldest) = self.retained_order.remove(position)
            {
                self.retained.remove(&oldest);
            }
        }
        self.retained_order.push_back(digest.clone());
        self.retained.insert(digest, selection);
    }
}

fn selection_identity_matches(
    left: &OnlineGenerationSelection,
    right: &OnlineGenerationSelection,
) -> bool {
    left.plugin_root_revision() == right.plugin_root_revision()
        && left.desired_state_digest() == right.desired_state_digest()
        && left.generation_spec_digest() == right.generation_spec_digest()
        && left.plan_digest() == right.plan_digest()
}

fn complete_rejected_identity_matches(
    plugin_root_revision: Option<&str>,
    desired_state_digest: Option<&str>,
    plan_digest: Option<&str>,
    desired_plugin_root_revision: &str,
    desired_state: &str,
    desired_plan: &str,
) -> bool {
    plugin_root_revision == Some(desired_plugin_root_revision)
        && desired_state_digest == Some(desired_state)
        && plan_digest == Some(desired_plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_are_non_destructive_and_report_truncation() {
        let mut log = OnlineGenerationEventLog::default();
        log.push(OnlineGenerationEvent::WatchDegraded {
            detail: "one".to_owned(),
        });
        let first = log.after(None);
        let repeated = log.after(None);
        assert_eq!(first, repeated);
        assert_eq!(first.cursor(), 1);
        assert_eq!(log.after(Some(1)).events(), []);

        for index in 0..=MAX_ONLINE_GENERATION_EVENTS {
            log.push(OnlineGenerationEvent::WatchDegraded {
                detail: index.to_string(),
            });
        }
        assert!(log.after(Some(0)).truncated());
    }

    #[test]
    fn partial_rejection_cannot_be_promoted_to_exact_terminal_evidence() {
        assert!(!complete_rejected_identity_matches(
            Some("sha256:root"),
            None,
            None,
            "sha256:root",
            "sha256:desired",
            "sha256:plan",
        ));
        assert!(complete_rejected_identity_matches(
            Some("sha256:root"),
            Some("sha256:desired"),
            Some("sha256:plan"),
            "sha256:root",
            "sha256:desired",
            "sha256:plan",
        ));
    }

    #[test]
    fn rejected_authoring_state_advances_the_desired_observation_epoch() {
        let plan: ResolvedAppPlan =
            serde_json::from_slice(crate::test_support::headless_plan()).unwrap();
        let selection = OnlineGenerationSelection::new(
            "sha256:root-active".to_owned(),
            "sha256:desired-active".to_owned(),
            "sha256:generation-active".to_owned(),
            "sha256:plan-active".to_owned(),
            plan,
        );
        let mut tracker = OnlineGenerationTracker::new(selection);
        let rejection = OnlineGenerationEvent::Rejected {
            plugin_root_revision: Some("sha256:root-invalid".to_owned()),
            resolution_authority_digest: Some("sha256:authority".to_owned()),
            desired_state_digest: None,
            plan_digest: None,
            detail: "invalid Plugin authoring state".to_owned(),
        };

        tracker.rejected(&rejection, 1);

        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.desired_epoch(), 1);
        assert_eq!(
            snapshot.desired().desired_state_digest(),
            "sha256:desired-active"
        );
        assert!(snapshot.rejected().is_none());
        let observed = snapshot.desired_rejection().unwrap();
        assert_eq!(observed.cursor(), 1);
        assert_eq!(observed.event(), &rejection);
    }

    #[test]
    fn valid_desired_projection_survives_downstream_retries_without_false_supersession() {
        let plan: ResolvedAppPlan =
            serde_json::from_slice(crate::test_support::headless_plan()).unwrap();
        let first = OnlineGenerationSelection::new(
            "sha256:root-active".to_owned(),
            "sha256:desired-active".to_owned(),
            "sha256:generation-active".to_owned(),
            "sha256:plan-active".to_owned(),
            plan.clone(),
        );
        let next = OnlineGenerationSelection::new(
            "sha256:root-next".to_owned(),
            "sha256:desired-next".to_owned(),
            "sha256:generation-next".to_owned(),
            "sha256:plan-next".to_owned(),
            plan,
        );
        let mut tracker = OnlineGenerationTracker::new(first);

        tracker.observe_desired(next.clone());
        let first_observation = tracker.snapshot();
        assert_eq!(first_observation.desired_epoch(), 1);
        assert_eq!(
            first_observation.desired().desired_state_digest(),
            "sha256:desired-next"
        );
        assert!(first_observation.preparing().is_none());

        tracker.observe_desired(next);
        assert_eq!(tracker.snapshot().desired_epoch(), 1);
    }

    #[test]
    fn valid_desired_recovery_clears_retained_partial_rejection() {
        let plan: ResolvedAppPlan =
            serde_json::from_slice(crate::test_support::headless_plan()).unwrap();
        let selection = OnlineGenerationSelection::new(
            "sha256:root-valid".to_owned(),
            "sha256:desired-valid".to_owned(),
            "sha256:generation-valid".to_owned(),
            "sha256:plan-valid".to_owned(),
            plan,
        );
        let mut tracker = OnlineGenerationTracker::new(selection.clone());
        let rejection = OnlineGenerationEvent::Rejected {
            plugin_root_revision: Some("sha256:root-invalid".to_owned()),
            resolution_authority_digest: Some("sha256:authority".to_owned()),
            desired_state_digest: None,
            plan_digest: None,
            detail: "invalid Plugin authoring state".to_owned(),
        };
        tracker.rejected(&rejection, 1);

        tracker.observe_desired(selection);

        let snapshot = tracker.snapshot();
        assert_eq!(snapshot.desired_epoch(), 2);
        assert!(snapshot.desired_rejection().is_none());
        assert!(snapshot.preparing().is_none());
        assert!(snapshot.rejected().is_none());
    }

    #[test]
    fn exact_failure_fence_survives_retry_and_new_partial_rejection_until_preparing() {
        let plan: ResolvedAppPlan =
            serde_json::from_slice(crate::test_support::headless_plan()).unwrap();
        let selection = OnlineGenerationSelection::new(
            "sha256:root-valid".to_owned(),
            "sha256:desired-valid".to_owned(),
            "sha256:generation-valid".to_owned(),
            "sha256:plan-valid".to_owned(),
            plan,
        );
        let digest = selection.generation_spec_digest().to_owned();
        let mut tracker = OnlineGenerationTracker::new(selection.clone());
        tracker.failed(&digest, 5);

        tracker.observe_desired(selection.clone());
        tracker.observe_desired(selection.clone());
        let retrying = tracker.snapshot();
        assert_eq!(retrying.desired_epoch(), 1);
        assert_eq!(retrying.rejected_cursor(), Some(5));

        let partial_rejection = OnlineGenerationEvent::Rejected {
            plugin_root_revision: Some("sha256:root-invalid".to_owned()),
            resolution_authority_digest: Some("sha256:authority".to_owned()),
            desired_state_digest: None,
            plan_digest: None,
            detail: "later invalid Plugin authoring state".to_owned(),
        };
        tracker.rejected(&partial_rejection, 6);
        tracker.observe_desired(selection.clone());
        tracker.observe_desired(selection.clone());
        let recovered = tracker.snapshot();
        assert_eq!(recovered.desired_epoch(), 3);
        assert!(recovered.desired_rejection().is_none());
        assert_eq!(recovered.rejected_cursor(), Some(5));

        tracker.mark_preparing(selection);
        let preparing = tracker.snapshot();
        assert!(preparing.preparing().is_some());
        assert!(preparing.rejected().is_none());
        assert_eq!(preparing.rejected_cursor(), None);
    }

    #[test]
    fn retained_plan_lookup_survives_a_route_and_projection_switch_race() {
        let plan: ResolvedAppPlan =
            serde_json::from_slice(crate::test_support::headless_plan()).unwrap();
        let first = OnlineGenerationSelection::new(
            "sha256:root-one".to_owned(),
            "sha256:desired-one".to_owned(),
            "sha256:generation-one".to_owned(),
            "sha256:plan-one".to_owned(),
            plan.clone(),
        );
        let second = OnlineGenerationSelection::new(
            "sha256:root-two".to_owned(),
            "sha256:desired-two".to_owned(),
            "sha256:generation-two".to_owned(),
            "sha256:plan-two".to_owned(),
            plan,
        );
        let mut tracker = OnlineGenerationTracker::new(first);
        tracker.preparing(second);
        tracker.switched("sha256:generation-two");

        assert!(tracker.retained_plan("sha256:generation-one").is_some());
        assert!(tracker.retained_plan("sha256:generation-two").is_some());
    }
}
