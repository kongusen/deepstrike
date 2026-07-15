use std::collections::{HashSet, VecDeque};

use compact_str::CompactString;

use super::attention::UrgencyBasedPolicy;
use super::queue::SignalQueue;
use crate::scheduler::tcb::TaskLifecycle;
use crate::types::policy::SignalDisposition;
use crate::types::signal::RuntimeSignal;

/// Signal router: dedup set + urgency-based attention + bounded priority queue.
pub struct SignalRouter {
    seen: HashSet<CompactString>,
    seen_order: VecDeque<CompactString>,
    dedupe_capacity: usize,
    queue: SignalQueue,
    attention: UrgencyBasedPolicy,
    ttl_ms: Option<u64>,
    deadline_escalation: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SignalRouteOutcome {
    pub disposition: SignalDisposition,
    pub displaced_signal_id: Option<String>,
    pub expired_signal_ids: Vec<String>,
}

impl SignalRouter {
    pub const DEFAULT_DEDUPE_CAPACITY: usize = 256;

    pub fn new(max_queue_size: usize) -> Self {
        Self::with_policy(max_queue_size, None, false)
    }

    pub fn with_policy(
        max_queue_size: usize,
        ttl_ms: Option<u64>,
        deadline_escalation: bool,
    ) -> Self {
        Self {
            seen: HashSet::with_capacity(Self::DEFAULT_DEDUPE_CAPACITY),
            seen_order: VecDeque::with_capacity(Self::DEFAULT_DEDUPE_CAPACITY),
            dedupe_capacity: Self::DEFAULT_DEDUPE_CAPACITY,
            queue: SignalQueue::new(max_queue_size),
            attention: UrgencyBasedPolicy,
            ttl_ms,
            deadline_escalation,
        }
    }

    /// Ingest a signal. Returns the disposition after dedup + attention evaluation.
    /// `Queue` dispositions are buffered; if the queue is full, returns `Dropped`
    /// so the SDK can apply backpressure or surface the loss to telemetry.
    /// All other dispositions are returned directly to the caller.
    pub fn ingest(&mut self, signal: RuntimeSignal, lifecycle: TaskLifecycle) -> SignalDisposition {
        let now_ms = signal.timestamp_ms;
        self.ingest_at(signal, lifecycle, now_ms).disposition
    }

    pub fn ingest_at(
        &mut self,
        mut signal: RuntimeSignal,
        lifecycle: TaskLifecycle,
        now_ms: u64,
    ) -> SignalRouteOutcome {
        let expired_signal_ids = self.expire(now_ms);
        let dedupe_key = signal.dedupe_key.clone();
        if let Some(ref key) = dedupe_key {
            if self.seen.contains(key) {
                return SignalRouteOutcome {
                    disposition: SignalDisposition::Ignore,
                    displaced_signal_id: None,
                    expired_signal_ids,
                };
            }
        }

        let deadline_escalated = self.deadline_escalation
            && signal
                .deadline_ms
                .is_some_and(|deadline_ms| now_ms >= deadline_ms);
        if deadline_escalated {
            signal.urgency = escalate_one_tier(signal.urgency);
        }

        let disposition = self.attention.evaluate(&signal, lifecycle);

        if disposition == SignalDisposition::Queue {
            let admission = self
                .queue
                .admit_with_deadline_state(signal, deadline_escalated);
            for key in &admission.displaced_dedupe_keys {
                self.release_dedupe_key(key);
            }
            let displaced_signal_id = admission
                .displaced
                .as_ref()
                .map(|displaced| displaced.id.to_string());
            if !admission.admitted {
                return SignalRouteOutcome {
                    disposition: SignalDisposition::Dropped,
                    displaced_signal_id: None,
                    expired_signal_ids,
                };
            }
            if let Some(key) = dedupe_key {
                self.commit_dedupe(key);
            }
            return SignalRouteOutcome {
                disposition,
                displaced_signal_id,
                expired_signal_ids,
            };
        }

        if let Some(key) = dedupe_key {
            self.commit_dedupe(key);
        }

        SignalRouteOutcome {
            disposition,
            displaced_signal_id: None,
            expired_signal_ids,
        }
    }

    /// Expire queued signals against the journaled clock before either admission or delivery.
    pub fn expire(&mut self, now_ms: u64) -> Vec<String> {
        let expired = self.queue.expire(now_ms, self.ttl_ms);
        if self.deadline_escalation {
            self.queue.escalate_deadlines(now_ms);
        }
        for (_, dedupe_keys) in &expired {
            for key in dedupe_keys {
                self.release_dedupe_key(key);
            }
        }
        expired
            .into_iter()
            .map(|(signal, _)| signal.id.to_string())
            .collect()
    }

    fn commit_dedupe(&mut self, key: CompactString) {
        if self.seen_order.len() == self.dedupe_capacity {
            if let Some(expired) = self.seen_order.pop_front() {
                self.seen.remove(&expired);
            }
        }
        self.seen.insert(key.clone());
        self.seen_order.push_back(key);
    }

    fn release_dedupe_key(&mut self, key: &CompactString) {
        self.seen.remove(key);
        self.seen_order.retain(|seen_key| seen_key != key);
    }

    /// Pull next queued signal.
    pub fn next(&mut self) -> Option<RuntimeSignal> {
        self.queue.pop()
    }

    /// Number of queued signals.
    pub fn depth(&self) -> usize {
        self.queue.len()
    }

    /// Clear the dedup set (call at session boundaries to prevent unbounded growth).
    pub fn clear_dedup(&mut self) {
        self.seen.clear();
        self.seen_order.clear();
    }

    #[cfg(test)]
    fn dedupe_len(&self) -> usize {
        self.seen.len()
    }
}

fn escalate_one_tier(urgency: crate::types::signal::Urgency) -> crate::types::signal::Urgency {
    use crate::types::signal::Urgency;
    match urgency {
        Urgency::Low => Urgency::Normal,
        Urgency::Normal => Urgency::High,
        Urgency::High | Urgency::Critical => Urgency::Critical,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::tcb::TaskLifecycle;
    use crate::types::signal::{SignalSource, SignalType, Urgency};

    #[test]
    fn deduplicates_signals() {
        let mut router = SignalRouter::new(100);
        let sig = RuntimeSignal::new(
            SignalSource::Cron,
            SignalType::Event,
            Urgency::Normal,
            "tick",
        )
        .with_dedupe("cron-tick-1");

        let d1 = router.ingest(sig.clone(), TaskLifecycle::Running);
        assert_ne!(d1, SignalDisposition::Ignore);

        let d2 = router.ingest(sig, TaskLifecycle::Running);
        assert_eq!(d2, SignalDisposition::Ignore);
    }

    #[test]
    fn normal_signal_queued() {
        let mut router = SignalRouter::new(100);
        let sig = RuntimeSignal::new(
            SignalSource::Cron,
            SignalType::Event,
            Urgency::Normal,
            "job",
        );

        let d = router.ingest(sig, TaskLifecycle::Running);
        assert_eq!(d, SignalDisposition::Queue);
        assert_eq!(router.depth(), 1);
        assert!(router.next().is_some());
    }

    #[test]
    fn interrupt_signals_not_queued() {
        let mut router = SignalRouter::new(100);
        let sig = RuntimeSignal::new(
            SignalSource::Gateway,
            SignalType::Alert,
            Urgency::Critical,
            "fire",
        );

        let d = router.ingest(sig, TaskLifecycle::Running);
        assert_eq!(d, SignalDisposition::InterruptNow);
        assert_eq!(router.depth(), 0);
    }

    #[test]
    fn full_queue_drops_signal() {
        let mut router = SignalRouter::new(1);
        let s1 = RuntimeSignal::new(
            SignalSource::Cron,
            SignalType::Event,
            Urgency::Normal,
            "first",
        );
        let s2 = RuntimeSignal::new(
            SignalSource::Cron,
            SignalType::Event,
            Urgency::Normal,
            "second",
        );

        assert_eq!(
            router.ingest(s1, TaskLifecycle::Running),
            SignalDisposition::Queue
        );
        assert_eq!(
            router.ingest(s2, TaskLifecycle::Running),
            SignalDisposition::Dropped
        );
    }

    #[test]
    fn clear_dedup_allows_reingest() {
        let mut router = SignalRouter::new(100);
        let sig = RuntimeSignal::new(
            SignalSource::Cron,
            SignalType::Event,
            Urgency::Normal,
            "tick",
        )
        .with_dedupe("key-1");

        router.ingest(sig.clone(), TaskLifecycle::Running);
        assert_eq!(
            router.ingest(sig.clone(), TaskLifecycle::Running),
            SignalDisposition::Ignore
        );

        router.clear_dedup();
        assert_ne!(
            router.ingest(sig, TaskLifecycle::Running),
            SignalDisposition::Ignore
        );
    }

    #[test]
    fn dedupe_window_is_bounded_and_expires_oldest_key() {
        let mut router = SignalRouter::new(1);
        for index in 0..=SignalRouter::DEFAULT_DEDUPE_CAPACITY {
            let signal =
                RuntimeSignal::new(SignalSource::Cron, SignalType::Event, Urgency::Low, "tick")
                    .with_dedupe(format!("key-{index}"));
            assert_ne!(
                router.ingest(signal, TaskLifecycle::Running),
                SignalDisposition::Ignore
            );
        }

        assert_eq!(router.dedupe_len(), SignalRouter::DEFAULT_DEDUPE_CAPACITY);
        let expired =
            RuntimeSignal::new(SignalSource::Cron, SignalType::Event, Urgency::Low, "tick")
                .with_dedupe("key-0");
        assert_ne!(
            router.ingest(expired, TaskLifecycle::Running),
            SignalDisposition::Ignore
        );
    }

    #[test]
    fn dropped_signal_does_not_commit_its_dedupe_key() {
        let mut router = SignalRouter::new(1);
        let admitted = RuntimeSignal::new(
            SignalSource::Cron,
            SignalType::Event,
            Urgency::Normal,
            "admitted",
        );
        let retryable = RuntimeSignal::new(
            SignalSource::Cron,
            SignalType::Event,
            Urgency::Normal,
            "retryable",
        )
        .with_dedupe("retryable-key");

        assert_eq!(
            router.ingest(admitted, TaskLifecycle::Running),
            SignalDisposition::Queue
        );
        assert_eq!(
            router.ingest(retryable.clone(), TaskLifecycle::Running),
            SignalDisposition::Dropped
        );
        assert!(router.next().is_some());
        assert_eq!(
            router.ingest(retryable, TaskLifecycle::Running),
            SignalDisposition::Queue
        );
    }

    #[test]
    fn ttl_cleanup_precedes_urgency_displacement() {
        let mut router = SignalRouter::with_policy(1, Some(10), false);
        let fresh = RuntimeSignal::new(
            SignalSource::Cron,
            SignalType::Event,
            Urgency::Normal,
            "fresh",
        )
        .with_timestamp(30);

        // Terminal forces the critical signal into the pending queue so TTL can be tested.
        let stale_queued = RuntimeSignal::new(
            SignalSource::Gateway,
            SignalType::Alert,
            Urgency::Critical,
            "stale queued",
        )
        .with_timestamp(10);
        assert_eq!(
            router
                .ingest_at(
                    stale_queued,
                    TaskLifecycle::Done(crate::types::result::TerminationReason::Completed),
                    10,
                )
                .disposition,
            SignalDisposition::Queue
        );

        let outcome = router.ingest_at(fresh, TaskLifecycle::Running, 30);
        assert_eq!(outcome.disposition, SignalDisposition::Queue);
        assert_eq!(outcome.expired_signal_ids.len(), 1);
        assert!(outcome.displaced_signal_id.is_none());
    }

    #[test]
    fn expiration_releases_dedupe_key_before_redelivery_is_checked() {
        let mut router = SignalRouter::with_policy(1, Some(10), false);
        let first = RuntimeSignal::new(
            SignalSource::Cron,
            SignalType::Event,
            Urgency::Normal,
            "first lease",
        )
        .with_timestamp(10)
        .with_dedupe("leased-work");
        assert_eq!(
            router
                .ingest_at(first, TaskLifecycle::Running, 10)
                .disposition,
            SignalDisposition::Queue
        );

        let redelivery = RuntimeSignal::new(
            SignalSource::Cron,
            SignalType::Event,
            Urgency::Normal,
            "redelivery",
        )
        .with_timestamp(30)
        .with_dedupe("leased-work");
        let outcome = router.ingest_at(redelivery, TaskLifecycle::Running, 30);

        assert_eq!(outcome.disposition, SignalDisposition::Queue);
        assert_eq!(outcome.expired_signal_ids.len(), 1);
    }

    #[test]
    fn displacement_releases_the_evicted_signals_dedupe_key() {
        let mut router = SignalRouter::new(2);
        let old = RuntimeSignal::new(
            SignalSource::Cron,
            SignalType::Event,
            Urgency::Low,
            "old low",
        )
        .with_timestamp(1)
        .with_dedupe("old-low");
        let newest = RuntimeSignal::new(
            SignalSource::Cron,
            SignalType::Event,
            Urgency::Low,
            "new low",
        )
        .with_timestamp(2)
        .with_dedupe("new-low");
        let newest_id = newest.id.to_string();
        let critical = RuntimeSignal::new(
            SignalSource::Gateway,
            SignalType::Alert,
            Urgency::Critical,
            "critical",
        )
        .with_timestamp(3);
        let terminal = TaskLifecycle::Done(crate::types::result::TerminationReason::Completed);
        assert_eq!(router.ingest(old, terminal), SignalDisposition::Queue);
        assert_eq!(router.ingest(newest, terminal), SignalDisposition::Queue);

        let outcome = router.ingest_at(critical, terminal, 3);
        assert_eq!(outcome.disposition, SignalDisposition::Queue);
        assert_eq!(
            outcome.displaced_signal_id.as_deref(),
            Some(newest_id.as_str())
        );

        let redelivery = RuntimeSignal::new(
            SignalSource::Cron,
            SignalType::Event,
            Urgency::Low,
            "new low redelivery",
        )
        .with_timestamp(4)
        .with_dedupe("new-low");
        assert_ne!(
            router.ingest(redelivery, TaskLifecycle::Running),
            SignalDisposition::Ignore
        );
    }

    #[test]
    fn due_deadline_escalates_exactly_one_urgency_tier_when_enabled() {
        let mut router = SignalRouter::with_policy(4, None, true);
        let due = RuntimeSignal::new(
            SignalSource::Gateway,
            SignalType::Event,
            Urgency::Normal,
            "due work",
        )
        .with_timestamp(10)
        .with_deadline(20);

        let outcome = router.ingest_at(due, TaskLifecycle::Running, 20);

        assert_eq!(outcome.disposition, SignalDisposition::Interrupt);
        assert_eq!(router.depth(), 0);
    }

    #[test]
    fn deadline_is_inert_when_escalation_policy_is_disabled() {
        let mut router = SignalRouter::with_policy(4, None, false);
        let due = RuntimeSignal::new(
            SignalSource::Gateway,
            SignalType::Event,
            Urgency::Normal,
            "due work",
        )
        .with_timestamp(10)
        .with_deadline(20);

        let outcome = router.ingest_at(due, TaskLifecycle::Running, 20);

        assert_eq!(outcome.disposition, SignalDisposition::Queue);
        assert_eq!(router.next().unwrap().urgency, Urgency::Normal);
    }

    #[test]
    fn queued_signals_coalesce_without_consuming_capacity_or_dedupe_semantics() {
        let mut router = SignalRouter::with_policy(1, None, false);
        let first = RuntimeSignal::new(
            SignalSource::Cron,
            SignalType::Event,
            Urgency::Normal,
            "first sample",
        )
        .with_timestamp(10)
        .with_deadline(200)
        .with_coalesce("telemetry")
        .with_dedupe("event-1");
        let second = RuntimeSignal::new(
            SignalSource::Cron,
            SignalType::Event,
            Urgency::Normal,
            "second sample",
        )
        .with_timestamp(20)
        .with_deadline(100)
        .with_coalesce("telemetry");

        assert_eq!(
            router.ingest(first, TaskLifecycle::Running),
            SignalDisposition::Queue
        );
        assert_eq!(
            router.ingest(second, TaskLifecycle::Running),
            SignalDisposition::Queue
        );
        assert_eq!(router.depth(), 1);

        let duplicate = RuntimeSignal::new(
            SignalSource::Cron,
            SignalType::Event,
            Urgency::Normal,
            "dedupe still wins",
        )
        .with_timestamp(30)
        .with_coalesce("telemetry")
        .with_dedupe("event-1");
        assert_eq!(
            router.ingest(duplicate, TaskLifecycle::Running),
            SignalDisposition::Ignore
        );

        let merged = router.next().unwrap();
        assert_eq!(merged.summary.as_str(), "first sample");
        assert_eq!(merged.timestamp_ms, 10);
        assert_eq!(merged.deadline_ms, Some(100));
        assert_eq!(merged.coalesced_count, 2);
    }

    #[test]
    fn expiration_releases_every_dedupe_key_merged_into_a_coalesced_entry() {
        let mut router = SignalRouter::with_policy(1, Some(10), false);
        let first = RuntimeSignal::new(
            SignalSource::Cron,
            SignalType::Event,
            Urgency::Normal,
            "first",
        )
        .with_timestamp(10)
        .with_coalesce("batch")
        .with_dedupe("event-1");
        let second = RuntimeSignal::new(
            SignalSource::Cron,
            SignalType::Event,
            Urgency::Normal,
            "second",
        )
        .with_timestamp(11)
        .with_coalesce("batch")
        .with_dedupe("event-2");

        assert_eq!(
            router.ingest(first, TaskLifecycle::Running),
            SignalDisposition::Queue
        );
        assert_eq!(
            router.ingest(second, TaskLifecycle::Running),
            SignalDisposition::Queue
        );
        assert_eq!(router.expire(30).len(), 1);

        for key in ["event-1", "event-2"] {
            let redelivery = RuntimeSignal::new(
                SignalSource::Cron,
                SignalType::Event,
                Urgency::Normal,
                "redelivery",
            )
            .with_timestamp(30)
            .with_dedupe(key);
            assert_eq!(
                router.ingest(redelivery, TaskLifecycle::Running),
                SignalDisposition::Queue
            );
            router.next();
        }
    }

    #[test]
    fn runtime_signal_wire_rejects_removed_topic_field() {
        let encoded = serde_json::json!({
            "id": uuid::Uuid::nil(),
            "source": "custom",
            "signal_type": "event",
            "urgency": "normal",
            "summary": "signal",
            "payload": null,
            "topic": "legacy-field",
            "timestamp_ms": 1
        });

        assert!(serde_json::from_value::<RuntimeSignal>(encoded).is_err());
    }
}
