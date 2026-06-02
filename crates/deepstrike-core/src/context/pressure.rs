use super::config::ContextConfig;
use super::partitions::ContextPartitions;
use super::token_engine::ContextTokenEngine;

/// Action recommended by the pressure monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PressureAction {
    None,
    SnipCompact,
    MicroCompact,
    ContextCollapse,
    AutoCompact,
}

/// Monitors rho = used_tokens / max_tokens and recommends compression actions.
/// All thresholds come from `ContextConfig` — no hardcoded constants.
pub struct PressureMonitor {
    max_tokens: u32,
    config: ContextConfig,
}

impl PressureMonitor {
    pub fn new(max_tokens: u32, config: ContextConfig) -> Self {
        Self { max_tokens, config }
    }

    pub fn max_tokens(&self) -> u32 {
        self.max_tokens
    }

    /// Current pressure rho ∈ [0, +∞).
    /// Uses provider-reported prompt tokens when available; otherwise estimates from partitions.
    pub fn pressure(
        &self,
        partitions: &ContextPartitions,
        engine: &ContextTokenEngine,
        observed_prompt_tokens: Option<u32>,
    ) -> f64 {
        if self.max_tokens == 0 {
            return 0.0;
        }
        match observed_prompt_tokens {
            Some(tokens) => tokens as f64 / self.max_tokens as f64,
            None => partitions.total_tokens(engine) as f64 / self.max_tokens as f64,
        }
    }

    pub fn recommend(&self, rho: f64) -> PressureAction {
        if rho > self.config.auto_threshold {
            PressureAction::AutoCompact
        } else if rho > self.config.collapse_threshold {
            PressureAction::ContextCollapse
        } else if rho > self.config.micro_threshold {
            PressureAction::MicroCompact
        } else if rho > self.config.snip_threshold {
            PressureAction::SnipCompact
        } else {
            PressureAction::None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::config::ContextConfig;
    use crate::context::partitions::ContextPartitions;
    use crate::context::token_engine::ContextTokenEngine;
    use crate::types::message::Message;

    fn engine() -> ContextTokenEngine {
        ContextTokenEngine::char_approx()
    }
    fn config() -> ContextConfig {
        ContextConfig::default()
    }

    #[test]
    fn thresholds_follow_config() {
        let cfg = config();
        let monitor = PressureMonitor::new(100, cfg.clone());
        assert_eq!(monitor.recommend(0.50), PressureAction::None);
        assert_eq!(
            monitor.recommend(cfg.snip_threshold + 0.01),
            PressureAction::SnipCompact
        );
        assert_eq!(
            monitor.recommend(cfg.micro_threshold + 0.01),
            PressureAction::MicroCompact
        );
        assert_eq!(
            monitor.recommend(cfg.collapse_threshold + 0.01),
            PressureAction::ContextCollapse
        );
        assert_eq!(
            monitor.recommend(cfg.auto_threshold + 0.01),
            PressureAction::AutoCompact
        );
    }

    #[test]
    fn custom_thresholds_respected() {
        let cfg = ContextConfig {
            snip_threshold: 0.50,
            ..Default::default()
        };
        let monitor = PressureMonitor::new(100, cfg);
        assert_eq!(monitor.recommend(0.51), PressureAction::SnipCompact);
        assert_eq!(monitor.recommend(0.49), PressureAction::None);
    }

    #[test]
    fn pressure_calculation_uses_engine() {
        let cfg = config();
        let monitor = PressureMonitor::new(1_000, cfg.clone());
        let mut ctx = ContextPartitions::new(&cfg);
        let baseline = ctx.total_tokens(&engine()) as f64;
        ctx.history.push(Message::user("test"), 500);
        let rho = monitor.pressure(&ctx, &engine(), None);
        let expected = (baseline + 500.0) / 1_000.0;
        assert!((rho - expected).abs() < f64::EPSILON);
    }
}
