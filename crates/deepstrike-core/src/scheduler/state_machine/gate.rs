//! Syscall trap + governance gate impl for [`super::LoopStateMachine`].

use std::collections::HashMap;

use super::super::tcb::{TaskLifecycle, WaitReason};
use super::{
    ApprovalRequest, GateToolOutcome, KernelObservation, LoopAction, LoopEvent, LoopPhase,
    LoopStateMachine, SuspendState,
};
use crate::syscall::{Disposition, Syscall};
use crate::types::agent::AgentIdentity;
use crate::types::message::{Content, ToolCall, ToolErrorKind, ToolResult};

impl LoopStateMachine {
    /// P1 (M2): the single syscall trap. Every effectful request the SDK proposes is adjudicated
    /// here, returning a unified [`Disposition`]. Tool calls run the governance pipeline (mapping
    /// its verdict via `GovernanceVerdict -> Disposition`); `Spawn` and `WriteMemory` additionally
    /// pass the resource quota (concurrency / depth / write rate). Variants with no
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
        }
    }

    /// R3-1 governance: deny a runtime workflow-node submission that would grow the DAG past
    /// `ResourceQuota::max_workflow_nodes` — a backstop against an unbounded loop-until-done. Reads
    /// only the kernel's own workflow node count; no I/O. No quota / no active workflow → allow.
    pub(super) fn evaluate_submit_nodes_quota(&self, count: usize) -> Disposition {
        let Some(max) = self
            .resource_quota
            .as_ref()
            .and_then(|q| q.max_workflow_nodes)
        else {
            return Disposition::Allow;
        };
        let current = self.workflow.as_ref().map(|w| w.len()).unwrap_or(0);
        let projected = current.saturating_add(count);
        if projected > max {
            Disposition::Deny {
                stage: "workflow_growth",
                reason: format!(
                    "submit_nodes would grow workflow to {projected} nodes (max {max})"
                ),
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
        let quota = self.resource_quota.as_ref();
        if quota.is_none() && self.budget_grant.is_none() {
            return None;
        }
        let nodes_used = self.workflow.as_ref().map(|w| w.len()).unwrap_or(0);
        let running_subagents = self
            .tasks
            .all()
            .iter()
            .filter(|t| t.proc.is_some() && matches!(t.state, TaskLifecycle::Running))
            .count();
        let nodes_max = quota.and_then(|quota| quota.max_workflow_nodes);
        let max_concurrent_subagents = quota
            .and_then(|quota| quota.max_concurrent_subagents)
            .map(|m| m as usize);
        // M4/G5 token headroom: the run-level cumulative token cap is always set on the scheduler
        // budget, so a coordinator always sees how many tokens remain (the "use 10k tokens" signal).
        let tokens_max = self
            .budget_grant
            .as_ref()
            .and_then(|grant| grant.tokens)
            .unwrap_or(self.policy.max_total_tokens)
            .min(self.policy.max_total_tokens);
        let tokens_used = self.total_tokens;
        Some(crate::orchestration::workflow::WorkflowBudget {
            nodes_used,
            nodes_max,
            nodes_remaining: nodes_max.map(|m| m.saturating_sub(nodes_used)),
            running_subagents,
            max_concurrent_subagents,
            concurrency_remaining: max_concurrent_subagents
                .map(|m| m.saturating_sub(running_subagents)),
            tokens_used,
            tokens_max: Some(tokens_max),
            tokens_remaining: Some(tokens_max.saturating_sub(tokens_used)),
        })
    }

    /// Spawn quota over the kernel's own `TaskTable` — no I/O.
    ///
    /// One evaluator, two callers with different failure modes for the **transient**
    /// concurrency axis:
    /// - synchronous spawn (`concurrency_transient = false`): a blocking spawn that can't
    ///   run *now* can only be rolled back → `Deny`;
    /// - workflow run queue (`concurrency_transient = true`): the node stays `Ready` and is
    ///   `Defer`red — the spawn round ends and the batch is retried on the next completion
    ///   event, when a running sibling has freed a slot.
    /// The **permanent** axes (cumulative total, depth) are always a hard `Deny`: a completed
    /// sibling never frees a cumulative slot, and more nesting never becomes available.
    fn evaluate_spawn_quota_inner(&mut self, concurrency_transient: bool) -> Disposition {
        let quota = self.resource_quota.as_ref();
        if let Some(max) = quota.and_then(|quota| quota.max_concurrent_subagents) {
            // W-6: a zero-slot pool can never free a slot — Defer would park every workflow node
            // forever and the drive loop would fall through to an empty "completed" outcome. A
            // permanent impossibility is a hard Deny on both caller paths.
            if max == 0 {
                return Disposition::Deny {
                    stage: "quota",
                    reason: "max_concurrent_subagents=0 permits no spawn (misconfigured quota)"
                        .to_string(),
                };
            }
            let running = self
                .tasks
                .all()
                .iter()
                .filter(|t| t.proc.is_some() && matches!(t.state, TaskLifecycle::Running))
                .count() as u32;
            if running >= max {
                return if concurrency_transient {
                    Disposition::Defer { slot: running }
                } else {
                    Disposition::Deny {
                        stage: "quota",
                        reason: format!(
                            "max_concurrent_subagents={max} reached ({running} running)"
                        ),
                    }
                };
            }
        }
        let quota_max = quota.and_then(|quota| quota.max_total_subagents);
        let grant_max = self.budget_grant.as_ref().and_then(|grant| grant.subagents);
        let max_total = match (quota_max, grant_max) {
            (Some(quota), Some(grant)) => Some(quota.min(grant)),
            (Some(quota), None) => Some(quota),
            (None, Some(grant)) => Some(grant),
            (None, None) => None,
        };
        if let Some(max) = max_total {
            let total = self.local_subagents_spawned();
            if total >= max {
                if grant_max.is_some() {
                    self.observations.push(KernelObservation::BudgetExceeded {
                        turn: self.turn,
                        budget: "subagents".into(),
                        operation_id: String::new(),
                        reservation_id: self
                            .budget_grant
                            .as_ref()
                            .map(|grant| grant.reservation_id.clone()),
                    });
                }
                return Disposition::Deny {
                    stage: "budget_grant",
                    reason: format!("subagent grant {max} reached ({total} spawned locally)"),
                };
            }
        }
        if let Some(max) = quota.and_then(|quota| quota.max_spawn_depth) {
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

    /// Synchronous spawn path: quota misses roll the turn back like a denied tool call.
    pub(super) fn evaluate_spawn_quota(&mut self) -> Disposition {
        self.evaluate_spawn_quota_inner(false)
    }

    /// W2-1 workflow run-queue path: the transient concurrency axis defers instead of denying.
    pub(super) fn evaluate_spawn_quota_deferrable(&mut self) -> Disposition {
        self.evaluate_spawn_quota_inner(true)
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

    /// O6 RepeatFuse: track consecutive identical turn signatures (non-meta `name(args)` joined —
    /// the SAME key the 2c soft STOP uses, so the ladder's rungs agree on what "a repeat" is) and
    /// escalate: `deny_after` ⇒ commit a visible synthetic error result; `terminate_after` ⇒ end
    /// the run [`TerminationReason::NoProgress`] after one final no-tools report turn. Returns
    /// `Some(action)` when a rung fires, `None` to proceed. A meta-tool-only turn records no
    /// signature and neither advances nor resets the streak (control-plane chatter must not
    /// launder a stall). The streak state remains independent from turn checkpoints so recovery
    /// cannot erase the evidence that tripped the fuse.
    pub(super) fn check_repeat_fuse(&mut self, calls: &[ToolCall]) -> Option<LoopAction> {
        if !self.repeat_fuse.enabled {
            return None;
        }
        let sig = calls
            .iter()
            .filter(|c| !crate::context::manager::is_meta_tool(c.name.as_str()))
            .map(|c| {
                let args = super::compact_tool_args(&c.arguments);
                if args.is_empty() {
                    c.name.to_string()
                } else {
                    format!("{}({})", c.name, args)
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        if sig.is_empty() {
            return None;
        }
        if self.repeat_sig.as_deref() == Some(sig.as_str()) {
            self.repeat_count += 1;
        } else {
            self.repeat_sig = Some(sig.clone());
            self.repeat_count = 1;
            return None;
        }

        let fuse = self.repeat_fuse;
        let count = self.repeat_count;

        if fuse.terminate_after > 0 && count >= fuse.terminate_after {
            self.observations
                .push(KernelObservation::RepeatFuseTripped {
                    turn: self.turn,
                    signature: sig.clone(),
                    count,
                    action: "terminate".to_string(),
                });
            // Close every pair with a visible not-executed error result (trained convention;
            // also keeps the committed assistant tool_use wire-valid), then force one final
            // no-tools report turn.
            self.ctx.push_signal(format!(
                "[NO-PROGRESS] `{sig}` was re-issued {count}x consecutively with no new outcome. \
                 The run is terminating. Report what was accomplished and what remains, in plain text."
            ));
            self.pending_termination = Some(crate::types::result::TerminationReason::NoProgress);
            let results = fuse_denied_results(calls, count);
            return Some(self.commit_synthetic_results(results));
        }

        if fuse.deny_after > 0 && count >= fuse.deny_after {
            self.observations
                .push(KernelObservation::RepeatFuseTripped {
                    turn: self.turn,
                    signature: sig.clone(),
                    count,
                    action: "deny".to_string(),
                });
            // The directive rides IN the error result — the model sees its own repeated attempt
            // and the refusal in one place, exactly the shape it is trained to adapt to.
            let results = fuse_denied_results(calls, count);
            return Some(self.commit_synthetic_results(results));
        }

        None
    }

    /// Commit kernel-synthesized tool results through the ordinary `ToolResults` funnel,
    /// preserving observations already collected this step (the recursive feed clears them).
    pub(super) fn commit_synthetic_results(&mut self, results: Vec<ToolResult>) -> LoopAction {
        self.phase = LoopPhase::Reason;
        let kept = std::mem::take(&mut self.observations);
        let action = self.feed(LoopEvent::ToolResults { results });
        let inner = std::mem::replace(&mut self.observations, kept);
        self.observations.extend(inner);
        action
    }

    /// Evaluate proposed tool calls through the syscall trap (governance gate).
    pub(super) fn gate_tool_calls(&mut self, calls: &[ToolCall]) -> GateToolOutcome {
        if self.governance.is_none() {
            return GateToolOutcome::Proceed;
        }
        let mut gated: Vec<(String, String, String)> = Vec::new();
        let mut denied: Vec<(compact_str::CompactString, String)> = Vec::new();
        for call in calls {
            match self.evaluate_syscall(&Syscall::Invoke(call.clone())) {
                Disposition::Allow => {}
                Disposition::Gate { reason, .. } => {
                    gated.push((call.id.to_string(), call.name.to_string(), reason));
                }
                Disposition::Deny { reason, .. } => {
                    denied.push((call.id.clone(), reason));
                }
                Disposition::RateLimited { retry_after_ms } => {
                    let reason = format!("rate limited, retry after {retry_after_ms}ms");
                    denied.push((call.id.clone(), reason));
                }
                // Backpressure deferral is not produced by the governance gate today.
                Disposition::Defer { .. } => {}
            }
        }

        // Denials become committed error results — the model sees its own attempt.
        // Allowed siblings still execute; the synthetic results merge into their `ToolResults`
        // feed via `pending_denied_results` (the same funnel the approval path uses). When
        // EVERYTHING was denied there is nothing to execute, so the results commit as a normal
        // tool turn directly. `remaining` (= calls minus denied) is what the rest of the gate
        // operates on, so an AskUser suspend can never resurrect a denied call.
        let denied_ids: std::collections::HashSet<compact_str::CompactString> =
            denied.iter().map(|(id, _)| id.clone()).collect();
        for (call_id, reason) in denied {
            self.pending_denied_results.push(ToolResult {
                call_id,
                output: Content::Text(format!("permission denied: {reason}")),
                is_error: true,
                is_fatal: false,
                error_kind: Some(ToolErrorKind::GovernanceDenied),
                token_count: None,
            });
        }
        let remaining: Vec<ToolCall> = if denied_ids.is_empty() {
            calls.to_vec()
        } else {
            calls
                .iter()
                .filter(|call| !denied_ids.contains(&call.id))
                .cloned()
                .collect()
        };
        if remaining.is_empty() {
            let results = std::mem::take(&mut self.pending_denied_results);
            return GateToolOutcome::Blocked(self.commit_synthetic_results(results));
        }

        if gated.is_empty() {
            if denied_ids.is_empty() {
                return GateToolOutcome::Proceed;
            }
            self.phase = LoopPhase::Act {
                tool_calls: remaining.clone(),
            };
            self.set_lifecycle(TaskLifecycle::Running, None);
            return GateToolOutcome::Blocked(LoopAction::ExecuteTools { calls: remaining });
        }

        let pending_calls: Vec<String> = gated.iter().map(|(id, _, _)| id.clone()).collect();
        let gated_reasons: HashMap<String, String> = gated
            .iter()
            .map(|(id, _, reason)| (id.clone(), reason.clone()))
            .collect();
        let requests = remaining
            .iter()
            .filter_map(|call| {
                gated_reasons
                    .get(call.id.as_str())
                    .map(|reason| ApprovalRequest {
                        call_id: call.id.to_string(),
                        tool: call.name.to_string(),
                        arguments: call.arguments.clone(),
                        reason: reason.clone(),
                    })
            })
            .collect();
        self.suspend_state = Some(SuspendState::AskUser {
            calls: remaining,
            gated_reasons,
        });
        self.set_lifecycle(TaskLifecycle::Suspended, Some(WaitReason::Approval));
        self.observations.push(KernelObservation::Suspended {
            turn: self.turn,
            reason: "ask_user".to_string(),
            pending_calls,
        });
        GateToolOutcome::ApprovalRequired(requests)
    }

    /// Apply a host-owned approval effect result to the suspended tool set.
    pub fn resolve_approval(
        &mut self,
        approved_calls: Vec<String>,
        denied_calls: Vec<String>,
    ) -> LoopAction {
        self.observations.clear();

        let Some(state) = self.suspend_state.take() else {
            return LoopAction::AwaitingResume;
        };

        if !self.is_suspended() {
            return LoopAction::AwaitingResume;
        }

        let approved_set: std::collections::HashSet<String> =
            approved_calls.iter().cloned().collect();
        let denied_set: std::collections::HashSet<String> = denied_calls.iter().cloned().collect();

        let SuspendState::AskUser {
            calls,
            gated_reasons,
        } = state
        else {
            return LoopAction::AwaitingResume;
        };

        for call in &calls {
            if let Some(reason) = gated_reasons.get(call.id.as_str()) {
                self.observations.push(KernelObservation::ToolGated {
                    turn: self.turn,
                    call_id: call.id.to_string(),
                    tool: call.name.to_string(),
                    reason: reason.clone(),
                });
            }
        }
        self.observations.push(KernelObservation::Resumed {
            turn: self.turn,
            approved: approved_calls,
            denied: denied_calls,
        });

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
            self.set_lifecycle(TaskLifecycle::Running, None);
            return self.feed(LoopEvent::ToolResults { results });
        }

        self.phase = LoopPhase::Act {
            tool_calls: to_execute.clone(),
        };
        self.set_lifecycle(TaskLifecycle::Running, None);
        LoopAction::ExecuteTools { calls: to_execute }
    }

    /// Preserve suspension and reissue a failed host approval effect without
    /// recording a successful approval fact.
    pub fn retry_approval(&mut self, error: String) -> LoopAction {
        self.observations.clear();
        self.observations
            .push(KernelObservation::ApprovalResolutionFailed {
                turn: self.turn,
                error,
            });
        let Some(SuspendState::AskUser {
            calls,
            gated_reasons,
        }) = &self.suspend_state
        else {
            return LoopAction::AwaitingResume;
        };
        let requests = calls
            .iter()
            .filter_map(|call| {
                gated_reasons
                    .get(call.id.as_str())
                    .map(|reason| ApprovalRequest {
                        call_id: call.id.to_string(),
                        tool: call.name.to_string(),
                        arguments: call.arguments.clone(),
                        reason: reason.clone(),
                    })
            })
            .collect();
        LoopAction::RequestApproval { requests }
    }
}

/// One not-executed error result per call in a fuse-tripped batch. The directive lives in the
/// result text (the trained "blocked call → error result" shape); every pair closes so the
/// committed assistant tool_use stays wire-valid.
fn fuse_denied_results(calls: &[ToolCall], count: u32) -> Vec<ToolResult> {
    calls
        .iter()
        .map(|call| ToolResult {
            call_id: call.id.clone(),
            output: Content::Text(format!(
                "not executed: this exact call (same tool, same arguments) has been issued \
                 {count}x consecutively with no new outcome — do something DIFFERENT: change \
                 the arguments, use another tool, or report the task state as it stands"
            )),
            is_error: true,
            is_fatal: false,
            error_kind: None,
            token_count: None,
        })
        .collect()
}
