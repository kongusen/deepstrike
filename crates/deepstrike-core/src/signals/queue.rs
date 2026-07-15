use std::cmp::Ordering;
use std::collections::BinaryHeap;

use compact_str::CompactString;

use crate::types::signal::{RuntimeSignal, Urgency};

/// Wrapper for priority ordering: higher urgency first, then older timestamp first.
struct PrioritizedSignal {
    urgency: Urgency,
    timestamp_ms: u64,
    deadline_escalated: bool,
    dedupe_keys: Vec<CompactString>,
    signal: RuntimeSignal,
}

impl PartialEq for PrioritizedSignal {
    fn eq(&self, other: &Self) -> bool {
        self.signal.id == other.signal.id
    }
}
impl Eq for PrioritizedSignal {}

impl PartialOrd for PrioritizedSignal {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PrioritizedSignal {
    fn cmp(&self, other: &Self) -> Ordering {
        self.urgency
            .cmp(&other.urgency)
            .then_with(|| other.timestamp_ms.cmp(&self.timestamp_ms))
            .then_with(|| self.signal.id.cmp(&other.signal.id))
    }
}

/// Priority queue for runtime signals. Internal to the signals module.
pub(super) struct SignalQueue {
    heap: BinaryHeap<PrioritizedSignal>,
    max_size: usize,
}

pub(super) struct QueueAdmission {
    pub(super) admitted: bool,
    pub(super) displaced: Option<RuntimeSignal>,
    pub(super) displaced_dedupe_keys: Vec<CompactString>,
}

impl SignalQueue {
    pub(super) fn new(max_size: usize) -> Self {
        Self {
            heap: BinaryHeap::new(),
            max_size,
        }
    }

    /// Admit using the queue's deterministic capacity policy without TTL cleanup.
    #[cfg(test)]
    pub(super) fn push(&mut self, signal: RuntimeSignal) -> bool {
        self.admit(signal).admitted
    }

    /// At capacity, a new signal may replace only a strictly lower-urgency entry. Among the lowest
    /// urgency, the newest entry is displaced so an older waiter never loses its place to an
    /// equal-priority arrival. The router calls [`Self::expire`] before admission.
    #[cfg(test)]
    pub(super) fn admit(&mut self, signal: RuntimeSignal) -> QueueAdmission {
        self.admit_with_deadline_state(signal, false)
    }

    pub(super) fn admit_with_deadline_state(
        &mut self,
        signal: RuntimeSignal,
        deadline_escalated: bool,
    ) -> QueueAdmission {
        if let Some(key) = signal.coalesce_key.as_ref() {
            let existing_id = self
                .heap
                .iter()
                .find(|queued| queued.signal.coalesce_key.as_ref() == Some(key))
                .map(|queued| queued.signal.id);
            if let Some(existing_id) = existing_id {
                let mut retained = BinaryHeap::with_capacity(self.heap.len());
                for mut queued in self.heap.drain() {
                    if queued.signal.id == existing_id {
                        queued.signal.coalesced_count = queued
                            .signal
                            .coalesced_count
                            .max(1)
                            .saturating_add(signal.coalesced_count.max(1));
                        queued.signal.urgency = queued.signal.urgency.max(signal.urgency);
                        queued.urgency = queued.signal.urgency;
                        queued.signal.deadline_ms =
                            earliest_deadline(queued.signal.deadline_ms, signal.deadline_ms);
                        queued.deadline_escalated |= deadline_escalated;
                        if let Some(dedupe_key) = signal.dedupe_key.as_ref() {
                            if !queued.dedupe_keys.contains(dedupe_key) {
                                queued.dedupe_keys.push(dedupe_key.clone());
                            }
                        }
                    }
                    retained.push(queued);
                }
                self.heap = retained;
                return QueueAdmission {
                    admitted: true,
                    displaced: None,
                    displaced_dedupe_keys: Vec::new(),
                };
            }
        }

        if self.heap.len() >= self.max_size {
            let lowest = self.heap.iter().map(|queued| queued.urgency).min();
            if lowest.is_none_or(|urgency| signal.urgency <= urgency) {
                return QueueAdmission {
                    admitted: false,
                    displaced: None,
                    displaced_dedupe_keys: Vec::new(),
                };
            }

            let lowest = lowest.expect("a full queue has a lowest urgency");
            let displaced_id = self
                .heap
                .iter()
                .filter(|queued| queued.urgency == lowest)
                .max_by(|left, right| {
                    left.timestamp_ms
                        .cmp(&right.timestamp_ms)
                        .then_with(|| left.signal.id.cmp(&right.signal.id))
                })
                .map(|queued| queued.signal.id)
                .expect("a full queue has a displacement candidate");
            let mut displaced = None;
            let mut displaced_dedupe_keys = Vec::new();
            let mut retained = BinaryHeap::with_capacity(self.heap.len());
            for queued in self.heap.drain() {
                if queued.signal.id == displaced_id {
                    displaced = Some(queued.signal);
                    displaced_dedupe_keys = queued.dedupe_keys;
                } else {
                    retained.push(queued);
                }
            }
            self.heap = retained;

            let urgency = signal.urgency;
            let timestamp_ms = signal.timestamp_ms;
            let dedupe_keys = signal.dedupe_key.iter().cloned().collect();
            self.heap.push(PrioritizedSignal {
                urgency,
                timestamp_ms,
                deadline_escalated,
                dedupe_keys,
                signal,
            });
            return QueueAdmission {
                admitted: true,
                displaced,
                displaced_dedupe_keys,
            };
        }

        let urgency = signal.urgency;
        let timestamp_ms = signal.timestamp_ms;
        let dedupe_keys = signal.dedupe_key.iter().cloned().collect();
        self.heap.push(PrioritizedSignal {
            urgency,
            timestamp_ms,
            deadline_escalated,
            dedupe_keys,
            signal,
        });
        QueueAdmission {
            admitted: true,
            displaced: None,
            displaced_dedupe_keys: Vec::new(),
        }
    }

    /// Remove entries whose timestamp plus the configured TTL has elapsed.
    pub(super) fn expire(
        &mut self,
        now_ms: u64,
        ttl_ms: Option<u64>,
    ) -> Vec<(RuntimeSignal, Vec<CompactString>)> {
        let Some(ttl_ms) = ttl_ms else {
            return Vec::new();
        };
        let mut expired = Vec::new();
        let mut retained = BinaryHeap::with_capacity(self.heap.len());
        for queued in self.heap.drain() {
            let has_timestamp = queued.timestamp_ms > 0;
            let reached_expiry = now_ms >= queued.timestamp_ms.saturating_add(ttl_ms);
            if has_timestamp && reached_expiry {
                expired.push((queued.signal, queued.dedupe_keys));
            } else {
                retained.push(queued);
            }
        }
        self.heap = retained;
        expired
    }

    /// Promote each due queued signal at most once, then rebuild priority ordering.
    pub(super) fn escalate_deadlines(&mut self, now_ms: u64) {
        let mut rebuilt = BinaryHeap::with_capacity(self.heap.len());
        for mut queued in self.heap.drain() {
            let due = queued
                .signal
                .deadline_ms
                .is_some_and(|deadline_ms| now_ms >= deadline_ms);
            if due && !queued.deadline_escalated {
                queued.signal.urgency = escalate_one_tier(queued.signal.urgency);
                queued.urgency = queued.signal.urgency;
                queued.deadline_escalated = true;
            }
            rebuilt.push(queued);
        }
        self.heap = rebuilt;
    }

    pub(super) fn pop(&mut self) -> Option<RuntimeSignal> {
        self.heap.pop().map(|ps| ps.signal)
    }

    pub(super) fn len(&self) -> usize {
        self.heap.len()
    }
}

fn earliest_deadline(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
        (None, None) => None,
    }
}

fn escalate_one_tier(urgency: Urgency) -> Urgency {
    match urgency {
        Urgency::Low => Urgency::Normal,
        Urgency::Normal => Urgency::High,
        Urgency::High | Urgency::Critical => Urgency::Critical,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::signal::{SignalSource, SignalType};

    #[test]
    fn higher_urgency_dequeued_first() {
        let mut q = SignalQueue::new(10);
        q.push(
            RuntimeSignal::new(SignalSource::Cron, SignalType::Event, Urgency::Low, "low")
                .with_timestamp(1),
        );
        q.push(
            RuntimeSignal::new(
                SignalSource::Gateway,
                SignalType::Alert,
                Urgency::Critical,
                "crit",
            )
            .with_timestamp(2),
        );
        q.push(
            RuntimeSignal::new(
                SignalSource::Cron,
                SignalType::Event,
                Urgency::Normal,
                "norm",
            )
            .with_timestamp(3),
        );

        assert_eq!(q.pop().unwrap().urgency, Urgency::Critical);
        assert_eq!(q.pop().unwrap().urgency, Urgency::Normal);
        assert_eq!(q.pop().unwrap().urgency, Urgency::Low);
    }

    #[test]
    fn respects_max_size() {
        let mut q = SignalQueue::new(1);
        assert!(
            q.push(
                RuntimeSignal::new(SignalSource::Cron, SignalType::Event, Urgency::Low, "a")
                    .with_timestamp(1)
            )
        );
        assert!(
            !q.push(
                RuntimeSignal::new(SignalSource::Cron, SignalType::Event, Urgency::Low, "b")
                    .with_timestamp(2)
            )
        );
    }

    #[test]
    fn same_urgency_older_first() {
        let mut q = SignalQueue::new(10);
        q.push(
            RuntimeSignal::new(
                SignalSource::Cron,
                SignalType::Event,
                Urgency::Normal,
                "newer",
            )
            .with_timestamp(100),
        );
        q.push(
            RuntimeSignal::new(
                SignalSource::Cron,
                SignalType::Event,
                Urgency::Normal,
                "older",
            )
            .with_timestamp(1),
        );

        assert_eq!(q.pop().unwrap().summary.as_str(), "older");
    }

    #[test]
    fn full_queue_accepts_strictly_higher_urgency_and_preserves_oldest_lowest() {
        let mut q = SignalQueue::new(2);
        assert!(
            q.push(
                RuntimeSignal::new(SignalSource::Cron, SignalType::Event, Urgency::Low, "old")
                    .with_timestamp(1)
            )
        );
        assert!(
            q.push(
                RuntimeSignal::new(SignalSource::Cron, SignalType::Event, Urgency::Low, "new")
                    .with_timestamp(2)
            )
        );

        let admission = q.admit(
            RuntimeSignal::new(
                SignalSource::Gateway,
                SignalType::Alert,
                Urgency::Critical,
                "critical",
            )
            .with_timestamp(3),
        );
        assert!(admission.admitted);
        assert_eq!(
            admission
                .displaced
                .as_ref()
                .map(|signal| signal.summary.as_str()),
            Some("new")
        );

        assert_eq!(q.pop().unwrap().summary.as_str(), "critical");
        assert_eq!(q.pop().unwrap().summary.as_str(), "old");
    }

    #[test]
    fn expired_entries_are_removed_before_capacity_is_evaluated() {
        let mut q = SignalQueue::new(1);
        assert!(
            q.push(
                RuntimeSignal::new(
                    SignalSource::Cron,
                    SignalType::Event,
                    Urgency::Critical,
                    "stale"
                )
                .with_timestamp(10)
            )
        );

        let expired = q.expire(30, Some(10));
        let admission = q.admit(
            RuntimeSignal::new(SignalSource::Cron, SignalType::Event, Urgency::Low, "fresh")
                .with_timestamp(30),
        );

        assert!(admission.admitted);
        assert!(admission.displaced.is_none());
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].0.summary.as_str(), "stale");
        assert_eq!(q.pop().unwrap().summary.as_str(), "fresh");
    }

    #[test]
    fn coalescing_keeps_first_identity_and_combines_policy_inputs() {
        let mut q = SignalQueue::new(1);
        let first = RuntimeSignal::new(
            SignalSource::Cron,
            SignalType::Event,
            Urgency::Normal,
            "first",
        )
        .with_timestamp(10)
        .with_deadline(200)
        .with_coalesce("batch");
        let first_id = first.id;
        assert!(q.admit(first).admitted);

        let second = RuntimeSignal::new(
            SignalSource::Cron,
            SignalType::Event,
            Urgency::High,
            "second",
        )
        .with_timestamp(20)
        .with_deadline(100)
        .with_coalesce("batch");
        let admission = q.admit(second);

        assert!(admission.admitted);
        assert!(admission.displaced.is_none());
        assert_eq!(q.len(), 1);
        let merged = q.pop().unwrap();
        assert_eq!(merged.id, first_id);
        assert_eq!(merged.urgency, Urgency::High);
        assert_eq!(merged.deadline_ms, Some(100));
        assert_eq!(merged.coalesced_count, 2);
    }
}
