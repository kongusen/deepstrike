use std::cmp::Ordering;
use std::collections::BinaryHeap;

use crate::types::signal::{RuntimeSignal, Urgency};

/// Wrapper for priority ordering: higher urgency first, then older timestamp first.
struct PrioritizedSignal {
    urgency: Urgency,
    timestamp_ms: u64,
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

impl SignalQueue {
    pub(super) fn new(max_size: usize) -> Self {
        Self {
            heap: BinaryHeap::new(),
            max_size,
        }
    }

    /// Returns false if the queue is full (signal is dropped).
    pub(super) fn push(&mut self, signal: RuntimeSignal) -> bool {
        if self.heap.len() >= self.max_size {
            return false;
        }
        let urgency = signal.urgency;
        let timestamp_ms = signal.timestamp_ms;
        self.heap.push(PrioritizedSignal {
            urgency,
            timestamp_ms,
            signal,
        });
        true
    }

    pub(super) fn pop(&mut self) -> Option<RuntimeSignal> {
        self.heap.pop().map(|ps| ps.signal)
    }

    pub(super) fn len(&self) -> usize {
        self.heap.len()
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
}
