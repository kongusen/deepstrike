//! Workflow orchestration impl for [`super::LoopStateMachine`].

use super::{KernelObservation, LoopAction, LoopPhase, LoopStateMachine, SuspendState};
use super::super::tcb::{TaskState, Tcb, WaitReason};
use crate::proc::AgentProcess;
use crate::runtime::session::RollbackReason;
use crate::syscall::{Disposition, Syscall};
use crate::types::message::Message;
use crate::types::result::SubAgentResult;

impl LoopStateMachine {
    /// Whether a workflow DAG is currently in flight.
    pub fn workflow_active(&self) -> bool {
        self.workflow.is_some()
    }

    /// W0: load a workflow DAG and spawn its first gated batch. On an invalid spec (cycle /
    /// out-of-range dependency) the workflow is not installed and the loop continues with a
    /// rollback note, mirroring how a denied effect is surfaced.
    pub fn load_workflow(
        &mut self,
        spec: crate::orchestration::workflow::WorkflowSpec,
        parent_session_id: &str,
    ) -> LoopAction {
        self.install_workflow(crate::orchestration::workflow::WorkflowRun::new(
            &spec,
            parent_session_id,
        ))
    }

    /// R3-1: append nodes to the in-flight workflow DAG at runtime, then drive one gated spawn round
    /// so any now-ready node starts immediately (alongside the still-running submitter). The append
    /// is pure graph mutation; each appended node's *spawn* still passes through the spawn gate in
    /// [`Self::spawn_ready_workflow_nodes`] (quota / depth / quarantine), so this adds no new gate and
    /// can't outrun the concurrency cap. No active workflow (or an empty submission) → a no-op that
    /// leaves the current suspension untouched.
    pub fn submit_workflow_nodes(
        &mut self,
        nodes: Vec<crate::orchestration::workflow::WorkflowNode>,
        submitter_agent_id: Option<&str>,
    ) -> LoopAction {
        if nodes.is_empty() || self.workflow.is_none() {
            return LoopAction::AwaitingResume;
        }
        // R3-1 governance: gate DAG growth through the syscall trap. A `max_workflow_nodes` quota
        // denies a submission that would grow the workflow past the cap (runaway loop-until-done
        // backstop); the workflow continues with its existing nodes and a rollback note is surfaced.
        let disposition = self.evaluate_syscall(&Syscall::SubmitNodes { count: nodes.len() });
        if !disposition.is_allowed() {
            let reason = match &disposition {
                Disposition::Deny { reason, .. } => reason.clone(),
                _ => "workflow node submission denied".to_string(),
            };
            let rb = RollbackReason::GovernanceDenied {
                tool_name: "submit_workflow_nodes".to_string(),
                reason,
            };
            let note = Message::user(super::super::rollback::build_rollback_note(
                &rb,
                self.ctx.config.verbose_control_notes,
            ));
            self.ctx
                .push_signal(note.content.as_text().unwrap_or_default().to_string());
            return LoopAction::AwaitingResume;
        }
        if let Some(run) = self.workflow.as_mut() {
            // G1: route through the trust-aware entry point — a quarantined submitter's nodes are
            // coerced to quarantined in-kernel before append (no topological privilege escalation).
            run.submit_nodes_from(submitter_agent_id, nodes);
        }
        self.drive_workflow(None)
    }

    /// M5/G1: an agent authors a whole `WorkflowSpec` (the article's "model writes its own harness").
    /// **Bootstrap-or-flatten** (one DAG, unified governance — never a workflow stack):
    /// - **No workflow active** (top-level agent) ⇒ *bootstrap* the DAG via `install_workflow`, exactly
    ///   like the host-only `load_workflow`, but agent-reachable through the syscall trap.
    /// - **Workflow active** (caller is a node) ⇒ *flatten*: append the spec's nodes through the same
    ///   trust-aware `submit_nodes_from` as `submit_workflow_nodes` (a spec is just a node batch).
    ///
    /// Gated by `Syscall::LoadWorkflow` (the same `max_workflow_nodes` backstop as `SubmitNodes`), so an
    /// authored harness cannot overgrow the DAG. A second author while a workflow is active flattens —
    /// it never stacks — so there is no unbounded recursion of kernels. Empty spec → no-op.
    pub fn submit_workflow(
        &mut self,
        spec: crate::orchestration::workflow::WorkflowSpec,
        parent_session_id: &str,
        submitter_agent_id: Option<&str>,
    ) -> LoopAction {
        if spec.nodes.is_empty() {
            return LoopAction::AwaitingResume;
        }
        let disposition = self.evaluate_syscall(&Syscall::LoadWorkflow {
            node_count: spec.nodes.len(),
        });
        if !disposition.is_allowed() {
            let reason = match &disposition {
                Disposition::Deny { reason, .. } => reason.clone(),
                _ => "workflow authoring denied".to_string(),
            };
            let rb = RollbackReason::GovernanceDenied {
                tool_name: "start_workflow".to_string(),
                reason,
            };
            let note = Message::user(super::super::rollback::build_rollback_note(
                &rb,
                self.ctx.config.verbose_control_notes,
            ));
            self.ctx
                .push_signal(note.content.as_text().unwrap_or_default().to_string());
            return LoopAction::AwaitingResume;
        }
        match self.workflow.as_mut() {
            // Flatten: caller is a workflow node; grow the existing DAG (G1 coercion applies).
            Some(run) => {
                run.submit_nodes_from(submitter_agent_id, spec.nodes);
                self.drive_workflow(None)
            }
            // Bootstrap: top-level agent starts a brand-new workflow in this same kernel.
            None => self.install_workflow(crate::orchestration::workflow::WorkflowRun::new(
                &spec,
                parent_session_id,
            )),
        }
    }

    /// W0-ABI resume: load a workflow whose listed node agent-ids already completed (recovered from
    /// the session log after an interruption); the kernel continues the DAG from the remaining work.
    pub fn load_workflow_resumed(
        &mut self,
        spec: crate::orchestration::workflow::WorkflowSpec,
        parent_session_id: &str,
        submissions: &[Vec<crate::orchestration::workflow::WorkflowNode>],
        completed: &[String],
    ) -> LoopAction {
        self.install_workflow(crate::orchestration::workflow::WorkflowRun::resume(
            &spec,
            parent_session_id,
            submissions,
            completed,
        ))
    }

    fn install_workflow(
        &mut self,
        built: crate::types::error::Result<crate::orchestration::workflow::WorkflowRun>,
    ) -> LoopAction {
        match built {
            Ok(run) => {
                self.workflow = Some(run);
                self.drive_workflow(None)
            }
            Err(err) => {
                let rb = RollbackReason::GovernanceDenied {
                    tool_name: "load_workflow".to_string(),
                    reason: err.to_string(),
                };
                let note = Message::user(super::super::rollback::build_rollback_note(
                    &rb,
                    self.ctx.config.verbose_control_notes,
                ));
                self.ctx
                    .push_signal(note.content.as_text().unwrap_or_default().to_string());
                self.phase = LoopPhase::Reason;
                self.emit_call_llm()
            }
        }
    }

    /// Spawn every workflow node that is **ready now and fits under the concurrency cap**, each
    /// gated through the *deferrable* spawn quota. A transient concurrency limit (`Defer`) stops
    /// the round and leaves the remaining ready nodes untouched — a running sibling's completion
    /// will free a slot and the next [`Self::drive_workflow`] round retries them (W2-1 収口: quota
    /// backpressure = enqueue-and-retry, not permanent denial). A permanent limit (`Deny`, e.g.
    /// depth) marks the node failed so its dependents starve. Returns the freshly spawned ids and
    /// their `WorkflowSpawnInfo` (for the `WorkflowBatchSpawned` observation).
    fn spawn_ready_workflow_nodes(
        &mut self,
    ) -> (Vec<String>, Vec<crate::orchestration::workflow::WorkflowSpawnInfo>) {
        // A2 tournament: a controller node whose deps are satisfied fans out into entrant children
        // (and spawns no agent of its own) before we read the ready set — so its entrants/judges
        // are picked up by the same run-queue spawn loop as any other node.
        if let Some(run) = self.workflow.as_mut() {
            run.expand_ready_controllers();
        }
        let ready = self
            .workflow
            .as_ref()
            .map(|w| w.ready_batch())
            .unwrap_or_default();
        let mut spawned_ids: Vec<String> = Vec::new();
        let mut spawned_infos: Vec<crate::orchestration::workflow::WorkflowSpawnInfo> = Vec::new();
        for node in ready {
            // W3 quarantine stage: a quarantined node that declares write privilege is a contradiction
            // (it reads untrusted content) — deny the spawn in-kernel and starve its dependents, rather
            // than trusting the SDK to honor read-only. Equivalent to `Deny{stage:"quarantine"}`.
            if self.workflow.as_ref().is_some_and(|w| w.quarantine_violation(node)) {
                if let Some(run) = self.workflow.as_mut() {
                    run.mark_denied(node);
                }
                let rb = RollbackReason::GovernanceDenied {
                    tool_name: format!(
                        "workflow-node:{}",
                        crate::orchestration::workflow::node_agent_id(node)
                    ),
                    reason: "quarantine: quarantined node requested write-capable isolation".to_string(),
                };
                let note = Message::user(super::super::rollback::build_rollback_note(
                    &rb,
                    self.ctx.config.verbose_control_notes,
                ));
                self.ctx
                    .push_signal(note.content.as_text().unwrap_or_default().to_string());
                continue;
            }
            // Owned manifest — releases the immutable `self.workflow` borrow before the gate.
            let manifest = match self.workflow.as_ref() {
                Some(w) => w.manifest_for(node),
                None => continue,
            };
            match self.evaluate_spawn_quota_deferrable() {
                Disposition::Allow => {
                    let agent_id = manifest.agent_id.to_string();
                    let child = Tcb::spawned(&manifest, self.policy.clone());
                    self.tasks.insert(child);
                    if let Some(process) = self.tasks.get(&agent_id).and_then(AgentProcess::from_tcb) {
                        self.push_agent_process_changed(process);
                    }
                    if let Some(run) = self.workflow.as_mut() {
                        run.mark_spawned(node, &agent_id);
                    }
                    if let Some(run) = self.workflow.as_ref() {
                        spawned_infos.push(run.spawn_info(node));
                    }
                    spawned_ids.push(agent_id);
                }
                Disposition::Defer { .. } => {
                    // Concurrency cap reached: leave this node (and the rest of this round) Ready;
                    // the scheduler retries them once a running sibling frees a slot.
                    break;
                }
                _ => {
                    // Permanent denial (e.g. depth limit): the node fails; dependents starve.
                    if let Some(run) = self.workflow.as_mut() {
                        run.mark_denied(node);
                    }
                }
            }
        }
        (spawned_ids, spawned_infos)
    }

    /// Run-queue workflow executor (W2-1 収口 — the default, replacing the old batch barrier). Spawns
    /// every currently-runnable ready node, then suspends on the running set or finishes. Unlike the
    /// batch barrier, a node's dependents can start the moment *that* node completes, without waiting
    /// for the slowest sibling in its dependency layer. For DAGs with no intra-layer skew
    /// (fanout/linear) the spawn sequence is identical to the old batch path. `just_completed` is the
    /// node whose completion triggered this round (`None` on the initial install).
    fn drive_workflow(&mut self, just_completed: Option<String>) -> LoopAction {
        // Drop the just-completed node from the running set (its TCB is already terminal).
        if let Some(id) = just_completed.as_deref() {
            if let Some(SuspendState::SubAgentAwait { agent_ids }) = self.suspend_state.as_mut() {
                agent_ids.retain(|a| a != id);
            }
        }

        // Spawn everything ready that fits under the concurrency cap right now.
        let (spawned_ids, spawned_infos) = self.spawn_ready_workflow_nodes();
        if !spawned_ids.is_empty() {
            // G4: snapshot remaining budget *after* this batch's spawns are reflected in the running
            // set, so a coordinator node reads accurate headroom for its next submission.
            let budget = self.workflow_budget();
            // W0-ABI: tell the SDK which nodes to run (with their goals) before suspending.
            self.observations.push(KernelObservation::WorkflowBatchSpawned {
                turn: self.turn,
                nodes: spawned_infos,
                budget,
            });
            match self.suspend_state.as_mut() {
                Some(SuspendState::SubAgentAwait { agent_ids }) => {
                    agent_ids.extend(spawned_ids.iter().cloned());
                }
                _ => {
                    self.suspend_state = Some(SuspendState::SubAgentAwait {
                        agent_ids: spawned_ids.clone(),
                    });
                }
            }
            let wait_ids: Vec<crate::scheduler::tcb::TaskId> = match &self.suspend_state {
                Some(SuspendState::SubAgentAwait { agent_ids }) => {
                    agent_ids.iter().map(|s| s.clone().into()).collect()
                }
                _ => Vec::new(),
            };
            self.set_lifecycle(TaskState::Suspended, Some(WaitReason::SubAgentJoin(wait_ids)));
            self.observations.push(KernelObservation::Suspended {
                turn: self.turn,
                reason: "workflow_batch".to_string(),
                pending_calls: spawned_ids,
            });
        }

        // Still nodes running? keep awaiting their completions.
        let running = matches!(
            self.suspend_state.as_ref(),
            Some(SuspendState::SubAgentAwait { agent_ids }) if !agent_ids.is_empty()
        );
        if running {
            return LoopAction::AwaitingResume;
        }

        // Nothing running and nothing newly spawned → the DAG is done, or stalled because a
        // gated/denied dependency starves its dependents. Resume the parent loop.
        self.suspend_state = None;
        if let Some(id) = just_completed {
            self.observations.push(KernelObservation::Resumed {
                turn: self.turn,
                approved: vec![id],
                denied: Vec::new(),
            });
        }
        self.finish_workflow()
    }

    /// Finish the in-flight workflow: emit `WorkflowCompleted` with its outcome, clear it, and
    /// resume the parent loop. Shared by the all-gated path and the drained-no-more-ready path.
    fn finish_workflow(&mut self) -> LoopAction {
        if let Some(run) = self.workflow.as_ref() {
            let (completed, failed) = run.outcome();
            self.observations.push(KernelObservation::WorkflowCompleted {
                turn: self.turn,
                completed,
                failed,
            });
        }
        self.workflow = None;
        self.phase = LoopPhase::Reason;
        self.emit_call_llm()
    }

    /// W0/W2-1: advance the in-flight workflow after a node completed. Records the completion, then
    /// hands off to the run-queue executor [`Self::drive_workflow`], which spawns any node whose
    /// dependencies are now satisfied (without waiting for the rest of the completing node's layer)
    /// and either suspends on the still-running set or finishes the workflow.
    pub(super) fn advance_workflow(&mut self, result: SubAgentResult) -> LoopAction {
        let agent_id = result.agent_id.to_string();
        if let Some(run) = self.workflow.as_mut() {
            run.record_completion(&agent_id, result.result.clone());
        }
        self.drive_workflow(Some(agent_id))
    }
}
