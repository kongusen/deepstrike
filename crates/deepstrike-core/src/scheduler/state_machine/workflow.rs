//! Workflow orchestration impl for [`super::LoopStateMachine`].

use super::super::tcb::{TaskLifecycle, Tcb, WaitReason};
use super::{
    KernelObservation, LoopAction, LoopPhase, LoopStateMachine, PendingWorkflowSpawn, SuspendState,
};
use crate::proc::AgentProcess;
use crate::runtime::session::RollbackReason;
use crate::syscall::{Disposition, Syscall};
use crate::types::message::Message;
use crate::types::result::SubAgentResult;

impl LoopStateMachine {
    pub(crate) fn validate_workflow_spawn_result(
        &self,
        started_agent_ids: &[String],
        failures: &[crate::runtime::kernel::WorkflowSpawnFailure],
        error: Option<&str>,
    ) -> Result<(), String> {
        let pending = self
            .pending_workflow_spawn
            .as_ref()
            .ok_or_else(|| "workflow spawn result has no pending batch".to_string())?;
        if error.is_some() {
            return if started_agent_ids.is_empty() && failures.is_empty() {
                Ok(())
            } else {
                Err("batch error cannot include per-agent results".to_string())
            };
        }

        let expected: std::collections::HashSet<&str> = pending
            .nodes
            .iter()
            .map(|node| node.agent_id.as_str())
            .collect();
        let mut actual = std::collections::HashSet::with_capacity(expected.len());
        for agent_id in started_agent_ids
            .iter()
            .map(String::as_str)
            .chain(failures.iter().map(|failure| failure.agent_id.as_str()))
        {
            if !actual.insert(agent_id) {
                return Err(format!(
                    "duplicate workflow spawn result for agent {agent_id}"
                ));
            }
        }
        if actual != expected {
            return Err(
                "workflow spawn result must resolve every requested agent exactly once".to_string(),
            );
        }
        Ok(())
    }

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
        self.append_nodes_gated(
            nodes,
            submitter_agent_id,
            Syscall::SubmitNodes { count: 0 },
            "submit_workflow_nodes",
        )
    }

    /// W-7: the ONE gated append shared by `submit_workflow_nodes` and `submit_workflow`'s flatten
    /// arm — gate → deny-note → trust-aware append → `WorkflowNodesSubmitted` observation → drive.
    /// `syscall` names the gate variant (its count is filled from `nodes.len()` here so the two
    /// entry points cannot disagree on what they meter).
    fn append_nodes_gated(
        &mut self,
        nodes: Vec<crate::orchestration::workflow::WorkflowNode>,
        submitter_agent_id: Option<&str>,
        syscall: Syscall,
        tool_label: &str,
    ) -> LoopAction {
        let syscall = match syscall {
            Syscall::SubmitNodes { .. } => Syscall::SubmitNodes { count: nodes.len() },
            Syscall::LoadWorkflow { .. } => Syscall::LoadWorkflow {
                node_count: nodes.len(),
            },
            other => other,
        };
        // R3-1 governance: gate DAG growth through the syscall trap. A `max_workflow_nodes` quota
        // denies a submission that would grow the workflow past the cap (runaway loop-until-done
        // backstop); the workflow continues with its existing nodes and a rollback note is surfaced.
        let disposition = self.evaluate_syscall(&syscall);
        if !disposition.is_allowed() {
            let reason = match &disposition {
                Disposition::Deny { reason, .. } => reason.clone(),
                _ => "workflow node submission denied".to_string(),
            };
            let rb = RollbackReason::GovernanceDenied {
                tool_name: tool_label.to_string(),
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
        let submission = self
            .workflow
            .as_mut()
            .map(|run| run.submit_nodes_from(submitter_agent_id, nodes));
        if let Some(submission) = submission {
            // G1: route through the trust-aware entry point — a quarantined submitter's nodes are
            // coerced to quarantined in-kernel before append (no topological privilege escalation).
            let appended = match submission {
                Ok(appended) => appended,
                Err(error) => {
                    self.observations.push(KernelObservation::NodesRejected {
                        turn: self.turn,
                        node_index: error.node_index as u32,
                        reason: error.reason,
                    });
                    return LoopAction::AwaitingResume;
                }
            };
            if let Some(&base) = appended.first() {
                // R3-1: surface the batch's base index so the SDK-persisted
                // `workflow_nodes_submitted` record lets resume rebuild exact indices.
                self.observations
                    .push(KernelObservation::WorkflowNodesSubmitted {
                        turn: self.turn,
                        base: base as u32,
                        count: appended.len() as u32,
                        submitter: submitter_agent_id.map(str::to_string),
                    });
            }
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
        if self.workflow.is_some() {
            // Flatten: caller is a workflow node; grow the existing DAG (G1 coercion applies).
            // Same gate + append + observation as `submit_workflow_nodes` (W-7: one decision).
            self.append_nodes_gated(
                spec.nodes,
                submitter_agent_id,
                Syscall::LoadWorkflow { node_count: 0 },
                "start_workflow",
            )
        } else {
            // Bootstrap: top-level agent starts a brand-new workflow in this same kernel.
            {
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
                // W-3: announce the bootstrap batch like any other submission (base 0), so the SDK
                // can persist an agent-authored workflow's nodes and reconstruct them on resume —
                // the host never had this spec, unlike the `load_workflow` path.
                let node_count = spec.nodes.len();
                let built =
                    crate::orchestration::workflow::WorkflowRun::new(&spec, parent_session_id);
                if built.is_ok() {
                    self.observations
                        .push(KernelObservation::WorkflowNodesSubmitted {
                            turn: self.turn,
                            base: 0,
                            count: node_count as u32,
                            submitter: submitter_agent_id.map(str::to_string),
                        });
                }
                self.install_workflow(built)
            }
        }
    }

    /// Load a workflow whose typed terminal outcomes were recovered from the session journal.
    /// `outcomes` carries status, termination, output and control signals so dependency semantics
    /// faithfully — see [`crate::orchestration::workflow::ResumedNodeOutcome`].
    pub fn load_workflow_resumed(
        &mut self,
        spec: crate::orchestration::workflow::WorkflowSpec,
        parent_session_id: &str,
        submissions: &[Vec<crate::orchestration::workflow::WorkflowNode>],
        submission_bases: &[u32],
        outcomes: &[crate::orchestration::workflow::ResumedNodeOutcome],
    ) -> LoopAction {
        self.install_workflow(crate::orchestration::workflow::WorkflowRun::resume(
            &spec,
            parent_session_id,
            submissions,
            submission_bases,
            outcomes,
        ))
    }

    fn install_workflow(
        &mut self,
        built: crate::types::error::Result<crate::orchestration::workflow::WorkflowRun>,
    ) -> LoopAction {
        match built {
            Ok(mut run) => {
                run.set_scheduler_policy(self.scheduler_policy);
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
    ) -> (
        Vec<String>,
        Vec<crate::orchestration::workflow::WorkflowSpawnInfo>,
    ) {
        // A2 tournament: a controller node whose deps are satisfied fans out into entrant children
        // (and spawns no agent of its own) before we read the ready set — so its entrants/judges
        // are picked up by the same run-queue spawn loop as any other node.
        if let Some(run) = self.workflow.as_mut() {
            run.expand_ready_controllers();
        }
        let ready = self
            .workflow
            .as_mut()
            .map(|w| w.ready_batch())
            .unwrap_or_default();
        let mut spawned_ids: Vec<String> = Vec::new();
        let mut spawned_infos: Vec<crate::orchestration::workflow::WorkflowSpawnInfo> = Vec::new();
        for node in ready {
            // W3 quarantine stage: a quarantined node that declares write privilege is a contradiction
            // (it reads untrusted content) — deny the spawn in-kernel and starve its dependents, rather
            // than trusting the SDK to honor read-only. Equivalent to `Deny{stage:"quarantine"}`.
            if self
                .workflow
                .as_ref()
                .is_some_and(|w| w.quarantine_violation(node))
            {
                if let Some(run) = self.workflow.as_mut() {
                    run.mark_denied(node);
                }
                let rb = RollbackReason::GovernanceDenied {
                    tool_name: format!(
                        "workflow-node:{}",
                        crate::orchestration::workflow::node_agent_id(node)
                    ),
                    reason: "quarantine: quarantined node requested write-capable isolation"
                        .to_string(),
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
                    // Permanent denial (e.g. depth limit): the node fails and dependency policy
                    // deterministically promotes or skips its descendants.
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
            self.set_lifecycle(
                TaskLifecycle::Suspended,
                Some(WaitReason::SubAgentJoin(wait_ids)),
            );
            self.pending_workflow_spawn = Some(PendingWorkflowSpawn {
                nodes: spawned_infos.clone(),
                budget: budget.clone(),
            });
            return LoopAction::SpawnWorkflow {
                nodes: spawned_infos,
                budget,
            };
        }

        // Still nodes running? keep awaiting their completions.
        let running = matches!(
            self.suspend_state.as_ref(),
            Some(SuspendState::SubAgentAwait { agent_ids }) if !agent_ids.is_empty()
        );
        if running {
            return LoopAction::AwaitingResume;
        }

        // Nothing running and nothing newly spawned → close every remaining node and resume the
        // parent loop. Dependency propagation normally closes blocked descendants before this;
        // `finish_workflow` performs the final invariant sweep.
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
        if let Some(run) = self.workflow.as_mut() {
            let node_outcomes = run.finish();
            self.observations
                .push(KernelObservation::WorkflowCompleted {
                    turn: self.turn,
                    node_outcomes,
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

    /// Commit a host workflow-spawn result. Only agents acknowledged as started
    /// become process facts; failed agents are removed from the live wait set and
    /// fail their workflow nodes before the DAG is driven again.
    pub fn resolve_workflow_spawn(
        &mut self,
        started_agent_ids: Vec<String>,
        failures: Vec<crate::runtime::kernel::WorkflowSpawnFailure>,
    ) -> LoopAction {
        let Some(pending) = self.pending_workflow_spawn.take() else {
            return LoopAction::AwaitingResume;
        };

        let failed_ids: std::collections::HashSet<&str> = failures
            .iter()
            .map(|failure| failure.agent_id.as_str())
            .collect();
        for failure in &failures {
            if let Some(task) = self.tasks.get_mut(failure.agent_id.as_str()) {
                task.state = TaskLifecycle::Done(crate::types::result::TerminationReason::Error);
            }
            if let Some(run) = self.workflow.as_mut() {
                run.mark_spawn_failed(&failure.agent_id);
            }
        }
        if let Some(SuspendState::SubAgentAwait { agent_ids }) = self.suspend_state.as_mut() {
            agent_ids.retain(|agent_id| !failed_ids.contains(agent_id.as_str()));
        }

        let started: std::collections::HashSet<&str> =
            started_agent_ids.iter().map(String::as_str).collect();
        let started_nodes: Vec<_> = pending
            .nodes
            .into_iter()
            .filter(|node| started.contains(node.agent_id.as_str()))
            .collect();
        for node in &started_nodes {
            if let Some(process) = self
                .tasks
                .get(&node.agent_id)
                .and_then(AgentProcess::from_tcb)
            {
                self.push_agent_process_changed(process);
            }
        }
        if !started_nodes.is_empty() {
            self.observations
                .push(KernelObservation::WorkflowBatchSpawned {
                    turn: self.turn,
                    nodes: started_nodes,
                    budget: pending.budget,
                });
            self.observations.push(KernelObservation::Suspended {
                turn: self.turn,
                reason: "workflow_batch".to_string(),
                pending_calls: started_agent_ids,
            });
        }

        let running = matches!(
            self.suspend_state.as_ref(),
            Some(SuspendState::SubAgentAwait { agent_ids }) if !agent_ids.is_empty()
        );
        if running {
            LoopAction::AwaitingResume
        } else {
            self.suspend_state = None;
            self.drive_workflow(None)
        }
    }

    /// A batch-level host failure leaves the reserved spawn intent intact and
    /// reissues it without recording any node as started.
    pub fn retry_workflow_spawn(&mut self, error: String) -> LoopAction {
        self.observations
            .push(KernelObservation::WorkflowSpawnFailed {
                turn: self.turn,
                error,
            });
        let pending = self
            .pending_workflow_spawn
            .as_ref()
            .expect("workflow spawn failure requires pending intent");
        LoopAction::SpawnWorkflow {
            nodes: pending.nodes.clone(),
            budget: pending.budget.clone(),
        }
    }
}
