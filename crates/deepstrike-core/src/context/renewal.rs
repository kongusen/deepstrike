use super::config::ContextConfig;
use super::partitions::ContextPartitions;
use super::pressure::PressureMonitor;
use super::token_engine::ContextTokenEngine;

pub struct RenewalPolicy {
    pub renewal_threshold: f64,
    pub carryover_ratio: f64,
}

impl RenewalPolicy {
    pub fn from_config(config: &ContextConfig) -> Self {
        Self {
            renewal_threshold: config.renewal_threshold,
            carryover_ratio: config.carryover_ratio,
        }
    }

    pub fn should_renew(
        &self,
        monitor: &PressureMonitor,
        partitions: &ContextPartitions,
        engine: &ContextTokenEngine,
    ) -> bool {
        monitor.pressure(partitions, engine, None) > self.renewal_threshold
    }

    /// Perform renewal: carry system + knowledge + task_state into new sprint.
    /// History is reset; only the last `carryover_tokens` worth of turns are kept.
    /// Signals are cleared (they are per-turn ephemeral).
    pub fn renew(&self, partitions: &ContextPartitions, max_tokens: u32) -> ContextPartitions {
        let config = ContextConfig {
            carryover_ratio: self.carryover_ratio,
            renewal_threshold: self.renewal_threshold,
            ..Default::default()
        };
        let mut renewed = ContextPartitions::new(&config);

        // Identity and Knowledge slots carry over unchanged.
        for msg in &partitions.system.messages {
            renewed
                .system
                .push(msg.clone(), msg.token_count.unwrap_or(0));
        }
        // Cloned wholesale (not re-pushed message-by-message) so entry identity — keys, pins,
        // pending upserts, boundary-eviction marks — survives renewal; the caller sweeps right
        // after (renewal IS a boundary).
        renewed.knowledge = partitions.knowledge.clone();

        // State: carry task_state (goal/plan/progress), clear scratchpad.
        renewed.task_state = partitions.task_state.clone();
        renewed.task_state.scratchpad.clear();
        // Signals are ephemeral — not carried over.

        // History: carry recent turns up to carryover budget.
        let carryover_budget = config.carryover_tokens(max_tokens);
        let mut remaining = carryover_budget;
        let mut carried: Vec<_> = partitions
            .history
            .messages
            .iter()
            .rev()
            .take_while(|msg| {
                let t = msg.token_count.unwrap_or(0);
                if t <= remaining {
                    remaining = remaining.saturating_sub(t);
                    true
                } else {
                    false
                }
            })
            .cloned()
            .collect();
        carried.reverse();
        for msg in carried {
            let t = msg.token_count.unwrap_or(0);
            renewed.history.push(msg, t);
        }

        renewed
    }
}

impl Default for RenewalPolicy {
    fn default() -> Self {
        Self::from_config(&ContextConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::config::ContextConfig;
    use crate::context::partitions::ContextPartitions;
    use crate::context::task_state::TaskState;
    use crate::types::message::Message;

    fn make_policy(carryover_ratio: f64) -> RenewalPolicy {
        RenewalPolicy::from_config(&ContextConfig {
            carryover_ratio,
            ..Default::default()
        })
    }

    #[test]
    fn renewal_preserves_system_and_knowledge() {
        let cfg = ContextConfig::default();
        let mut ctx = ContextPartitions::new(&cfg);
        ctx.system.push(Message::system("rules"), 10);
        ctx.knowledge.push(Message::system("skill: debug"), 20);
        let renewed = make_policy(0.05).renew(&ctx, 1_000);
        assert_eq!(renewed.system.len(), 1);
        assert_eq!(renewed.knowledge.len(), 1);
    }

    #[test]
    fn renewal_clears_signals() {
        let cfg = ContextConfig::default();
        let mut ctx = ContextPartitions::new(&cfg);
        ctx.signals.push("[ROLLBACK] failed".to_string());
        let renewed = make_policy(0.05).renew(&ctx, 1_000);
        assert!(renewed.signals.is_empty());
    }

    #[test]
    fn carryover_respects_token_budget() {
        let cfg = ContextConfig::default();
        let mut ctx = ContextPartitions::new(&cfg);
        for i in 0..10 {
            ctx.history.push(Message::user(format!("msg {i}")), 100);
        }
        let renewed = make_policy(0.05).renew(&ctx, 1_000);
        assert!(renewed.history.token_count <= 100);
    }

    #[test]
    fn renewal_clears_task_scratchpad_keeps_goal() {
        let cfg = ContextConfig::default();
        let mut ctx = ContextPartitions::new(&cfg);
        ctx.task_state = TaskState {
            goal: "build".to_string(),
            scratchpad: "temp data".to_string(),
            ..Default::default()
        };
        let renewed = make_policy(0.05).renew(&ctx, 1_000);
        assert_eq!(renewed.task_state.goal, "build");
        assert!(renewed.task_state.scratchpad.is_empty());
    }
}
