//! spc_003-03: `WaitIndex` — the reverse index that turns "an event arrived" into "which tasks
//! wake up" without scanning every task. Pure in-memory data structure; not wired to `TaskTable`
//! or `Tcb.wait` in this card (that is spc_003-04).

use std::collections::{BTreeMap, HashMap};

use super::tcb::{
    ApprovalId, ChannelId, LogicalDeadline, ResourceKey, SignalFilter, SubscriptionId, TaskId,
    WaitCondition, WaitMode, WaitSet,
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
    fn keys_for(condition: &WaitCondition) -> Vec<WaitKey> {
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
    /// spc_003 debt closure: tasks registered through [`Self::register_wait_set`] — the
    /// `WaitSet` itself plus which of its `conditions` (by index) have already fired. Tasks
    /// registered through the plain [`Self::insert`]/[`Self::wake`] pair never appear here; those
    /// two mechanisms are independent by design (single-condition legacy waits don't need
    /// Any/All bookkeeping at all).
    pending_sets: HashMap<TaskId, (WaitSet, std::collections::BTreeSet<usize>)>,
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
        self.pending_sets
            .insert(task_id, (wait_set, std::collections::BTreeSet::new()));
    }

    /// spc_003 debt closure: notify every task registered under `key` (via
    /// [`Self::register_wait_set`]) that this condition fired, and return the ones whose whole
    /// `WaitSet` is now satisfied (`Any` ⇒ this condition alone; `All` ⇒ every condition fired).
    /// A task not yet fully satisfied stays registered under its remaining keys — this is the one
    /// difference from [`Self::wake`], which always unconditionally removes on any hit. A task
    /// with no tracked `WaitSet` (i.e. one only ever registered through [`Self::insert`] directly)
    /// is not touched here — call [`Self::wake`] for that path, as before.
    pub fn notify(&mut self, key: &WaitKey) -> Vec<TaskId> {
        let candidates = self.tasks_by_key.get(key).cloned().unwrap_or_default();
        let mut satisfied_tasks = Vec::new();
        for task_id in candidates {
            let Some((wait_set, satisfied)) = self.pending_sets.get_mut(&task_id) else {
                continue;
            };
            for (index, condition) in wait_set.conditions.iter().enumerate() {
                if WaitKey::keys_for(condition).contains(key) {
                    satisfied.insert(index);
                }
            }
            let is_satisfied = match wait_set.mode {
                WaitMode::Any => !satisfied.is_empty(),
                WaitMode::All => satisfied.len() == wait_set.conditions.len(),
            };
            if is_satisfied {
                satisfied_tasks.push(task_id);
            }
        }
        for task_id in &satisfied_tasks {
            if let Some((wait_set, _)) = self.pending_sets.remove(task_id) {
                for condition in &wait_set.conditions {
                    self.remove(task_id, condition);
                }
            }
        }
        satisfied_tasks
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
    fn any_mode_wakes_on_the_first_condition_satisfied() {
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

        let woken = index.notify(&WaitKey::Effect(e1));
        assert_eq!(woken, vec![TaskId::from("task-1")]);

        // Fully removed: the still-unfired e2 key must not still reference this task.
        assert!(index.lookup(&WaitKey::Effect(e2)).is_empty());
    }

    #[test]
    fn all_mode_waits_for_every_condition_before_waking() {
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

        let woken_after_e1 = index.notify(&WaitKey::Effect(e1.clone()));
        assert!(
            woken_after_e1.is_empty(),
            "All-mode must not wake until every condition has fired"
        );
        // Still registered under e2, waiting for the second half.
        assert_eq!(
            index.lookup(&WaitKey::Effect(e2.clone())),
            &[TaskId::from("task-1")]
        );

        let woken_after_e2 = index.notify(&WaitKey::Effect(e2));
        assert_eq!(woken_after_e2, vec![TaskId::from("task-1")]);

        // A stray redelivery of e1 after the task is already fully woken is a no-op.
        let woken_again = index.notify(&WaitKey::Effect(e1));
        assert!(woken_again.is_empty());
    }

    #[test]
    fn heterogeneous_any_condition_wakes_on_a_child_or_a_timer() {
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

        let woken = index.notify(&WaitKey::Child(TaskId::from("child-1")));
        assert_eq!(woken, vec![TaskId::from("task-1")]);
        // The timer side must be cleaned up too — no stray entry left in the timer bucket.
        assert_eq!(index.expire_timers(u64::MAX), Vec::<TaskId>::new());
    }
}
