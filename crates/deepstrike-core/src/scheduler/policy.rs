/// Loop execution and termination policy.
#[derive(Debug, Clone)]
pub struct LoopPolicy {
    /// Context window size passed to PressureMonitor.
    pub max_tokens: u32,
    pub max_turns: u32,
    pub max_total_tokens: u64,
    pub timeout_ms: Option<u64>,
}

impl Default for LoopPolicy {
    fn default() -> Self {
        Self {
            max_tokens: 128_000,
            max_turns: 25,
            max_total_tokens: 1_000_000,
            timeout_ms: None,
        }
    }
}

impl LoopPolicy {
    pub fn should_terminate(&self, turns: u32, total_tokens: u64) -> Option<&'static str> {
        if turns >= self.max_turns {
            return Some("max_turns");
        }
        if total_tokens >= self.max_total_tokens {
            return Some("token_budget");
        }
        None
    }
}
