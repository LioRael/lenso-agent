//! Bounded, owner-fair admission above immutable Generation leases.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueuePolicy {
    /// Retain a bounded waiting queue; reject overflow.
    #[default]
    Queue,
    /// Reject a Turn immediately if it cannot run now.
    Reject,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentWebScheduling {
    pub global_running: usize,
    pub per_user_running: usize,
    pub global_queue: usize,
    pub per_user_queue: usize,
    /// Round robin across owners. FIFO within each owner and Session always applies.
    pub fair: bool,
    pub queue_policy: QueuePolicy,
}
impl Default for AgentWebScheduling {
    fn default() -> Self {
        Self {
            global_running: 1,
            per_user_running: 1,
            global_queue: 16,
            per_user_queue: 16,
            fair: true,
            queue_policy: QueuePolicy::Queue,
        }
    }
}
impl AgentWebScheduling {
    pub(crate) fn validate(&self) -> Result<(), String> {
        if self.global_running == 0
            || self.per_user_running == 0
            || self.global_running > lenso_agent_host::generation::WEB_TURN_CONCURRENCY_CEILING
            || self.per_user_running > self.global_running
            || self.global_queue > 65536
            || self.per_user_queue > self.global_queue
        {
            return Err("Invalid Agent Turn scheduling limits".into());
        }
        Ok(())
    }
}
#[derive(Debug)]
struct Waiting<T> {
    owner: String,
    request: String,
    session: Option<String>,
    value: T,
}
pub(crate) struct Scheduler<T> {
    config: AgentWebScheduling,
    waiting: VecDeque<Waiting<T>>,
    running: BTreeMap<(String, String), Option<String>>,
    last_owner: Option<String>,
}
impl<T> Scheduler<T> {
    pub(crate) fn new(config: AgentWebScheduling) -> Self {
        Self {
            config,
            waiting: VecDeque::new(),
            running: BTreeMap::new(),
            last_owner: None,
        }
    }
    fn running_for(&self, owner: &str) -> usize {
        self.running.keys().filter(|(o, _)| o == owner).count()
    }
    fn eligible(&self, owner: &str, session: Option<&str>) -> bool {
        self.running.len() < self.config.global_running
            && self.running_for(owner) < self.config.per_user_running
            && session
                .is_none_or(|session| !self.running.values().any(|s| s.as_deref() == Some(session)))
    }
    pub(crate) fn enqueue(
        &mut self,
        owner: String,
        request: String,
        session: Option<String>,
        value: T,
    ) -> Result<(), (T, &'static str)> {
        if self.running.contains_key(&(owner.clone(), request.clone()))
            || self
                .waiting
                .iter()
                .any(|w| w.owner == owner && w.request == request)
        {
            return Err((value, "Agent request ID is already admitted"));
        }
        let queued_owner = self.waiting.iter().filter(|w| w.owner == owner).count();
        // One dispatch slot may be reserved when idle, including queue=0.
        let immediate = self.waiting.is_empty() && self.eligible(&owner, session.as_deref());
        if !immediate
            && (self.config.queue_policy == QueuePolicy::Reject
                || self.waiting.len() >= self.config.global_queue
                || queued_owner >= self.config.per_user_queue)
        {
            return Err((value, "Agent Turn capacity is exhausted"));
        }
        self.waiting.push_back(Waiting {
            owner,
            request,
            session,
            value,
        });
        Ok(())
    }
    pub(crate) fn pop_ready(&mut self) -> Option<T> {
        let mut seen_sessions = BTreeSet::new();
        let candidates = self
            .waiting
            .iter()
            .enumerate()
            .filter_map(|(i, w)| {
                let ordered = w
                    .session
                    .as_ref()
                    .is_none_or(|session| seen_sessions.insert(session.clone()));
                (ordered && self.eligible(&w.owner, w.session.as_deref())).then_some((i, &w.owner))
            })
            .collect::<Vec<_>>();
        let index = if self.config.fair {
            candidates
                .iter()
                .filter(|(_, owner)| self.last_owner.as_ref().is_none_or(|last| *owner > last))
                .min_by(|(_, a), (_, b)| a.cmp(b))
                .or_else(|| candidates.iter().min_by(|(_, a), (_, b)| a.cmp(b)))
                .map(|(i, _)| *i)
        } else {
            candidates.first().map(|(i, _)| *i)
        }?;
        let waiting = self.waiting.remove(index)?;
        self.last_owner = Some(waiting.owner.clone());
        self.running
            .insert((waiting.owner, waiting.request), waiting.session);
        Some(waiting.value)
    }
    /// Called before the first event exposes a generated or forked Session ID.
    pub(crate) fn bind_session(
        &mut self,
        key: &(String, String),
        session: String,
    ) -> Result<(), String> {
        if self
            .running
            .iter()
            .any(|(other, active)| other != key && active.as_ref() == Some(&session))
        {
            return Err("Session already has an active Turn".into());
        }
        let active = self
            .running
            .get_mut(key)
            .ok_or_else(|| "Turn admission is no longer active".to_owned())?;
        *active = Some(session);
        Ok(())
    }
    pub(crate) fn finish(&mut self, key: &(String, String)) {
        self.running.remove(key);
    }
    pub(crate) fn cancel(&mut self, key: &(String, String)) -> Option<T> {
        let index = self
            .waiting
            .iter()
            .position(|w| (&w.owner, &w.request) == (&key.0, &key.1))?;
        self.waiting.remove(index).map(|w| w.value)
    }
    pub(crate) fn is_running(&self) -> bool {
        !self.running.is_empty()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn parallel() -> AgentWebScheduling {
        AgentWebScheduling {
            global_running: 2,
            per_user_running: 1,
            global_queue: 3,
            per_user_queue: 2,
            ..Default::default()
        }
    }
    #[test]
    fn different_owners_run_while_same_session_waits_and_finish_preserves_order() {
        let mut s = Scheduler::new(parallel());
        s.enqueue("a".into(), "1".into(), Some("s".into()), 1)
            .unwrap();
        s.enqueue("a".into(), "2".into(), Some("s".into()), 2)
            .unwrap();
        s.enqueue("b".into(), "3".into(), Some("other".into()), 3)
            .unwrap();
        assert_eq!(s.pop_ready(), Some(1));
        assert_eq!(s.pop_ready(), Some(3));
        assert_eq!(s.pop_ready(), None);
        s.finish(&("a".into(), "1".into()));
        assert_eq!(s.pop_ready(), Some(2));
    }
    #[test]
    fn cancellation_cannot_select_another_owner_and_limits_are_bounded() {
        let mut s = Scheduler::new(parallel());
        s.enqueue("a".into(), "1".into(), None, 1).unwrap();
        assert_eq!(s.pop_ready(), Some(1));
        s.enqueue("a".into(), "2".into(), None, 2).unwrap();
        s.enqueue("a".into(), "3".into(), None, 3).unwrap();
        assert!(s.enqueue("a".into(), "4".into(), None, 4).is_err());
        assert_eq!(s.cancel(&("b".into(), "2".into())), None);
        assert_eq!(s.cancel(&("a".into(), "2".into())), Some(2));
        s.enqueue("b".into(), "2".into(), None, 5).unwrap();
        assert_eq!(s.pop_ready(), Some(5));
    }
    #[test]
    fn reject_policy_and_duplicate_ids_do_not_grow_queue() {
        let mut config = parallel();
        config.queue_policy = QueuePolicy::Reject;
        let mut s = Scheduler::new(config);
        s.enqueue("a".into(), "1".into(), None, 1).unwrap();
        s.pop_ready();
        assert!(s.enqueue("a".into(), "2".into(), None, 2).is_err());
        assert!(s.enqueue("a".into(), "1".into(), None, 3).is_err());
    }
    #[test]
    fn generated_session_binding_precedes_visibility_and_blocks_followups() {
        let mut config = parallel();
        config.per_user_running = 2;
        let mut s = Scheduler::new(config);
        s.enqueue("a".into(), "new".into(), None, 1).unwrap();
        s.pop_ready();
        s.bind_session(&("a".into(), "new".into()), "created".into())
            .unwrap();
        s.enqueue("a".into(), "followup".into(), Some("created".into()), 2)
            .unwrap();
        assert_eq!(s.pop_ready(), None);
        s.finish(&("a".into(), "new".into()));
        assert_eq!(s.pop_ready(), Some(2));
    }
    #[test]
    fn round_robin_rotates_three_owners_without_starving_the_third() {
        let mut config = parallel();
        config.global_running = 1;
        config.global_queue = 8;
        config.per_user_queue = 4;
        let mut s = Scheduler::new(config);
        for (owner, request, value) in [
            ("a", "1", 1),
            ("a", "2", 2),
            ("b", "1", 3),
            ("b", "2", 4),
            ("c", "1", 5),
            ("c", "2", 6),
        ] {
            s.enqueue(owner.into(), request.into(), None, value)
                .unwrap();
        }
        assert_eq!(s.pop_ready(), Some(1));
        s.finish(&("a".into(), "1".into()));
        assert_eq!(s.pop_ready(), Some(3));
        s.finish(&("b".into(), "1".into()));
        assert_eq!(s.pop_ready(), Some(5));
        s.finish(&("c".into(), "1".into()));
        assert_eq!(s.pop_ready(), Some(2));
    }
}
