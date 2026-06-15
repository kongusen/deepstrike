//! Syscall trap + governance gate impl for [`super::LoopStateMachine`].

use std::collections::HashMap;

use super::super::tcb::{TaskState, WaitReason};
use super::{
    GateToolOutcome, KernelObservation, LoopAction, LoopEvent, LoopPhase, LoopStateMachine,
    SuspendState,
};
use crate::runtime::session::RollbackReason;
use crate::syscall::{Disposition, Syscall};
use crate::types::agent::AgentIdentity;
use crate::types::message::{Content, Message, ToolCall, ToolErrorKind, ToolResult};

impl LoopStateMachine {
    /// P1 (M2): the single syscall trap. Every effectful request the SDK proposes is adjudicated
    /// here, returning a unified [`Disposition`]. Tool calls run the governance pipeline (mapping
    /// its verdict via `GovernanceVerdict -> Disposition`); `Spawn` and `WriteMemory` additionally
    /// pass the resource quota (concurrency / depth / write rate). `PageIn`/`QueryMemory` carry no
    /// quota yet and default to `Allow` — but route through the *same* trap so a policy can attach
    /// later without a new ABI.
    pub(super) fn evaluate_syscall(&mut self, sys: &Syscall) -> Disposition {
        match sys {
            Syscall::Invoke(call) => {
                let caller = self
                    .run_spec
                    .as_ref()
                    .map(|s| s.identity.clone())
                    .unwrap_or_else(|| AgentIdentity::new("agent", "session"));
                match self.governance.as_mut() {
                    Some(pipeline) => pipeline.evaluate(call, &caller).into(),
                    None => Disposition::Allow,
                }
            }
            Syscall::Spawn(_) => self.evaluate_spawn_quota(),
            Syscall::WriteMemory(_) => self.evaluate_memory_write_quota(),
            Syscall::SubmitNodes { count } => self.evaluate_submit_nodes_quota(*count),
            // M5/G1: an agent-authored spec grows the DAG by `node_count`; same backstop as SubmitNodes.
            Syscall::LoadWorkflow { node_count } => self.evaluate_submit_nodes_quota(*node_count),
            Syscall::PageIn(_) | Syscall::QueryMemory(_) => Disposition::Allow,
        }
    }

    /// R3-1 governance: deny a runtime workflow-node submission that would grow the DAG past
    /// `ResourceQuota::max_workflow_nodes` — a backstop against an unbounded loop-until-done. Reads
    /// only the kernel's own workflow node count; no I/O. No quota / no active workflow → allow.
    pub(super) fn evaluate_submit_nodes_quota(&self, count: usize) -> Disposition {
        let Some(max) = self.resource_quota.as_ref().and_then(|q| q.max_workflow_nodes) else {
            return Disposition::Allow;
        };
        let current = self.workflow.as_ref().map(|w| w.len()).unwrap_or(0);
        let projected = current.saturating_add(count);
        if projected > max {
            Disposition::Deny {
                stage: "workflow_growth",
                reason: format!("submit_nodes would grow workflow to {projected} nodes (max {max})"),
            }
        } else {
            Disposition::Allow
        }
    }

    /// Public entry to the syscall trap, for effectful requests adjudicated outside the tool-call
    /// path (memory writes, page-in). Tool calls go through `gate_tool_calls`; spawn through
    /// `spawn_sub_agent`. All converge on [`Self::evaluate_syscall`].
    pub fn gate_syscall(&mut self, sys: &Syscall) -> Disposition {
        self.evaluate_syscall(sys)
    }

    /// G4: snapshot the active workflow's remaining headroom under the resource quota. `None` when no
    /// quota is installed (nothing to bound, so no signal to report). Reads only the kernel's own
    /// node count + `TaskTable` — no I/O. Carried on `WorkflowBatchSpawned` so a coordinator node can
    /// scale its next submission to what is actually available.
    pub(super) fn workflow_budget(&self) -> Option<crate::orchestration::workflow::WorkflowBudget> {
        let quota = self.resource_quota.as_ref()?;
        let nodes_used = self.workflow.as_ref().map(|w| w.len()).unwrap_or(0);
        let running_subagents = self
            .tasks
            .all()
            .iter()
            .filter(|t| t.proc.is_some() && matches!(t.state, TaskState::Running))
            .count();
        let nodes_max = quota.max_workflow_nodes;
        let max_concurrent_subagents = quota.max_concurrent_subagents.map(|m| m as usize);
        // M4/G5 token headroom: the run-level cumulative token cap is always set on the scheduler
        // budget, so a coordinator always sees how many tokens remain (the "use 10k tokens" signal).
        let tokens_max = self.policy.max_total_tokens;
        Some(crate::orchestration::workflow::WorkflowBudget {
            nodes_used,
            nodes_max,
            nodes_remaining: nodes_max.map(|m| m.saturating_sub(nodes_used)),
            running_subagents,
            max_concurrent_subagents,
            concurrency_remaining: max_concurrent_subagents
                .map(|m| m.saturating_sub(running_subagents)),
            tokens_used: self.total_tokens,
            tokens_max: Some(tokens_max),
            tokens_remaining: Some(tokens_max.saturating_sub(self.total_tokens)),
        })
    }

    /// Spawn quota: deny once the running sub-agent count hits `max_concurrent_subagents`, or the
    /// new child's nesting depth would exceed `max_spawn_depth`. Reads only the kernel's own
    /// `TaskTable` — no I/O. A denied spawn rolls the turn back like a denied tool call.
    pub(super) fn evaluate_spawn_quota(&self) -> Disposition {
        let Some(quota) = self.resource_quota.as_ref() else {
            return Disposition::Allow;
        };
        if let Some(max) = quota.max_concurrent_subagents {
            let running = self
                .tasks
                .all()
                .iter()
                .filter(|t| t.proc.is_some() && matches!(t.state, TaskState::Running))
                .count() as u32;
            if running >= max {
                return Disposition::Deny {
                    stage: "quota",
                    reason: format!(
                        "max_concurrent_subagents={max} reached ({running} running)"
                    ),
                };
            }
        }
        if let Some(max) = quota.max_spawn_depth {
            // Sub-agents currently parent to the root task (depth 1). Nested spawning would
            // generalize this to the spawning task's lineage depth.
            let depth = 1u32;
            if depth > max {
                return Disposition::Deny {
                    stage: "quota",
                    reason: format!("max_spawn_depth={max} exceeded (depth {depth})"),
                };
            }
        }
        Disposition::Allow
    }

    /// W2-1: spawn-quota evaluation for a **deferrable** caller (the workflow run queue). Unlike the
    /// synchronous `evaluate_spawn_quota` — where a blocking spawn that can't run *now* can only be
    /// rolled back (`Deny`) — a run-queue node that hits the **transient** concurrency limit is
    /// `Defer`red: it stays `Ready`, gets a `deferred_until` backoff, and is retried by the scheduler
    /// once a running sibling frees a slot. A **permanent** depth limit is still a hard `Deny` (more
    /// nesting will never become available). This is the first **real producer** of
    /// `Disposition::Defer` — resolving diagnostic A's dead variant for the run-queue path. The
    /// synchronous path keeps using `evaluate_spawn_quota`, so its `Deny`-on-quota behavior (and the
    /// golden/tests pinning it) is unchanged.
    pub(super) fn evaluate_spawn_quota_deferrable(&self) -> Disposition {
        let Some(quota) = self.resource_quota.as_ref() else {
            return Disposition::Allow;
        };
        if let Some(max) = quota.max_concurrent_subagents {
            let running = self
                .tasks
                .all()
                .iter()
                .filter(|t| t.proc.is_some() && matches!(t.state, TaskState::Running))
                .count() as u32;
            if running >= max {
                // Transient: a running sibling will complete and free a slot → defer and retry.
                return Disposition::Defer { slot: running };
            }
        }
        if let Some(max) = quota.max_spawn_depth {
            let depth = 1u32;
            if depth > max {
                return Disposition::Deny {
                    stage: "quota",
                    reason: format!("max_spawn_depth={max} exceeded (depth {depth})"),
                };
            }
        }
        Disposition::Allow
    }

    /// Memory-write quota: a rolling-window rate limit. Prunes timestamps older than the window,
    /// rate-limits if the window is full, else records this write's time. Uses the observed clock
    /// (`last_now_ms`); with no clock fed it degenerates to "all in window 0" which still bounds
    /// the count per `window_ms`.
    pub(super) fn evaluate_memory_write_quota(&mut self) -> Disposition {
        let Some((max, window)) = self
            .resource_quota
            .as_ref()
            .and_then(|q| q.memory_writes_per_window)
        else {
            return Disposition::Allow;
        };
        let now = self.last_now_ms.unwrap_or(0);
        self.memory_write_times
            .retain(|&t| now.saturating_sub(t) < window);
        if self.memory_write_times.len() as u32 >= max {
            let oldest = self.memory_write_times.first().copied().unwrap_or(now);
            let retry_after_ms = window.saturating_sub(now.saturating_sub(oldest));
            return Disposition::RateLimited { retry_after_ms };
        }
        self.memory_write_times.push(now);
        Disposition::Allow
    }

    /// Evaluate proposed tool calls through the syscall trap (governance gate).
    pub(super) fn gate_tool_calls(&mut self, calls: &[ToolCall]) -> GateToolOutcome {
        if self.governance.is_none() {
            return GateToolOutcome::Proceed;
        }

        let mut gated: Vec<(String, String, String)> = Vec::new();
        let mut hard_block: Option<(String, String)> = None;
        for call in calls {
            match self.evaluate_syscall(&Syscall::Invoke(call.clone())) {
                Disposition::Allow => {}
                Disposition::Gate { reason, .. } => {
                    gated.push((call.id.to_string(), call.name.to_string(), reason));
                }
                Disposition::Deny { reason, .. } => {
                    if hard_block.is_none() {
                        hard_block = Some((call.name.to_string(), reason));
                    }
                }
                Disposition::RateLimited { retry_after_ms } => {
                    if hard_block.is_none() {
                        hard_block = Some((
                            call.name.to_string(),
                            format!("rate limited, retry after {retry_after_ms}ms"),
                        ));
                    }
                }
                // Backpressure deferral is not produced by the governance gate today.
                Disposition::Defer { .. } => {}
            }
        }

        if let Some((tool_name, reason)) = hard_block {
            let rb = RollbackReason::GovernanceDenied { tool_name, reason };
            let note = Message::user(super::super::rollback::build_rollback_note(
                &rb,
                self.ctx.config.verbose_control_notes,
            ));
            self.rollback(rb);
            self.ctx
                .push_signal(note.content.as_text().unwrap_or_default().to_string());
            self.phase = LoopPhase::Reason;
            return GateToolOutcome::Blocked(self.emit_call_llm());
        }

        if gated.is_empty() {
            return GateToolOutcome::Proceed;
        }

        let pending_calls: Vec<String> = gated.iter().map(|(id, _, _)| id.clone()).collect();
        let gated_reasons: HashMap<String, String> = gated
            .iter()
            .map(|(id, _, reason)| (id.clone(), reason.clone()))
            .collect();
        for (call_id, tool, reason) in &gated {
            self.observations.push(KernelObservation::ToolGated {
                turn: self.turn,
                call_id: call_id.clone(),
                tool: tool.clone(),
                reason: reason.clone(),
            });
        }
        self.suspend_state = Some(SuspendState::AskUser {
            calls: calls.to_vec(),
            gated_reasons,
        });
        self.set_lifecycle(TaskState::Suspended, Some(WaitReason::Approval));
        self.observations.push(KernelObservation::Suspended {
            turn: self.turn,
            reason: "ask_user".to_string(),
            pending_calls,
        });
        GateToolOutcome::Suspended
    }

    /// Resume from `Suspended` after SDK resolves human approval (or wake preload).
    pub fn resume_from_suspend(
        &mut self,
        approved_calls: Vec<String>,
        denied_calls: Vec<String>,
    ) -> LoopAction {
        self.observations.clear();

        if self.suspend_state.is_none() && approved_calls.is_empty() && denied_calls.is_empty() {
            return self.resume_after_preload();
        }

        let Some(state) = self.suspend_state.take() else {
            if approved_calls.is_empty() && denied_calls.is_empty() {
                return self.resume_after_preload();
            }
            return LoopAction::AwaitingResume;
        };

        if !self.is_suspended() {
            return LoopAction::AwaitingResume;
        }

        self.observations.push(KernelObservation::Resumed {
            turn: self.turn,
            approved: approved_calls.clone(),
            denied: denied_calls.clone(),
        });

        let approved_set: std::collections::HashSet<String> = approved_calls.into_iter().collect();
        let denied_set: std::collections::HashSet<String> = denied_calls.into_iter().collect();

        let SuspendState::AskUser { calls, gated_reasons } = state else {
            return LoopAction::AwaitingResume;
        };

        let mut to_execute = Vec::new();
        let mut synthetic_results = Vec::new();

        for call in calls {
            let id = call.id.to_string();
            if let Some(reason) = gated_reasons.get(&id) {
                if approved_set.contains(&id) {
                    to_execute.push(call.clone());
                } else if denied_set.contains(&id) || !approved_set.contains(&id) {
                    synthetic_results.push(ToolResult {
                        call_id: call.id.clone(),
                        output: Content::Text(format!("permission denied: {reason}")),
                        is_error: true,
                        is_fatal: false,
                        error_kind: Some(ToolErrorKind::GovernanceDenied),
                        token_count: None,
                    });
                }
            } else {
                to_execute.push(call.clone());
            }
        }

        self.pending_denied_results = synthetic_results;

        if to_execute.is_empty() {
            let results = std::mem::take(&mut self.pending_denied_results);
            self.phase = LoopPhase::Reason;
            self.set_lifecycle(TaskState::Running, None);
            return self.feed(LoopEvent::ToolResults { results });
        }

        self.phase = LoopPhase::Act {
            tool_calls: to_execute.clone(),
        };
        self.set_lifecycle(TaskState::Running, None);
        LoopAction::ExecuteTools {
            calls: to_execute,
        }
    }
}
