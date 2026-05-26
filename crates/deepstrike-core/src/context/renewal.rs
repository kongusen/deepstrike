use serde::{Deserialize, Serialize};

use super::config::ContextConfig;
use super::partitions::ContextPartitions;
use super::pressure::PressureMonitor;
use super::token_engine::ContextTokenEngine;

/// Per-criterion verdict carried in HandoffArtifact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContractCheckResult {
    pub criterion_id: String,
    pub passed: bool,
    pub evidence: Option<String>,
}

/// Structured state passed between sprints / agent instances.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffArtifact {
    pub goal: String,
    pub sprint: u32,
    pub progress_summary: String,
    pub open_tasks: Vec<String>,
    pub context_snapshot: serde_json::Value,
    #[serde(default)]
    pub contract_status: Vec<ContractCheckResult>,
    #[serde(default)]
    pub drift_rate_24h: f64,
    #[serde(default)]
    pub blocked_on: Vec<String>,
}

/// Context renewal strategy — when compression isn't enough, start a fresh
/// context while preserving essential state.
///
/// All numeric limits come from `ContextConfig` ratios — no hardcoded
/// message counts or byte limits.
pub struct RenewalPolicy {
    pub renewal_threshold: f64,
    /// Fraction of max_tokens worth of history tokens to carry over.
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
        monitor.pressure(partitions, engine) > self.renewal_threshold
    }

    /// Perform renewal: preserve system + memory + working + task_state,
    /// carry over recent history up to `carryover_ratio * max_tokens` tokens.
    /// The `skill` partition is left empty — the caller re-runs skill selection.
    pub fn renew(
        &self,
        partitions: &ContextPartitions,
        goal: &str,
        sprint: u32,
        max_tokens: u32,
    ) -> (ContextPartitions, HandoffArtifact) {
        let config = ContextConfig {
            carryover_ratio: self.carryover_ratio,
            renewal_threshold: self.renewal_threshold,
            ..Default::default()
        };
        let mut renewed = ContextPartitions::new(&config);

        for msg in &partitions.system.messages {
            renewed
                .system
                .push(msg.clone(), msg.token_count.unwrap_or(0));
        }
        for msg in &partitions.memory.messages {
            renewed
                .memory
                .push(msg.clone(), msg.token_count.unwrap_or(0));
        }

        // skill: left empty — caller re-selects for new sprint goal.

        renewed.working = partitions.working.clone();
        renewed.dashboard = partitions.dashboard.clone();

        // task_state: carry goal + criteria + open steps; clear scratchpad.
        renewed.task_state = partitions.task_state.clone();
        renewed.task_state.scratchpad.clear();

        // Carry history in reverse until carryover token budget is exhausted.
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

        let artifact = HandoffArtifact {
            goal: goal.to_string(),
            sprint,
            progress_summary: partitions.task_state.progress.clone(),
            open_tasks: partitions.task_state.open_steps(),
            context_snapshot: serde_json::json!({
                "history_len": partitions.history.messages.len(),
                "memory_len":  partitions.memory.messages.len(),
            }),
            contract_status: Vec::new(),
            drift_rate_24h: 0.0,
            blocked_on: partitions.task_state.blocked_on.clone(),
        };

        (renewed, artifact)
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
        let cfg = ContextConfig {
            carryover_ratio,
            ..Default::default()
        };
        RenewalPolicy::from_config(&cfg)
    }

    #[test]
    fn renewal_preserves_system_and_memory() {
        let cfg = ContextConfig::default();
        let mut ctx = ContextPartitions::new(&cfg);
        ctx.system.push(Message::system("rules"), 10);
        ctx.memory.push(Message::user("memory"), 20);
        let (renewed, _) = make_policy(0.05).renew(&ctx, "goal", 0, 1_000);
        assert_eq!(renewed.system.len(), 1);
        assert_eq!(renewed.memory.len(), 1);
    }

    #[test]
    fn renewal_clears_skill() {
        let cfg = ContextConfig::default();
        let mut ctx = ContextPartitions::new(&cfg);
        ctx.skill.push(Message::user("skill"), 15);
        let (renewed, _) = make_policy(0.05).renew(&ctx, "goal", 0, 1_000);
        assert_eq!(renewed.skill.len(), 0);
    }

    #[test]
    fn carryover_respects_token_budget() {
        let cfg = ContextConfig::default();
        let mut ctx = ContextPartitions::new(&cfg);
        // 10 messages × 100 tokens = 1000 tokens total
        for i in 0..10 {
            ctx.history.push(Message::user(format!("msg {i}")), 100);
        }
        // carryover_ratio=0.05 on max_tokens=1000 → budget = 50 tokens → ≤ 1 message
        let (renewed, _) = make_policy(0.05).renew(&ctx, "goal", 0, 1_000);
        assert!(
            renewed.history.token_count <= 100,
            "carried: {}",
            renewed.history.token_count
        );
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
        let (renewed, artifact) = make_policy(0.05).renew(&ctx, "build", 0, 1_000);
        assert_eq!(renewed.task_state.goal, "build");
        assert!(renewed.task_state.scratchpad.is_empty());
        assert_eq!(artifact.goal, "build");
    }

    #[test]
    fn artifact_open_tasks_from_task_state() {
        use crate::context::task_state::PlanStep;
        let cfg = ContextConfig::default();
        let mut ctx = ContextPartitions::new(&cfg);
        ctx.task_state = TaskState {
            goal: "g".to_string(),
            plan: vec![
                PlanStep {
                    label: "done step".to_string(),
                    done: true,
                },
                PlanStep {
                    label: "open step".to_string(),
                    done: false,
                },
            ],
            ..Default::default()
        };
        let (_, artifact) = make_policy(0.05).renew(&ctx, "g", 0, 1_000);
        assert_eq!(artifact.open_tasks, vec!["open step"]);
    }
}
