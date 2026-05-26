use crate::types::policy::{AttentionPolicy, SignalDisposition};
use crate::types::signal::{RuntimeSignal, Urgency};

/// Default attention policy based on signal urgency.
pub struct UrgencyBasedPolicy;

impl AttentionPolicy for UrgencyBasedPolicy {
    fn evaluate(&self, signal: &RuntimeSignal, is_running: bool) -> SignalDisposition {
        match signal.urgency {
            Urgency::Critical => {
                if is_running {
                    SignalDisposition::InterruptNow
                } else {
                    SignalDisposition::Run { priority: 255 }
                }
            }
            Urgency::High => {
                if is_running {
                    SignalDisposition::Interrupt
                } else {
                    SignalDisposition::Run { priority: 100 }
                }
            }
            Urgency::Normal => SignalDisposition::Queue,
            Urgency::Low => SignalDisposition::Observe,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::signal::{SignalSource, SignalType};

    #[test]
    fn critical_signal_interrupts_running() {
        let policy = UrgencyBasedPolicy;
        let signal = RuntimeSignal::new(
            SignalSource::Gateway,
            SignalType::Alert,
            Urgency::Critical,
            "fire",
        );
        assert_eq!(
            policy.evaluate(&signal, true),
            SignalDisposition::InterruptNow
        );
    }

    #[test]
    fn low_signal_observed() {
        let policy = UrgencyBasedPolicy;
        let signal =
            RuntimeSignal::new(SignalSource::Cron, SignalType::Event, Urgency::Low, "tick");
        assert_eq!(policy.evaluate(&signal, false), SignalDisposition::Observe);
    }
}
