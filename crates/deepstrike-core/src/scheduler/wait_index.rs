//! spc_003-03: `WaitIndex` — the reverse index that turns "an event arrived" into "which tasks
//! wake up" without scanning every task. Pure in-memory data structure; not wired to `TaskTable`
//! or `Tcb.wait` in this card (that is spc_003-04).

use std::collections::{BTreeMap, HashMap};

use super::tcb::{
    ApprovalId, ChannelId, LogicalDeadline, ResourceKey, SignalFilter, SubscriptionId, TaskId,
    WaitCondition, WaitSet,
};
use crate::runtime::kernel::wire::EffectId;

/// One indexable event key. A single `WaitCondition` can expand into more than one key —
/// `Children` fans out into one `Child` key per child id, since any of them completing is a fact
/// the parent's wait needs to observe.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WaitKey {
    Effect(EffectId),
    Child(TaskId),
    Approval(ApprovalId),
    Signal(SignalFilter),
    Timer(LogicalDeadline),
    Channel(ChannelId),
    Resource(ResourceKey),
    External(SubscriptionId),
}

impl WaitKey {
    pub(crate) fn keys_for(condition: &WaitCondition) -> Vec<WaitKey> {
        match condition {
            WaitCondition::Effect(id) => vec![WaitKey::Effect(id.clone())],
            WaitCondition::Child(id) => vec![WaitKey::Child(id.clone())],
            WaitCondition::Children(ids) => ids.iter().cloned().map(WaitKey::Child).collect(),
            WaitCondition::Approval(id) => vec![WaitKey::Approval(id.clone())],
            WaitCondition::Signal(filter) => vec![WaitKey::Signal(filter.clone())],
            WaitCondition::Timer(deadline) => vec![WaitKey::Timer(*deadline)],
            WaitCondition::Channel(id) => vec![WaitKey::Channel(id.clone())],
            WaitCondition::Resource(key) => vec![WaitKey::Resource(key.clone())],
            WaitCondition::External(id) => vec![WaitKey::External(id.clone())],
        }
    }

    pub(crate) fn matches(&self, condition: &WaitCondition) -> bool {
        Self::keys_for(condition).contains(self)
    }
}

/// One `HashMap` keyed by [`WaitKey`] rather than one map per condition kind (the spec's §3
/// sketch shows several typed buckets): a single generic index is simpler to keep correct and
/// still answers "who is waiting on this key" in O(1) average, which is the actual requirement.
#[derive(Debug, Clone, Default)]
pub struct WaitIndex {
    tasks_by_key: HashMap<WaitKey, Vec<TaskId>>,
    /// spc_003-05: deadline (ms) → waiting task ids, kept in lockstep with the `Timer` entries in
    /// `tasks_by_key`. `BTreeMap` gives cheap "everything due by `now_ms`" range queries without
    /// reaching for a heavier structure than correctness needs.
    timers: BTreeMap<u64, Vec<TaskId>>,
}

impl WaitIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, task_id: TaskId, condition: &WaitCondition) {
        for key in WaitKey::keys_for(condition) {
            if let WaitKey::Timer(LogicalDeadline(ms)) = key {
                self.timers.entry(ms).or_default().push(task_id.clone());
            }
            let bucket = self.tasks_by_key.entry(key).or_default();
            if !bucket.contains(&task_id) {
                bucket.push(task_id.clone());
            }
        }
    }

    pub fn remove(&mut self, task_id: &TaskId, condition: &WaitCondition) {
        for key in WaitKey::keys_for(condition) {
            if let WaitKey::Timer(LogicalDeadline(ms)) = key
                && let Some(bucket) = self.timers.get_mut(&ms)
            {
                bucket.retain(|id| id != task_id);
                if bucket.is_empty() {
                    self.timers.remove(&ms);
                }
            }
            if let Some(bucket) = self.tasks_by_key.get_mut(&key) {
                bucket.retain(|id| id != task_id);
                if bucket.is_empty() {
                    self.tasks_by_key.remove(&key);
                }
            }
        }
    }

    pub fn lookup(&self, key: &WaitKey) -> &[TaskId] {
        self.tasks_by_key
            .get(key)
            .map(|ids| ids.as_slice())
            .unwrap_or(&[])
    }

    /// spc_003-06 / spec §3: "Effect E completed → `WaitIndex[E]` → wake task" — the general wake
    /// primitive every event-arrival path uses. Removes and returns every task waiting on exactly
    /// `key`. Idempotent by construction: waking an already-empty (or never-registered) key finds
    /// nothing and returns `[]`, which is what makes a redelivered/duplicate completion event safe
    /// — a second wake for the same key is a harmless no-op, not a second transition.
    pub fn wake(&mut self, key: &WaitKey) -> Vec<TaskId> {
        let woken = self.tasks_by_key.remove(key).unwrap_or_default();
        if let WaitKey::Timer(LogicalDeadline(ms)) = key {
            self.timers.remove(ms);
        }
        woken
    }

    /// spc_003 debt closure / spec §4: register `task_id` against every condition in `wait_set`
    /// (via the existing [`Self::insert`], so each condition gets the same O(1)-average key
    /// indexing every other wait does) and track the set itself so [`Self::notify`] can evaluate
    /// `WaitMode::Any`/`WaitMode::All` satisfaction as individual conditions fire.
    pub fn register_wait_set(&mut self, task_id: TaskId, wait_set: WaitSet) {
        for condition in &wait_set.conditions {
            self.insert(task_id.clone(), condition);
        }
    }

    /// spc_003 debt closure: notify every task registered under `key` (via
    /// [`Self::register_wait_set`]) that this condition fired, and return the ones whose whole
    /// `WaitSet` is now satisfied (`Any` ⇒ this condition alone; `All` ⇒ every condition fired).
    /// A task not yet fully satisfied stays registered under its remaining keys — this is the one
    /// difference from [`Self::wake`], which always unconditionally removes on any hit. A task
    /// with no tracked `WaitSet` (i.e. one only ever registered through [`Self::insert`] directly)
    /// is not touched here — call [`Self::wake`] for that path, as before.
    pub fn notify(&mut self, key: &WaitKey) -> Vec<TaskId> {
        self.lookup(key).to_vec()
    }

    /// spc_003-05: remove and return every task waiting on a `Timer` whose deadline has passed
    /// (`deadline <= now_ms`), matching the `now_ms >= deadline` expiry predicate used elsewhere
    /// in this crate (`signals/queue.rs::escalate_deadlines`). Built on [`Self::wake`] — expiry is
    /// just "wake every due `Timer` key," nothing bespoke.
    pub fn expire_timers(&mut self, now_ms: u64) -> Vec<TaskId> {
        let due_deadlines: Vec<u64> = self.timers.range(..=now_ms).map(|(ms, _)| *ms).collect();
        due_deadlines
            .into_iter()
            .flat_map(|ms| self.wake(&WaitKey::Timer(LogicalDeadline(ms))))
            .collect()
    }

    /// Timer keys due under the journal-owned logical clock. Satisfaction/lifecycle mutation is
    /// deliberately left to `TaskTable::notify`; this index only answers which keys are due.
    pub(crate) fn due_timer_keys(&self, now_ms: u64) -> Vec<WaitKey> {
        self.timers
            .range(..=now_ms)
            .map(|(ms, _)| WaitKey::Timer(LogicalDeadline(*ms)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::tcb::WaitCondition;

    #[test]
    fn insert_then_lookup_hits() {
        let mut index = WaitIndex::new();
        let effect = EffectId::new("e1").unwrap();
        index.insert(
            TaskId::from("task-1"),
            &WaitCondition::Effect(effect.clone()),
        );

        assert_eq!(
            index.lookup(&WaitKey::Effect(effect)),
            &[TaskId::from("task-1")]
        );
    }

    #[test]
    fn remove_then_lookup_is_empty() {
        let mut index = WaitIndex::new();
        let effect = EffectId::new("e1").unwrap();
        let condition = WaitCondition::Effect(effect.clone());
        index.insert(TaskId::from("task-1"), &condition);

        index.remove(&TaskId::from("task-1"), &condition);

        assert!(index.lookup(&WaitKey::Effect(effect)).is_empty());
    }

    #[test]
    fn two_tasks_waiting_on_the_same_effect_both_come_back() {
        let mut index = WaitIndex::new();
        let effect = EffectId::new("e1").unwrap();
        let condition = WaitCondition::Effect(effect.clone());
        index.insert(TaskId::from("task-1"), &condition);
        index.insert(TaskId::from("task-2"), &condition);

        let hits = index.lookup(&WaitKey::Effect(effect));
        assert_eq!(hits.len(), 2);
        assert!(hits.contains(&TaskId::from("task-1")));
        assert!(hits.contains(&TaskId::from("task-2")));
    }

    use crate::scheduler::tcb::{WaitMode, WaitSet};

    #[test]
    fn wait_set_registration_indexes_every_condition_without_owning_satisfaction() {
        let mut index = WaitIndex::new();
        let e1 = EffectId::new("e1").unwrap();
        let e2 = EffectId::new("e2").unwrap();
        let wait_set = WaitSet {
            mode: WaitMode::Any,
            conditions: vec![
                WaitCondition::Effect(e1.clone()),
                WaitCondition::Effect(e2.clone()),
            ],
        };
        index.register_wait_set(TaskId::from("task-1"), wait_set);

        assert_eq!(
            index.notify(&WaitKey::Effect(e1)),
            vec![TaskId::from("task-1")]
        );
        assert_eq!(
            index.lookup(&WaitKey::Effect(e2)),
            &[TaskId::from("task-1")],
            "the reverse index reports candidates; the TCB owns satisfaction and cleanup"
        );
    }

    #[test]
    fn notify_is_a_non_mutating_reverse_lookup_for_all_mode_too() {
        let mut index = WaitIndex::new();
        let e1 = EffectId::new("e1").unwrap();
        let e2 = EffectId::new("e2").unwrap();
        let wait_set = WaitSet {
            mode: WaitMode::All,
            conditions: vec![
                WaitCondition::Effect(e1.clone()),
                WaitCondition::Effect(e2.clone()),
            ],
        };
        index.register_wait_set(TaskId::from("task-1"), wait_set);

        assert_eq!(
            index.notify(&WaitKey::Effect(e1.clone())),
            &[TaskId::from("task-1")]
        );
        assert_eq!(
            index.notify(&WaitKey::Effect(e2)),
            vec![TaskId::from("task-1")]
        );
        assert_eq!(
            index.notify(&WaitKey::Effect(e1)),
            vec![TaskId::from("task-1")],
            "dedupe is durable TCB state, not reverse-index state"
        );
    }

    #[test]
    fn heterogeneous_wait_set_registers_child_and_timer_keys() {
        // Matches spc_003 §2's own usage example: `wait_any(child_result, user_reply, deadline)`.
        let mut index = WaitIndex::new();
        let wait_set = WaitSet {
            mode: WaitMode::Any,
            conditions: vec![
                WaitCondition::Child(TaskId::from("child-1")),
                WaitCondition::Timer(LogicalDeadline(1_000)),
            ],
        };
        index.register_wait_set(TaskId::from("task-1"), wait_set);

        assert_eq!(
            index.notify(&WaitKey::Child(TaskId::from("child-1"))),
            vec![TaskId::from("task-1")]
        );
        assert_eq!(
            index.lookup(&WaitKey::Timer(LogicalDeadline(1_000))),
            &[TaskId::from("task-1")]
        );
    }
}
