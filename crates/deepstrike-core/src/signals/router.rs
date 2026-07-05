use std::collections::HashSet;

use compact_str::CompactString;

use super::attention::UrgencyBasedPolicy;
use super::queue::SignalQueue;
use crate::types::policy::SignalDisposition;
use crate::types::signal::RuntimeSignal;

/// Signal router: dedup set + urgency-based attention + bounded priority queue.
pub struct SignalRouter {
    seen: HashSet<CompactString>,
    queue: SignalQueue,
    attention: UrgencyBasedPolicy,
}

impl SignalRouter {
    pub fn new(max_queue_size: usize) -> Self {
        Self {
            seen: HashSet::new(),
            queue: SignalQueue::new(max_queue_size),
            attention: UrgencyBasedPolicy,
        }
    }

    /// Ingest a signal. Returns the disposition after dedup + attention evaluation.
    /// `Queue` dispositions are buffered; if the queue is full, returns `Dropped`
    /// so the SDK can apply backpressure or surface the loss to telemetry.
    /// All other dispositions are returned directly to the caller.
    pub fn ingest(&mut self, signal: RuntimeSignal, is_running: bool) -> SignalDisposition {
        if let Some(ref key) = signal.dedupe_key {
            if !self.seen.insert(key.clone()) {
                return SignalDisposition::Ignore;
            }
        }

        let disposition = self.attention.evaluate(&signal, is_running);

        if disposition == SignalDisposition::Queue {
            if !self.queue.push(signal) {
                return SignalDisposition::Dropped;
            }
        }

        disposition
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

        let d1 = router.ingest(sig.clone(), false);
        assert_ne!(d1, SignalDisposition::Ignore);

        let d2 = router.ingest(sig, false);
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

        let d = router.ingest(sig, false);
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

        let d = router.ingest(sig, true);
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

        assert_eq!(router.ingest(s1, false), SignalDisposition::Queue);
        assert_eq!(router.ingest(s2, false), SignalDisposition::Dropped);
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

        router.ingest(sig.clone(), false);
        assert_eq!(router.ingest(sig.clone(), false), SignalDisposition::Ignore);

        router.clear_dedup();
        assert_ne!(router.ingest(sig, false), SignalDisposition::Ignore);
    }
}
