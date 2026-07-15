//! MM / eviction execution impl for [`super::LoopStateMachine`].

use super::{KernelObservation, LoopAction, LoopPhase, LoopStateMachine, PendingHostEffect};
use crate::context::pressure::PressureAction;
use crate::mm::tier_hint_for_compress;
use crate::runtime::kernel::KernelPressureAction;
use crate::types::message::Message;
use crate::types::result::TerminationReason;

/// Max consecutive compact-and-retry attempts before a context overflow is declared
/// unrecoverable. Bounds the reactive recovery ladder (anti-spiral); resets on any successful
/// provider turn. The `force_compress` "nothing left to save" check is the real terminator —
/// this is the belt-and-suspenders cap so a degenerate provider that 413s forever still ends.
/// Classify a provider error message as a context-overflow (prompt-too-long / 413). Centralizes
/// the case-insensitive string match the four SDK runners (node/python/rust/wasm) each used to
/// own, so the recovery vocabulary lives in exactly one place.
pub(crate) fn is_prompt_too_long(message: &str) -> bool {
    let msg = message.to_lowercase();
    msg.contains("413")
        || msg.contains("too long")
        || msg.contains("prompt too long")
        || msg.contains("context length exceeded")
        || msg.contains("context_length_exceeded")
}

impl LoopStateMachine {
    /// Reactive recovery for a provider error the SDK reports via [`KernelInputEvent::ProviderError`].
    /// Owns the policy the SDK runners used to duplicate: classify → compact-and-retry on overflow
    /// (bounded, anti-spiral) → honest terminal when the ladder is exhausted. Returns the next
    /// [`LoopAction`] so the kernel's normal action tail dispatches it: `CallLLM` to retry the
    /// provider with a freshly compacted context, or `Done { ContextOverflow }` to give up.
    ///
    /// This is the reactive twin of the proactive eviction checkpoint in `feed` — same execution
    /// funnel (`force_compact` → `EvictionOp`s), now driven by a real provider 413 instead of an
    /// rho threshold, and surfaced as a kernel decision rather than SDK control flow.
    pub fn recover_from_provider_error(&mut self, message: &str) -> LoopAction {
        self.observations.clear();
        if !is_prompt_too_long(message) {
            // Non-overflow provider failures aren't recoverable here — terminate with `Error`,
            // the same outcome the runners produced, minus the fabricated `timeout`.
            return self.terminate(TerminationReason::Error, None);
        }
        if self.recovery_attempts >= self.provider_recovery_attempt_limit {
            return self.terminate(TerminationReason::ContextOverflow, None);
        }
        self.recovery_attempts += 1;
        if self.force_compact() {
            // Recovered headroom: re-render and retry as a normal turn (tools intact). The
            // The compression fact and page-out effect ride out in this same step; the provider
            // retry remains deferred until the host commits the archive.
            self.phase = LoopPhase::Reason;
            self.emit_call_llm()
        } else {
            // Nothing left to compact — the prompt genuinely won't fit. Honest terminal.
            self.terminate(TerminationReason::ContextOverflow, None)
        }
    }

    /// 强行进行一次最大力度的压缩归档。通常用于收到模型 API 413 (Prompt too long) 时做兜底重试。
    pub fn force_compact(&mut self) -> bool {
        let action = PressureAction::AutoCompact;
        let (saved, summary, archived, cache_at) = self.ctx.force_compress();
        if saved > 0 {
            self.push_compression_observations(action, summary, archived, cache_at);
            true
        } else {
            false
        }
    }

    pub(super) fn push_compression_observations(
        &mut self,
        action: PressureAction,
        summary: Option<String>,
        archived: Vec<Message>,
        invalidates_prefix_at: Option<usize>,
    ) {
        let rho_after = self.ctx.rho();
        let kernel_action = KernelPressureAction::from(action);
        self.observations.push(KernelObservation::Compressed {
            turn: self.turn,
            action: kernel_action,
            rho_after,
            summary: summary.clone(),
            archived_count: archived.len() as u32,
            invalidates_prefix_at,
        });
        if !archived.is_empty() {
            self.pending_host_effects
                .push_back(PendingHostEffect::ArchivePageOut {
                    turn: self.turn,
                    action: kernel_action,
                    summary,
                    archived,
                    tier: tier_hint_for_compress(action).label().to_string(),
                });
        }
        // K1: surface any boundary knowledge sweep that ran inside this compaction (an
        // in-place compaction can still have swept knowledge).
        self.emit_knowledge_sweep_observations();
    }

    /// K1: drain boundary knowledge sweeps (deferred upserts applied / marked entries dropped
    /// inside the compaction that just ran) into `KnowledgeSwept` observations. Called from the
    /// compression funnel above and after renewal — the only two places sweeps occur.
    pub(super) fn emit_knowledge_sweep_observations(&mut self) {
        for sweep in self.ctx.take_knowledge_sweeps() {
            self.observations.push(KernelObservation::KnowledgeSwept {
                turn: self.turn,
                removed_keys: sweep.removed_keys,
                tokens_freed: sweep.tokens_freed,
            });
        }
    }

    /// Execute one [`EvictionOp`] from an [`EvictionPlan`] — the single compaction execution
    /// funnel (M3). Each op maps to the appropriate legacy compression path for now (behavior
    /// preservation); the full refactor (step 3+) will route each to a dedicated executor.
    pub(super) fn execute_eviction_op(&mut self, op: &crate::mm::EvictionOp) {
        use crate::mm::EvictionOp;
        match op {
            EvictionOp::Spool(_) => {
                // Layer 1: handled at tool-result ingestion, not here. No-op in this path.
            }
            EvictionOp::Snip { per_msg_ratio: _ } => {
                // Layer 2: route to SnipCompact via the pipeline (behavior-preserving shim).
                // Use the public `compress_with_time` which already wires target_tokens from config.
                let (saved, summary, archived, cache_at) = self
                    .ctx
                    .compress_with_time(PressureAction::SnipCompact, self.last_now_ms);
                if saved > 0 || summary.is_some() {
                    self.push_compression_observations(
                        PressureAction::SnipCompact,
                        summary,
                        archived,
                        cache_at,
                    );
                }
            }
            EvictionOp::TimeDecayMicro => {
                // Layer 3: idle/time-decay micro-compact. Uses non-time compress path + stamps time.
                let (_, summary, archived, cache_at) =
                    self.ctx.compress(PressureAction::MicroCompact);
                self.push_compression_observations(
                    PressureAction::MicroCompact,
                    summary,
                    archived,
                    cache_at,
                );
                if let Some(now_ms) = self.last_now_ms {
                    self.ctx.last_compact_ms = Some(now_ms);
                }
            }
            EvictionOp::Collapse { target_tokens } => {
                // Layer 4: collapse to the planner's explicit target (W1-1 收口 — the executor honors
                // the plan's `target_tokens` verbatim instead of re-deriving it from config). The
                // planner stamps `config.target_tokens(max)`, so this is behavior-identical to the
                // old config-derived path while making the plan the single decision point.
                let (saved, summary, archived, cache_at) = self.ctx.compress_with_target(
                    PressureAction::ContextCollapse,
                    *target_tokens,
                    self.last_now_ms,
                );
                if saved > 0 || summary.is_some() {
                    self.push_compression_observations(
                        PressureAction::ContextCollapse,
                        summary,
                        archived,
                        cache_at,
                    );
                }
            }
            EvictionOp::AutoCompact { preserve_turns: _ } => {
                // Layer 5: auto-compact down to the preserve floor (target 0). The op carries the
                // truthful `preserve_turns` (= `config.preserve_recent_turns`, stamped by the planner);
                // the pipeline applies that same value at the compactor, so honoring the op and the
                // config path are byte-identical here. Per-op preserve plumbing into the pipeline is a
                // minor follow-up; the headline target placeholder is already gone (see Collapse).
                let (saved, summary, archived, cache_at) = self.ctx.force_compress();
                if saved > 0 || summary.is_some() {
                    self.push_compression_observations(
                        PressureAction::AutoCompact,
                        summary,
                        archived,
                        cache_at,
                    );
                }
            }
        }
    }

    /// Apply SDK-fetched entries into the knowledge partition — the durable, non-evicted slot.
    /// Reserved for genuinely stable content (skill definitions, host-pinned reference material),
    /// NOT single-use retrieval hits: a live `memory`/`knowledge` tool call's result already lands
    /// in `history` via the normal tool-result path and decays with the compression pyramid like
    /// any other tool output — pushing the SAME content here on top would make it immortal, which
    /// defeats the "use it, then let it go" policy this partition now enforces by construction
    /// (nothing routes ephemeral content here anymore; see the removed `PageInRequested` producer).
    pub fn apply_page_in(&mut self, entries: &[crate::mm::PageInEntry]) {
        for entry in entries {
            let tokens = entry
                .tokens
                .unwrap_or_else(|| self.ctx.engine.count(&entry.content).max(1));
            self.ctx.push_knowledge_entry(
                entry.key.as_deref().map(compact_str::CompactString::new),
                Message::system(entry.content.clone()),
                tokens,
                entry.pinned,
            );
        }
    }
}
