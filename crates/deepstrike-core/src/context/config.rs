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
