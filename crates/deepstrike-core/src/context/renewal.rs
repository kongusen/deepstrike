use super::partitions::ContextPartitions;
use super::pressure::PressureMonitor;

#[derive(Debug, Clone)]
pub struct HandoffArtifact {
    pub goal: String,
    pub sprint: u32,
    pub progress_summary: String,
    pub open_tasks: Vec<String>,
    pub context_snapshot: serde_json::Value,
}

/// Context renewal strategy — when compression isn't enough,
/// start a fresh context while preserving essential state.
pub struct RenewalPolicy {
    /// Pressure threshold above which renewal is triggered.
    pub renewal_threshold: f64,
    /// Maximum messages to carry over from history.
    pub max_carryover: usize,
}

impl Default for RenewalPolicy {
    fn default() -> Self {
        Self {
            renewal_threshold: 0.98,
            max_carryover: 5,
        }
    }
}

impl RenewalPolicy {
    pub fn should_renew(&self, monitor: &PressureMonitor, partitions: &ContextPartitions) -> bool {
        monitor.pressure(partitions) > self.renewal_threshold
    }

    /// Perform renewal: preserve system + memory + working dashboard,
    /// carry over recent history. The `skill` partition is intentionally
    /// left empty — the caller is expected to re-run `plan_skill_selection`
    /// for the new sprint's goal so skills can be swapped in/out.
    /// Returns renewed partitions and a HandoffArtifact.
    pub fn renew(
        &self,
        partitions: &ContextPartitions,
        goal: &str,
        sprint: u32,
    ) -> (ContextPartitions, HandoffArtifact) {
        let mut renewed = ContextPartitions::new();

        // Preserve system in full
        for msg in &partitions.system.messages {
            let tokens = msg.token_count.unwrap_or(0);
            renewed.system.push(msg.clone(), tokens);
        }

        // Preserve memory in full
        for msg in &partitions.memory.messages {
            let tokens = msg.token_count.unwrap_or(0);
            renewed.memory.push(msg.clone(), tokens);
        }

        // skill: deliberately left empty.
        // The new sprint may have a different goal — old skill selections may
        // no longer be relevant. ContextManager re-runs selection after renew.

        // Restore working partition and dashboard from snapshot
        renewed.working = partitions.working.clone();
        renewed.dashboard = partitions.dashboard.clone();

        // Carry over last N history messages
        let history = &partitions.history.messages;
        let start = history.len().saturating_sub(self.max_carryover);
        for msg in &history[start..] {
            let tokens = msg.token_count.unwrap_or(0);
            renewed.history.push(msg.clone(), tokens);
        }

        let artifact = HandoffArtifact {
            goal: goal.to_string(),
            sprint,
            progress_summary: partitions.dashboard.goal_progress.clone(),
            open_tasks: partitions.dashboard.plan.clone(),
            context_snapshot: serde_json::json!({
                "history_len": partitions.history.messages.len(),
                "memory_len": partitions.memory.messages.len(),
            }),
        };

        (renewed, artifact)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::message::Message;

    #[test]
    fn renewal_preserves_system_memory_clears_skill_and_carries_recent_history() {
        let mut ctx = ContextPartitions::new();
        ctx.system.push(Message::system("rules"), 10);
        ctx.memory.push(Message::user("memory entry"), 20);
        ctx.skill.push(Message::user("skill entry"), 15);
        for i in 0..20 {
            ctx.history.push(Message::user(format!("msg {i}")), 50);
        }

        let policy = RenewalPolicy { renewal_threshold: 0.98, max_carryover: 3 };
        let (renewed, artifact) = policy.renew(&ctx, "test goal", 1);

        assert_eq!(renewed.system.len(), 1);
        assert_eq!(renewed.memory.len(), 1);
        // skill must be empty after renewal — caller re-selects for new sprint
        assert_eq!(renewed.skill.len(), 0);
        assert_eq!(renewed.history.len(), 3);
        assert_eq!(artifact.goal, "test goal");
        assert_eq!(artifact.sprint, 1);
    }
}

