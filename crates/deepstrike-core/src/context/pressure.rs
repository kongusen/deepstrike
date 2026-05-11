use super::partitions::ContextPartitions;

/// Action recommended by the pressure monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PressureAction {
    /// No compression needed
    None,
    /// rho > 0.70: truncate long text segments
    SnipCompact,
    /// rho > 0.80: cache tool results by call_id
    MicroCompact,
    /// rho > 0.90: fold inactive regions
    ContextCollapse,
    /// rho > 0.95: full compression pass
    AutoCompact,
}

/// Monitors context pressure rho = used_tokens / max_tokens
/// and recommends compression actions.
pub struct PressureMonitor {
    max_tokens: u32,
}

impl PressureMonitor {
    pub fn new(max_tokens: u32) -> Self {
        Self { max_tokens }
    }

    /// Current pressure value rho in [0.0, +inf).
    pub fn pressure(&self, partitions: &ContextPartitions) -> f64 {
        if self.max_tokens == 0 {
            return 0.0;
        }
        partitions.total_tokens() as f64 / self.max_tokens as f64
    }

    /// Recommend a compression action based on current pressure.
    pub fn recommend(&self, rho: f64) -> PressureAction {
        if rho > 0.95 {
            PressureAction::AutoCompact
        } else if rho > 0.90 {
            PressureAction::ContextCollapse
        } else if rho > 0.80 {
            PressureAction::MicroCompact
        } else if rho > 0.70 {
            PressureAction::SnipCompact
        } else {
            PressureAction::None
        }
    }

    pub fn max_tokens(&self) -> u32 {
        self.max_tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::partitions::ContextPartitions;
    use crate::types::message::Message;

    #[test]
    fn pressure_thresholds() {
        let monitor = PressureMonitor::new(100);
        assert_eq!(monitor.recommend(0.50), PressureAction::None);
        assert_eq!(monitor.recommend(0.71), PressureAction::SnipCompact);
        assert_eq!(monitor.recommend(0.81), PressureAction::MicroCompact);
        assert_eq!(monitor.recommend(0.91), PressureAction::ContextCollapse);
        assert_eq!(monitor.recommend(0.96), PressureAction::AutoCompact);
    }

    #[test]
    fn pressure_calculation() {
        let monitor = PressureMonitor::new(1000);
        let mut ctx = ContextPartitions::new();
        // Capture the empty-context baseline (dashboard has a fixed-field token estimate).
        let baseline = ctx.total_tokens() as f64;
        ctx.history.push(Message::user("test"), 500);
        let rho = monitor.pressure(&ctx);
        let expected = (baseline + 500.0) / 1000.0;
        assert!((rho - expected).abs() < f64::EPSILON);
    }
}
