use crate::scheduler::tcb::TaskLifecycle;
use crate::types::policy::SignalDisposition;
use crate::types::signal::{RuntimeSignal, Urgency};

/// Default attention policy based on signal urgency.
pub struct UrgencyBasedPolicy;

impl UrgencyBasedPolicy {
    pub fn evaluate(&self, signal: &RuntimeSignal, lifecycle: TaskLifecycle) -> SignalDisposition {
        if lifecycle.is_terminal() {
            return SignalDisposition::Queue;
        }

        match signal.urgency {
            Urgency::Critical => {
                if matches!(lifecycle, TaskLifecycle::Running | TaskLifecycle::Suspended) {
                    SignalDisposition::InterruptNow
                } else {
                    SignalDisposition::Run
                }
            }
            Urgency::High => {
                if matches!(lifecycle, TaskLifecycle::Running | TaskLifecycle::Suspended) {
                    SignalDisposition::Interrupt
                } else {
                    SignalDisposition::Run
                }
            }
            Urgency::Normal => {
                if matches!(lifecycle, TaskLifecycle::Ready) {
                    SignalDisposition::Run
                } else {
                    SignalDisposition::Queue
                }
            }
            Urgency::Low => SignalDisposition::Observe,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scheduler::tcb::TaskLifecycle;
    use crate::types::result::TerminationReason;
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
            policy.evaluate(&signal, TaskLifecycle::Running),
            SignalDisposition::InterruptNow
        );
    }

    #[test]
    fn low_signal_observed() {
        let policy = UrgencyBasedPolicy;
        let signal =
            RuntimeSignal::new(SignalSource::Cron, SignalType::Event, Urgency::Low, "tick");
        assert_eq!(
            policy.evaluate(&signal, TaskLifecycle::Ready),
            SignalDisposition::Observe
        );
    }

    #[test]
    fn normal_signal_runs_an_idle_nonterminal_task() {
        let policy = UrgencyBasedPolicy;
        let signal = RuntimeSignal::new(
            SignalSource::Cron,
            SignalType::Job,
            Urgency::Normal,
            "work ready",
        );

        assert_eq!(
            policy.evaluate(&signal, TaskLifecycle::Ready),
            SignalDisposition::Run
        );
        assert_eq!(
            policy.evaluate(&signal, TaskLifecycle::Running),
            SignalDisposition::Queue
        );
    }

    #[test]
    fn terminal_task_queues_signal_for_host_decision() {
        let policy = UrgencyBasedPolicy;
        let signal = RuntimeSignal::new(
            SignalSource::Gateway,
            SignalType::Alert,
            Urgency::Critical,
            "late signal",
        );

        assert_eq!(
            policy.evaluate(&signal, TaskLifecycle::Done(TerminationReason::Completed),),
            SignalDisposition::Queue
        );
    }
}
