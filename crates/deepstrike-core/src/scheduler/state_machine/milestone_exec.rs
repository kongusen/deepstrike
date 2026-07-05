//! Milestone handling impl for [`super::LoopStateMachine`].

use super::{KernelObservation, LoopAction, LoopPhase, LoopStateMachine};
use crate::types::milestone::{MilestoneCheckResult, MilestoneContract};
use crate::types::result::TerminationReason;

impl LoopStateMachine {
    /// Load a milestone contract.  Must be called before `start()`.
    pub fn load_milestone_contract(&mut self, contract: MilestoneContract) {
        self.milestone.load_contract(contract);
    }

    /// Returns the ID of the current (not-yet-passed) phase, or `None` when
    /// no contract is loaded or all phases are complete.
    pub fn current_milestone_phase_id(&self) -> Option<&str> {
        self.milestone.current_phase_id()
    }

    /// Returns the acceptance criteria of the current phase as a slice.
    pub fn current_milestone_criteria(&self) -> &[String] {
        self.milestone.current_criteria()
    }

    /// Returns `true` when there is no contract or all phases have passed.
    pub fn is_milestone_complete(&self) -> bool {
        self.milestone.is_complete()
    }

    pub(super) fn handle_milestone_result(&mut self, result: MilestoneCheckResult) -> LoopAction {
        self.observations.clear();

        if result.passed {
            // Advance phase: mount unlocked capabilities with milestone provenance.
            if let Some(phase) = self.milestone.current_phase().cloned() {
                let mounted_by = Some(format!("milestone:{}", phase.id));
                let mut unlocked: Vec<String> = Vec::new();
                for cap in phase.unlocks {
                    unlocked.push(format!("{}:{}", cap.kind.label(), cap.id));
                    self.mount_capability(
                        cap,
                        mounted_by.clone(),
                        Some("phase_advance".to_string()),
                    );
                }
                self.observations.push(KernelObservation::MilestoneAdvanced {
                    turn: self.turn,
                    phase_id: phase.id,
                    capabilities_unlocked: unlocked,
                });
            }
            self.milestone.advance();

            if self.is_milestone_complete() {
                return self.terminate(TerminationReason::Completed, None);
            }

            // Prompt the LLM with the next phase context.
            if let Some(criteria) = self.milestone.current_phase().map(|p| {
                if p.criteria.is_empty() {
                    format!("[NEXT MILESTONE PHASE: {}]", p.id)
                } else {
                    format!(
                        "[NEXT MILESTONE PHASE: {} — Criteria: {}]",
                        p.id,
                        p.criteria.join("; ")
                    )
                }
            }) {
                self.ctx.push_signal(criteria);
            }
            self.phase = LoopPhase::Reason;
            self.emit_call_llm()
        } else {
            // Phase blocked — increment retry count.
            let blocked_count = self.milestone.record_block();
            let reason = result.reason.as_deref().unwrap_or("milestone criteria not met");

            // Retrieve the rollback_policy and retry budget for the current phase.
            let (rollback_policy, max_attempts) = self
                .milestone
                .current_phase()
                .map(|p| {
                    let max = p
                        .retry_policy
                        .as_ref()
                        .map(|rp| rp.max_attempts)
                        .unwrap_or(0);
                    (p.rollback_policy.clone(), max)
                })
                .unwrap_or_default();

            // Check retry budget (0 = unlimited).
            let budget_exceeded = max_attempts > 0 && blocked_count as u32 >= max_attempts;

            if budget_exceeded {
                use crate::types::milestone::MilestoneRollbackPolicy;
                match rollback_policy {
                    MilestoneRollbackPolicy::Terminate => {
                        self.observations.push(KernelObservation::MilestoneBlocked {
                            turn: self.turn,
                            phase_id: result.phase_id.clone(),
                            reason: format!("retry budget exhausted: {reason}"),
                        });
                        return self.terminate(TerminationReason::MilestoneExceeded, None);
                    }
                    MilestoneRollbackPolicy::Rollback => {
                        self.observations.push(KernelObservation::MilestoneBlocked {
                            turn: self.turn,
                            phase_id: result.phase_id.clone(),
                            reason: format!("retry budget exhausted (rollback): {reason}"),
                        });
                        let rb_reason = crate::runtime::session::RollbackReason::MalformedReplay {
                            reason: format!("milestone {} retry budget exhausted", result.phase_id),
                        };
                        self.rollback(rb_reason);
                        self.phase = LoopPhase::Reason;
                        return self.emit_call_llm();
                    }
                    MilestoneRollbackPolicy::Continue => {
                        // Fall through to normal blocked handling below.
                    }
                }
            }

            // Normal blocked: inject message and retry.
            self.ctx.push_signal(format!(
                "[MILESTONE BLOCKED: {} — {}. Address the criteria and try again.]",
                result.phase_id, reason
            ));
            self.observations.push(KernelObservation::MilestoneBlocked {
                turn: self.turn,
                phase_id: result.phase_id,
                reason: reason.to_string(),
            });
            self.phase = LoopPhase::Reason;
            self.emit_call_llm()
        }
    }
}
