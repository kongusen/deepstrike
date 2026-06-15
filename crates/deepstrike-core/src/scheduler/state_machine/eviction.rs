//! MM / eviction execution impl for [`super::LoopStateMachine`].

use super::{KernelObservation, LoopStateMachine};
use crate::context::pressure::PressureAction;
use crate::mm::{page_in_requests_from_calls, tier_hint_for_compress};
use crate::runtime::kernel::KernelPressureAction;
use crate::types::message::{Message, ToolCall};

impl LoopStateMachine {
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
        self.observations.push(KernelObservation::Compressed {
            action: KernelPressureAction::from(action),
            rho_after,
            summary: summary.clone(),
            archived: archived.clone(),
            invalidates_prefix_at,
        });
        if archived.is_empty() {
            return;
        }
        let tier_hint = tier_hint_for_compress(action).label().to_string();
        self.observations.push(KernelObservation::PageOut {
            turn: self.turn,
            action: KernelPressureAction::from(action),
            rho_after,
            summary,
            archived,
            tier_hint,
        });
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
                let (saved, summary, archived, cache_at) =
                    self.ctx.compress_with_time(PressureAction::SnipCompact, self.last_now_ms);
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
                let (_, summary, archived, cache_at) = self.ctx.compress(PressureAction::MicroCompact);
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

    pub(super) fn emit_page_in_requested(&mut self, calls: &[ToolCall]) {
        for req in page_in_requests_from_calls(calls) {
            self.observations.push(KernelObservation::PageInRequested {
                turn: self.turn,
                call_id: req.call_id,
                tool: req.tool,
                query: req.query,
                top_k: req.top_k,
            });
        }
    }

    /// Apply SDK-fetched long-term entries into the knowledge partition (page-in).
    pub fn apply_page_in(&mut self, entries: &[crate::mm::PageInEntry]) {
        for entry in entries {
            let tokens = entry
                .tokens
                .unwrap_or_else(|| self.ctx.engine.count(&entry.content).max(1));
            self.ctx.push_knowledge(Message::system(entry.content.clone()), tokens);
        }
    }
}
