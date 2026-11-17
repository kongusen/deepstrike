//! Context pressure, the estimate's calibration, compaction and renewal.

use super::*;

impl ContextManager {
    // ── Pressure ──────────────────────────────────────────────────────────────

    /// **Raw** rho — full partition weight (or provider-observed tokens when available). This is the
    /// projection-decision rho: [`Self::recompute_handle_residency`] marks the Resident↔Collapsed set
    /// from *this* value, so it must NOT discount paged content (else collapse → rho drops →
    /// un-collapse would oscillate).
    pub fn rho(&self) -> f64 {
        let observed = self.pressure.pressure(
            &self.partitions,
            &self.engine,
            self.last_observed_prompt_tokens,
        );
        // A provider sees a selected projection, not the entire resident history. Its smaller
        // count must not suppress compaction while the unselected history still triggers renewal.
        // Calibration may raise the estimate (the provider counts more than the kernel does, as
        // for CJK text or tool templates) but never lowers it: a provider or proxy reporting too
        // little must not delay compaction past the point the kernel's own count calls for.
        let max_tokens = f64::from(self.max_tokens.max(1));
        let raw = self
            .partitions
            .total_tokens(&self.engine)
            .saturating_add(self.replay_tokens());
        let estimated = if self.max_tokens == 0 {
            0.0
        } else {
            f64::from(self.calibrated(raw).max(raw)) / max_tokens
        };
        observed.max(estimated)
    }

    pub fn set_observed_prompt_tokens(&mut self, tokens: u32) {
        self.last_observed_prompt_tokens = Some(tokens);
        if self.last_request_estimate > 0 {
            if self.estimate_samples.len() == ESTIMATE_CALIBRATION_SAMPLES {
                self.estimate_samples.pop_front();
            }
            self.estimate_samples.push_back((self.last_request_estimate, tokens));
            self.last_request_estimate = 0;
        }
    }

    /// Remember `raw`, the uncalibrated estimate of the request about to be sent, and return it
    /// calibrated.
    pub fn note_request_estimate(&mut self, raw: u32) -> u32 {
        self.last_request_estimate = raw;
        self.calibrated(raw)
    }

    /// `tokens` (an uncalibrated estimate) as the provider would count it: through the
    /// least-squares line of the recent samples; with one sample (or samples of one size) by
    /// their mean offset, the gap being mostly fixed overhead; unchanged with none. Integer arithmetic, so replay reproduces
    /// it; bounded to 1/2..4x of the estimate.
    pub(super) fn calibrated(&self, tokens: u32) -> u32 {
        let n = self.estimate_samples.len() as i128;
        if n == 0 || tokens == 0 {
            return tokens;
        }
        let (mut sx, mut sy, mut sxx, mut sxy) = (0i128, 0i128, 0i128, 0i128);
        for &(x, y) in &self.estimate_samples {
            let (x, y) = (i128::from(x), i128::from(y));
            sx += x;
            sy += y;
            sxx += x * x;
            sxy += x * y;
        }
        let x = i128::from(tokens);
        let denominator = n * sxx - sx * sx;
        let fitted = if n >= 2 && denominator > 0 {
            (sy * sxx - sx * sxy + x * (n * sxy - sx * sy)) / denominator
        } else {
            x + (sy - sx) / n
        };
        fitted.clamp(x / 2, x * 4).clamp(0, i128::from(u32::MAX)) as u32
    }

    pub fn should_compress(&self) -> PressureAction {
        // Compaction-tier recommendation runs on **raw** rho. A paging-aware discount
        // (`effective_rho`) was tried during W1-1 and over-relieved pressure: once
        // `micro_compact` paged out tool-result handles, the discounted rho fell below the
        // collapse/auto_compact thresholds and the heavy tiers never fired. Raw rho keeps
        // escalation intact (recoverable from git if a cache-aware planner ever lands).
        self.kernel_owned_tier(self.pressure.recommend(self.rho()))
    }

    /// The highest compaction tier at or below `action` that the kernel still runs itself. A tier
    /// the provider performs natively is skipped, never escalated past: delegated compaction falls
    /// back to the kernel's own tool-result tier when that one is still kernel-owned. A forced
    /// compaction (`force_compress`, the overflow ladder) is a recovery, not a pressure tier, and is
    /// never filtered; renewal is decided apart from this and is never delegated.
    pub fn kernel_owned_tier(&self, action: PressureAction) -> PressureAction {
        let clearing = self.config.delegate_tool_result_clearing;
        match action {
            PressureAction::ContextCollapse | PressureAction::AutoCompact
                if self.config.delegate_compaction =>
            {
                if clearing {
                    PressureAction::None
                } else {
                    PressureAction::MicroCompact
                }
            }
            PressureAction::MicroCompact if clearing => {
                PressureAction::None
            }
            other => other,
        }
    }

    pub fn compress(
        &mut self,
        action: PressureAction,
    ) -> (u32, Option<String>, Vec<CoreMessage>, Option<usize>) {
        let target = self.config.target_tokens(self.max_tokens);
        self.compress_with_target(action, target)
    }

    pub fn force_compress(&mut self) -> (u32, Option<String>, Vec<CoreMessage>, Option<usize>) {
        self.compress_with_target(PressureAction::AutoCompact, 0)
    }

    /// W1-1 收口: run one compaction `action` toward an **explicit** `target_tokens`, instead of
    /// re-deriving the target from config. This is what lets `EvictionOp::Collapse { target_tokens }`
    /// flow from the planner (the single decision point) straight to the executor — the compactor no
    /// longer re-decides the target. This is the single compaction implementation;
    /// `compress_with_time` (config-derived target) and `force_compress` (AutoCompact, target 0)
    /// are thin delegations.
    pub fn compress_with_target(
        &mut self,
        action: PressureAction,
        target_tokens: u32,
    ) -> (u32, Option<String>, Vec<CoreMessage>, Option<usize>) {
        let (saved, summary, archived, cache_at) = self.compression.compress(
            &mut self.partitions,
            action,
            self.max_tokens,
            target_tokens,
            &self.engine,
        );
        for row in &archived {
            let record = HistoryItemRecord::of(
                row.tag,
                row.measurement.tokens,
                row.tag.fidelity,
                Some(ItemEnd::Compacted),
            );
            self.retire_history_item(record);
        }
        let archived: Vec<CoreMessage> = archived.into_iter().map(|row| row.message).collect();
        if saved > 0 || !archived.is_empty() {
            // The measurement describes the request before this rewrite, not the new context.
            self.last_observed_prompt_tokens = None;
            self.cache_ledger.note_cause(BreakCause::of_compaction(action));
        }
        // Archived messages have left history — drop their now-orphaned handles (bounds the table).
        if !archived.is_empty() {
            self.prune_orphaned_handles();
            self.prune_item_replays();
            // Compaction rewrote the history prefix — start a fresh collapse generation so
            // surviving handles re-evaluate from Resident (P0-C: the one cache-free un-collapse point).
            self.reset_collapse_generation();
            // K1: the prompt-cache prefix is being rebuilt anyway — the one cache-free moment to
            // apply deferred knowledge upserts/removals (rewriting system[1] bytes).
            self.sweep_knowledge_at_boundary();
        }
        // P2-D × P1-E: re-anchor the frozen-prefix boundary only when the compaction actually broke
        // the prompt-cache prefix (`result.3` = the planner's per-step `cache_at` cost, `Some` ⇒ a
        // prefix break). A prefix-safe compaction (late Snip/Excerpt that touches no early message)
        // leaves `[0..frozen]` byte-stable, so the deep cache survives the compaction and the boundary
        // holds — strictly more precise than the old `archived`-keyed reset, which missed an early
        // in-place Snip and needlessly re-anchored after a prefix-safe pass.
        if cache_at.is_some() {
            self.frozen_history_len = self.partitions.history.len();
        }
        (saved, summary, archived, cache_at)
    }

    /// W1-1 收口: the compaction target the planner stamps into `EvictionOp::Collapse`, read once
    /// from config so the op carries the real value and the executor stays a pure executor.
    pub fn compaction_target_tokens(&self) -> u32 {
        self.config.target_tokens(self.max_tokens)
    }

    // ── Renewal ───────────────────────────────────────────────────────────────

    pub fn should_renew(&self) -> bool {
        self.renewal
            .should_renew(&self.pressure, &self.partitions, &self.engine)
    }

    pub fn renew(&mut self) {
        let renewed = self
            .renewal
            .renew(&self.partitions, self.max_tokens, &self.engine);
        let carried: std::collections::HashSet<ItemId> =
            renewed.history.tags().iter().map(|tag| tag.id).collect();
        for index in 0..self.partitions.history.len() {
            let tag = self.partitions.history.tag(index);
            if !carried.contains(&tag.id) {
                let tokens = self.partitions.history.measured_tokens(index, &self.engine);
                let fidelity = self.history_fidelity(index);
                self.retire_history_item(HistoryItemRecord::of(
                    tag,
                    tokens,
                    fidelity,
                    Some(ItemEnd::Renewed),
                ));
            }
        }
        self.partitions = renewed;
        self.sprint += 1;
        self.cache_ledger.note_cause(BreakCause::Renewal);
        self.last_observed_prompt_tokens = None;
        // The renewed history dropped earlier recalls with the rest; recall again for the new sprint.
        self.recall_due = self.recall_top_k.is_some();
        // History was rebuilt wholesale — drop handles anchored to messages it no longer carries,
        // and start a fresh collapse generation (P0-C) since the whole prefix changed.
        self.prune_orphaned_handles();
        self.prune_item_replays();
        self.reset_collapse_generation();
        // K1: renewal is a boundary — apply deferred knowledge upserts/removals now.
        self.sweep_knowledge_at_boundary();
        // P1-E: the renewed history is the new frozen base.
        self.frozen_history_len = self.partitions.history.len();
    }
}
