/// Deterministic DAG scheduling policy. All weights are non-negative; setting every
/// weight to zero reduces ordering to FIFO with node-id tie-breaking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchedulerPolicyConfig {
    pub critical_path_weight: i64,
    pub fanout_weight: i64,
    pub age_weight: i64,
    pub token_cost_weight: i64,
    /// Deadline urgency, process priority, and host-reported pressure are integer-only inputs.
    /// Zero preserves the pre-016-06 ordering when no new factor is configured.
    pub deadline_weight: i64,
    pub process_priority_weight: i64,
    pub resource_pressure_weight: i64,
    pub budget_pressure_weight: i64,
}

impl Default for SchedulerPolicyConfig {
    fn default() -> Self {
        Self {
            critical_path_weight: 1_000_000,
            fanout_weight: 10_000,
            age_weight: 1_000,
            token_cost_weight: 1,
            deadline_weight: 0,
            process_priority_weight: 0,
            resource_pressure_weight: 0,
            budget_pressure_weight: 0,
        }
    }
}

impl SchedulerPolicyConfig {
    pub fn validate(&self) -> Result<(), String> {
        for (name, weight) in [
            ("critical_path_weight", self.critical_path_weight),
            ("fanout_weight", self.fanout_weight),
            ("age_weight", self.age_weight),
            ("token_cost_weight", self.token_cost_weight),
            ("deadline_weight", self.deadline_weight),
            ("process_priority_weight", self.process_priority_weight),
            ("resource_pressure_weight", self.resource_pressure_weight),
            ("budget_pressure_weight", self.budget_pressure_weight),
        ] {
            if !(0..=1_000_000_000).contains(&weight) {
                return Err(format!(
                    "scheduler_policy {name} must be between 0 and 1000000000"
                ));
            }
        }
        Ok(())
    }
}

/// OS Phase-2 unified scheduler budget: turn / token / wall-clock three axes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SchedulerBudget {
    /// Context window size passed to the pressure monitor.
    pub max_tokens: u32,
    /// Maximum tool-call turns before the loop forces a final text response.
    pub max_turns: u32,
    /// Accumulated token budget across all turns.
    pub max_total_tokens: u64,
    /// Optional wall-clock run budget in milliseconds. Evaluated from
    /// `started_at_ms` using accepted envelope time.
    /// `None` means no wall-clock limit (existing behavior).
    pub max_wall_ms: Option<u64>,
}

impl Default for SchedulerBudget {
    fn default() -> Self {
        Self {
            max_tokens: 128_000,
            max_turns: 25,
            max_total_tokens: 1_000_000,
            max_wall_ms: None,
        }
    }
}

impl SchedulerBudget {
    /// Check whether any budget axis is exceeded.
    /// Returns `Some(budget_name)` for the first axis that fires.
    pub fn should_terminate(
        &self,
        turns: u32,
        total_tokens: u64,
        now_ms: Option<u64>,
        started_at_ms: Option<u64>,
    ) -> Option<&'static str> {
        if turns >= self.max_turns {
            return Some("max_turns");
        }
        if total_tokens >= self.max_total_tokens {
            return Some("token_budget");
        }
        if let (Some(limit), Some(now), Some(start)) = (self.max_wall_ms, now_ms, started_at_ms) {
            if now.saturating_sub(start) >= limit {
                return Some("wall_time");
            }
        }
        None
    }
}
