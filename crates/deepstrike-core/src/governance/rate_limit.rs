use std::collections::{HashMap, VecDeque};

use compact_str::CompactString;

use crate::types::message::ToolCall;
use crate::types::policy::GovernanceVerdict;

/// Rate limit configuration for a tool.
#[derive(Debug, Clone)]
pub struct RateLimit {
    pub max_calls: u32,
    pub window_ms: u64,
}

impl Default for RateLimit {
    fn default() -> Self {
        Self {
            max_calls: 60,
            window_ms: 60_000,
        }
    }
}

/// Sliding-window rate limiter per tool.
pub struct RateLimiter {
    windows: HashMap<CompactString, VecDeque<u64>>,
    limits: HashMap<CompactString, RateLimit>,
    default_limit: RateLimit,
    /// Current timestamp in ms — injected by SDK layer (no I/O in kernel).
    current_time_ms: u64,
}

impl RateLimiter {
    pub fn new(default_limit: RateLimit) -> Self {
        Self {
            windows: HashMap::new(),
            limits: HashMap::new(),
            default_limit,
            current_time_ms: 0,
        }
    }

    pub fn set_limit(&mut self, tool_name: impl Into<CompactString>, limit: RateLimit) {
        self.limits.insert(tool_name.into(), limit);
    }

    /// Must be called before each check to provide current time.
    pub fn set_time(&mut self, now_ms: u64) {
        self.current_time_ms = now_ms;
    }

    pub fn check(&mut self, call: &ToolCall) -> Option<GovernanceVerdict> {
        // current_time_ms defaults to 0; SDK is expected to call set_time() before check.
        // We don't debug_assert here because 0 is a valid monotonic-clock origin.
        let limit = self.limits.get(&call.name).unwrap_or(&self.default_limit);
        let window = self.windows.entry(call.name.clone()).or_default();

        // Evict expired entries
        let cutoff = self.current_time_ms.saturating_sub(limit.window_ms);
        while window.front().is_some_and(|&t| t < cutoff) {
            window.pop_front();
        }

        if window.len() as u32 >= limit.max_calls {
            let oldest = window.front().copied().unwrap_or(self.current_time_ms);
            let retry_after = oldest + limit.window_ms - self.current_time_ms;
            return Some(GovernanceVerdict::RateLimited {
                retry_after_ms: retry_after,
            });
        }

        window.push_back(self.current_time_ms);
        None
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new(RateLimit::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_call(name: &str) -> ToolCall {
        ToolCall {
            id: CompactString::new("c"),
            name: CompactString::new(name),
            arguments: serde_json::Value::Null,
        }
    }

    #[test]
    fn allows_within_limit() {
        let mut rl = RateLimiter::new(RateLimit {
            max_calls: 3,
            window_ms: 1000,
        });
        rl.set_time(100);
        assert!(rl.check(&make_call("foo")).is_none());
        assert!(rl.check(&make_call("foo")).is_none());
        assert!(rl.check(&make_call("foo")).is_none());
        // 4th call should be limited
        assert!(rl.check(&make_call("foo")).is_some());
    }

    #[test]
    fn expires_old_entries() {
        let mut rl = RateLimiter::new(RateLimit {
            max_calls: 1,
            window_ms: 100,
        });
        rl.set_time(0);
        assert!(rl.check(&make_call("bar")).is_none());
        assert!(rl.check(&make_call("bar")).is_some());

        rl.set_time(200); // window expired
        assert!(rl.check(&make_call("bar")).is_none());
    }
}
