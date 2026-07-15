pub const SCHEDULER_POLICY_VERSION: u32 = 1;

/// Versioned deterministic DAG scheduling policy. All weights are non-negative; setting every
/// weight to zero reduces ordering to FIFO with node-id tie-breaking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchedulerPolicyConfig {
    pub version: u32,
    pub critical_path_weight: i64,
    pub fanout_weight: i64,
    pub age_weight: i64,
    pub token_cost_weight: i64,
}

impl Default for SchedulerPolicyConfig {
    fn default() -> Self {
        Self {
            version: SCHEDULER_POLICY_VERSION,
            critical_path_weight: 1_000_000,
            fanout_weight: 10_000,
            age_weight: 1_000,
            token_cost_weight: 1,
        }
    }
}

impl SchedulerPolicyConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != SCHEDULER_POLICY_VERSION {
            return Err(format!(
                "scheduler_policy version must be {SCHEDULER_POLICY_VERSION}"
            ));
        }
        for (name, weight) in [
            ("critical_path_weight", self.critical_path_weight),
            ("fanout_weight", self.fanout_weight),
            ("age_weight", self.age_weight),
            ("token_cost_weight", self.token_cost_weight),
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
    /// `started_at_ms` using the `now_ms` timestamps fed via `ProviderResult`.
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
