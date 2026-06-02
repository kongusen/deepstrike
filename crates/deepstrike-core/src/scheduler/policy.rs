/// OS Phase-2 unified scheduler budget: turn / token / wall-clock three axes.
#[derive(Debug, Clone)]
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

// COMPAT(sched-policy-rename): LoopPolicy was the original name. All SDK/test
// code that constructs `LoopPolicy { .. }` keeps compiling without changes.
// Remove this alias once all callers use SchedulerBudget directly.
pub type LoopPolicy = SchedulerBudget;
