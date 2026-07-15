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
/// Invariant: snip < micro < collapse < auto < renewal (strictly increasing).
#[derive(Debug, Clone)]
pub struct ContextConfig {
    // ── Pressure thresholds ─────────────────────────────────────────────────
    pub snip_threshold: f64,
    pub micro_threshold: f64,
    pub collapse_threshold: f64,
    pub auto_threshold: f64,
    pub renewal_threshold: f64,

    // ── Post-compression target ──────────────────────────────────────────────
    /// Target rho after any compression pass. Must be < snip_threshold.
    pub target_after_compress: f64,

    // ── Per-compactor ratios ─────────────────────────────────────────────────
    /// Fraction of max_tokens any single message may occupy after SnipCompact.
    /// Messages smaller than this are never touched.
    pub snip_per_msg_ratio: f64,

    // ── Renewal ──────────────────────────────────────────────────────────────
    /// Fraction of max_tokens worth of history tokens to carry across renewal.
    /// Renewal stops carrying messages once this token budget is exhausted.
    pub carryover_ratio: f64,

    // ── Recovery / repair ────────────────────────────────────────────────────
    /// Maximum fraction of max_tokens a recovery/replay payload may occupy.
    pub recovery_content_ratio: f64,

    /// Recent conversational transactions always kept during render.
    pub preserve_recent_units: usize,

    /// Number of most-recent turns (user+assistant pairs) preserved by
    /// CollapseCompactor and AutoCompactor. Each turn = 2 messages, so
    /// the actual message count kept is `preserve_recent_turns * 2`.
    /// Must be ≥ 1. Default: 2 (= 4 messages).
    pub preserve_recent_turns: usize,

    // ── Noise reduction ──────────────────────────────────────────────────────
    /// Use verbose internal control notes (e.g. "[SYSTEM] Transaction rollback: …").
    /// Defaults to false; uses concise natural-language notes instead.
    pub verbose_control_notes: bool,

    /// Collapse the *narration* text of OLD assistant turns (those past the
    /// `preserve_recent_units` window that also carry tool calls) to a short stub at render time —
    /// non-destructively (the full text stays in `partitions.history`). The model's user-facing
    /// preamble ("好的，我来…先X") has no value once it has aged out of the recent window, but
    /// re-feeding it verbatim every turn primes the model to keep emitting the same preamble (an
    /// in-context repetition trap). Tool calls and pairing are untouched; current progress lives in
    /// the TASK STATE turn. Defaults to true.
    pub collapse_assistant_narration: bool,

    // ── Layer 3: Time-based decay ───────────────────────────────────────
    /// Minutes of inactivity before triggering Micro-Compact (Layer 3).
    /// Defaults to 60 minutes — assumes Prompt Cache has expired by then.
    pub micro_compact_idle_minutes: u32,

    /// Number of recent tool results to preserve during Micro-Compact.
    pub preserved_tool_results: usize,

    // ── Layer 5: Auto-Compact buffer ─────────────────────────────────────
    /// Buffer size for Auto-Compact trigger (Layer 5).
    /// Trigger threshold = max_tokens - autocompact_buffer.
    /// Defaults to 13K tokens (p99.99 of summarizer output length + safety margin).
    pub autocompact_buffer: u32,

    // ── Layer 1: Large-result spool ──────────────────────────────────────
    /// Byte size above which a single tool result is spooled (Layer 1): the kernel
    /// keeps only a preview in context and emits a `SpoolLargeResult` host effect.
    /// Default: 50 KiB. `0` disables spooling.
    pub spool_threshold_bytes: u32,

    /// Preview byte budget kept in context when a tool result is spooled. Default: 2 KiB.
    pub spool_preview_bytes: u32,

    // ── K2: knowledge budget ─────────────────────────────────────────────
    /// Max share of `max_tokens` the knowledge partition may occupy. Exceeding it emits a
    /// `KnowledgeBudgetExceeded` observation (once per cache generation) and marks the OLDEST
    /// unpinned, non-skill entries for eviction at the next compaction/renewal boundary until the
    /// projected usage fits. Pinned entries and `skill:`-keyed pins are never budget-evicted
    /// (skills are governed by deactivation/lease, not the budget). `0.0` disables (no cap).
    /// Default: 0.25.
    pub knowledge_budget_ratio: f64,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            snip_threshold: 0.70,
            micro_threshold: 0.80,
            collapse_threshold: 0.90,
            auto_threshold: 0.95,
            renewal_threshold: 0.98,
            target_after_compress: 0.65,
            snip_per_msg_ratio: 0.05,
            carryover_ratio: 0.05,
            recovery_content_ratio: 0.25,
            preserve_recent_units: 2,
            preserve_recent_turns: 2,
            verbose_control_notes: false,
            collapse_assistant_narration: true,
            micro_compact_idle_minutes: 60,
            preserved_tool_results: 5,
            autocompact_buffer: 13_000,
            spool_threshold_bytes: 50 * 1024,
            spool_preview_bytes: 2 * 1024,
            knowledge_budget_ratio: 0.25,
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

    /// Auto-Compact trigger threshold (Layer 5).
    /// Returns `max_tokens - autocompact_buffer` (absolute value).
    pub fn autocompact_threshold(&self, max_tokens: u32) -> u32 {
        max_tokens.saturating_sub(self.autocompact_buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_reduction_defaults_to_quiet() {
        let c = ContextConfig::default();
        assert!(
            !c.verbose_control_notes,
            "verbose notes should be off by default"
        );
    }

    #[test]
    fn default_thresholds_strictly_increasing() {
        let c = ContextConfig::default();
        assert!(c.snip_threshold < c.micro_threshold);
        assert!(c.micro_threshold < c.collapse_threshold);
        assert!(c.collapse_threshold < c.auto_threshold);
        assert!(c.auto_threshold < c.renewal_threshold);
    }

    #[test]
    fn target_after_compress_below_snip_threshold() {
        let c = ContextConfig::default();
        assert!(c.target_after_compress < c.snip_threshold);
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
