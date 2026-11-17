/// Host-counted provider request overhead and reserves deducted before context rendering.
/// These values are input facts, so configuring them through the kernel journal makes replay use
/// the same hard prompt allowance as the original run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptBudgetConfig {
    pub prompt_overhead_tokens: u32,
    pub output_reserve_tokens: u32,
    pub safety_margin_tokens: u32,
}

impl PromptBudgetConfig {
    pub fn reserved_tokens(self) -> u32 {
        self.prompt_overhead_tokens
            .saturating_add(self.output_reserve_tokens)
            .saturating_add(self.safety_margin_tokens)
    }
}

/// All compression and context management parameters expressed as fractions of
/// `max_tokens`. This is the single control surface for the compression pipeline:
/// changing `max_tokens` (e.g. switching model) rescales every derived limit
/// automatically with no other configuration change required.
///
/// Invariant: micro < collapse < auto < renewal (strictly increasing). Kernel-distillation plan,
/// stage 2 (DEC-13): the separate snip tier and idle time-decay were measured and deleted; a micro
/// compaction caps oversized messages first.
#[derive(Debug, Clone)]
pub struct ContextConfig {
    // ── Pressure thresholds ─────────────────────────────────────────────────
    pub micro_threshold: f64,
    pub collapse_threshold: f64,
    pub auto_threshold: f64,
    pub renewal_threshold: f64,

    // ── Post-compression target ──────────────────────────────────────────────
    /// Target rho after any compression pass. Must be < micro_threshold.
    pub target_after_compress: f64,

    // ── Per-compactor ratios ─────────────────────────────────────────────────
    /// Fraction of max_tokens any single message may occupy after a micro compaction's snip stage.
    /// Messages smaller than this are never touched.
    pub snip_per_msg_ratio: f64,

    // ── Renewal ──────────────────────────────────────────────────────────────
    /// Fraction of max_tokens worth of history tokens to carry across renewal.
    /// Renewal stops carrying messages once this token budget is exhausted.
    pub carryover_ratio: f64,

    // ── Recovery / repair ────────────────────────────────────────────────────
    /// Maximum fraction of max_tokens a recovery/replay payload may occupy.
    pub recovery_content_ratio: f64,

    /// The most recent history units (a tool call stays with its results) that render never
    /// drops and compaction never archives. The wire's `preserve_recent_turns` sets it. Default 2.
    pub preserve_recent_units: usize,

    // ── Noise reduction ──────────────────────────────────────────────────────

    /// Number of recent tool results to preserve during Micro-Compact.
    pub preserved_tool_results: usize,

    // ── K2: knowledge budget ─────────────────────────────────────────────
    /// Max share of `max_tokens` the knowledge partition may occupy. Exceeding it emits a
    /// `KnowledgeBudgetExceeded` observation (once per cache generation) and marks the OLDEST
    /// unpinned, non-skill entries for eviction at the next compaction/renewal boundary until the
    /// projected usage fits. Pinned entries and `skill:`-keyed pins are never budget-evicted
    /// (skills are governed by deactivation/lease, not the budget). `0.0` disables (no cap).
    /// Default: 0.25.
    pub knowledge_budget_ratio: f64,

    // ── Provider delegation ──────────────────────────────────────────────────
    /// The provider clears old tool results natively, so the kernel runs none of its own
    /// tool-result tiers: idle time-decay, snip, micro-compact and the collapsed-preview projection.
    pub delegate_tool_result_clearing: bool,
    /// The provider compacts natively, so the kernel runs neither context collapse nor auto-compact.
    /// Renewal is never delegated: it is decided on the full local history and stays the safety net.
    pub delegate_compaction: bool,

    // ── Notes (plan task 9.1) ───────────────────────────────────────────────
    /// Tokens the model's notes may hold in the state turn. `0` turns the `note` tool off.
    pub notes_budget_tokens: u32,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            micro_threshold: 0.70,
            collapse_threshold: 0.90,
            auto_threshold: 0.95,
            renewal_threshold: 0.98,
            target_after_compress: 0.65,
            snip_per_msg_ratio: 0.05,
            carryover_ratio: 0.05,
            recovery_content_ratio: 0.25,
            preserve_recent_units: 2,
            preserved_tool_results: 5,
            knowledge_budget_ratio: 0.25,
            delegate_tool_result_clearing: false,
            delegate_compaction: false,
            notes_budget_tokens: super::notes::DEFAULT_NOTES_BUDGET_TOKENS,
        }
    }
}

impl ContextConfig {
    /// Token budget to target after a compression pass.
    pub fn target_tokens(&self, max_tokens: u32) -> u32 {
        (max_tokens as f64 * self.target_after_compress) as u32
    }

    /// Per-message token cap used by SnipCompact.
    /// Floor of 50 ensures very small context windows still get useful output.
    pub fn snip_per_msg_tokens(&self, max_tokens: u32) -> u32 {
        ((max_tokens as f64 * self.snip_per_msg_ratio) as u32).max(50)
    }

    /// Token budget for history carryover across renewal.
    pub fn carryover_tokens(&self, max_tokens: u32) -> u32 {
        ((max_tokens as f64 * self.carryover_ratio) as u32).max(100)
    }

    /// Token cap for a single recovery/replay payload.
    pub fn recovery_content_tokens(&self, max_tokens: u32) -> u32 {
        (max_tokens as f64 * self.recovery_content_ratio) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_thresholds_strictly_increasing() {
        let c = ContextConfig::default();
        assert!(c.micro_threshold < c.collapse_threshold);
        assert!(c.collapse_threshold < c.auto_threshold);
        assert!(c.auto_threshold < c.renewal_threshold);
    }

    #[test]
    fn target_after_compress_below_micro_threshold() {
        let c = ContextConfig::default();
        assert!(c.target_after_compress < c.micro_threshold);
    }

    #[test]
    fn derived_limits_scale_with_max_tokens() {
        let c = ContextConfig::default();
        let small = 8_000u32;
        let large = 200_000u32;
        let ratio = c.snip_per_msg_tokens(large) as f64 / c.snip_per_msg_tokens(small) as f64;
        assert!((ratio - 25.0).abs() < 1.0, "expected ~25×, got {ratio}");
    }

    #[test]
    fn small_context_window_has_floor() {
        let c = ContextConfig::default();
        assert!(c.snip_per_msg_tokens(100) >= 50);
        assert!(c.carryover_tokens(100) >= 100);
    }
}
