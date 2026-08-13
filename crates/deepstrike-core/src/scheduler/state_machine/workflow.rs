//! Workflow orchestration impl for [`super::LoopStateMachine`].

use super::super::tcb::{TaskLifecycle, TaskSpawnError, WaitReason};
use super::{
    KernelObservation, LoopAction, LoopPhase, LoopStateMachine, PendingWorkflowSpawn, SuspendState,
};
use crate::orchestration::workflow::run::WorkflowRuntimeNodeState;
use crate::proc::AgentProcess;
use crate::syscall::{Disposition, Syscall};
use crate::types::result::SubAgentResult;

impl LoopStateMachine {
    /// Stable-checkpoint projection of the active workflow.
    pub(crate) fn workflow_checkpoint_nodes(&self) -> Option<Vec<WorkflowRuntimeNodeState>> {
        self.workflow.as_ref().map(|run| run.checkpoint_nodes())
    }

    /// Install a workflow rebuilt from checkpoint source state and recreate the derived wait and
    /// pending-spawn views that the next child completion/effect resolution reads.
    pub(crate) fn restore_checkpoint_workflow(
        &mut self,
        mut run: crate::orchestration::workflow::WorkflowRun,
    ) {
        run.set_scheduler_policy(self.scheduler_policy);
        let active: Vec<(String, crate::orchestration::workflow::WorkflowSpawnInfo)> = run
            .checkpoint_nodes()
            .iter()
            .enumerate()
            .filter_map(|(node, state)| {
                state
                    .active_agent_id
                    .as_ref()
                    .map(|agent_id| (agent_id.clone(), run.spawn_info(node)))
            })
            .collect();
        self.workflow = Some(run);

        let active_ids: Vec<String> = active
            .iter()
            .map(|(agent_id, _)| agent_id.clone())
            .collect();
        self.suspend_state = (!active_ids.is_empty()).then(|| SuspendState::SubAgentAwait {
            agent_ids: active_ids,
        });

        let pending_nodes: Vec<_> = active
            .into_iter()
            .filter_map(|(agent_id, info)| {
                self.tasks
                    .get(&agent_id)
                    .is_some_and(|task| {
                        matches!(
                            task.state,
                            TaskLifecycle::PendingLaunch | TaskLifecycle::Starting
                        )
                    })
                    .then_some(info)
            })
            .collect();
        self.pending_workflow_spawn = (!pending_nodes.is_empty()).then(|| PendingWorkflowSpawn {
            nodes: pending_nodes,
            budget: self.workflow_budget(),
        });
    }

    /// Whether a workflow DAG is currently in flight.
    pub fn workflow_active(&self) -> bool {
        self.workflow.is_some()
    }

    /// How many nodes the in-flight DAG holds. `0` when none is loaded. The quantity
    /// `Syscall::SubmitNodes` is metered against, and what a host projection reports.
    pub fn workflow_node_count(&self) -> usize {
        self.workflow.as_ref().map(|run| run.len()).unwrap_or(0)
    }

    /// W0: load a workflow DAG and spawn its first gated batch. On an invalid spec (cycle /
    /// out-of-range dependency) the workflow is not installed and the rejection is surfaced as a
    /// committed control result.
    pub fn load_workflow(
        &mut self,
        spec: crate::orchestration::workflow::WorkflowSpec,
    ) -> LoopAction {
        self.load_workflow_from(spec, None)
    }

    /// Canonical-driver entrypoint: the first launch is parented by the caller the kernel derived
    /// from execution focus/syscall causation, never by a host-supplied identity.
    pub(crate) fn load_workflow_as(
        &mut self,
        spec: crate::orchestration::workflow::WorkflowSpec,
        caller: &str,
    ) -> LoopAction {
        self.load_workflow_from(spec, Some(caller.into()))
    }

    fn load_workflow_from(
        &mut self,
        spec: crate::orchestration::workflow::WorkflowSpec,
        caller: Option<crate::scheduler::tcb::TaskId>,
    ) -> LoopAction {
        self.install_workflow(
            crate::orchestration::workflow::WorkflowRun::new(&spec),
            "load_workflow",
            None,
            caller,
        )
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
        if !self.append_workflow_nodes(nodes, submitter_agent_id, syscall, tool_label) {
            return LoopAction::AwaitingResume;
        }
        // `submitter_agent_id` is a legacy audit/trust label, not caller authority. Canonical
        // callers enter only through `drive_workflow_round` after kernel causation derivation.
        self.drive_workflow(None, None)
    }

    /// §10.3 · the gate + trust-aware append, **without** the drive.
    ///
    /// Split out of [`Self::append_nodes_gated`] for one reason: a `ChildCompleted`'s
    /// `parent_requests` are adjudicated *before* the completion is fed, and the completion's own
    /// `drive_workflow` is what produces the next ready batch. Driving here as well would reserve
    /// two spawn batches in a single transition and the second `pending_workflow_spawn` would
    /// silently replace the first. Returns whether the batch was admitted — a caller that drives
    /// (the provider-tool path) drives only on `true`.
    pub fn append_workflow_nodes(
        &mut self,
        nodes: Vec<crate::orchestration::workflow::WorkflowNode>,
        submitter_agent_id: Option<&str>,
        syscall: Syscall,
        tool_label: &str,
    ) -> bool {
        let syscall = match syscall {
            Syscall::SubmitNodes { .. } => Syscall::SubmitNodes { count: nodes.len() },
            Syscall::LoadWorkflow { .. } => Syscall::LoadWorkflow {
                node_count: nodes.len(),
            },
            other => other,
        };
        // R3-1 governance: gate DAG growth through the syscall trap. A `max_workflow_nodes` quota
        // denies a submission that would grow the workflow past the cap (runaway loop-until-done
        // backstop); the workflow continues with its existing nodes and a rejection note is surfaced.
        let disposition = self.evaluate_syscall(&syscall);
        if !disposition.is_allowed() {
            let reason = match &disposition {
                Disposition::Deny { reason, .. } => reason.clone(),
                _ => "workflow node submission denied".to_string(),
            };
            let note = super::super::rollback::build_control_rejection_note(
                tool_label,
                &reason,
                self.ctx.config.verbose_control_notes,
            );
            self.ctx.push_signal(note);
            self.observations
                .push(KernelObservation::ControlRequestRejected {
                    turn: self.turn,
                    operation: tool_label.to_string(),
                    subject: submitter_agent_id.map(str::to_string),
                    reason,
                });
            return false;
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
                    return false;
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
        true
    }

    /// §10.3 · run one spawn round over the current DAG. The public half of
    /// [`Self::drive_workflow`], for the canonical driver's syscall reductions — an append that was
    /// admitted still needs its next ready batch, and the driver decides when that happens relative
    /// to the completion it is also folding.
    pub(crate) fn drive_workflow_round(&mut self, caller: &str) -> LoopAction {
        self.drive_workflow(None, Some(caller.into()))
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
                    let note = super::super::rollback::build_control_rejection_note(
                        "start_workflow",
                        &reason,
                        self.ctx.config.verbose_control_notes,
                    );
                    self.ctx.push_signal(note);
                    self.observations
                        .push(KernelObservation::ControlRequestRejected {
                            turn: self.turn,
                            operation: "start_workflow".to_string(),
                            subject: submitter_agent_id.map(str::to_string),
                            reason,
                        });
                    self.phase = LoopPhase::Reason;
                    return self.emit_call_llm();
                }
                // W-3: announce the bootstrap batch like any other submission (base 0), so the SDK
                // can persist an agent-authored workflow's nodes and reconstruct them on resume —
                // the host never had this spec, unlike the `load_workflow` path.
                let node_count = spec.nodes.len();
                let built = crate::orchestration::workflow::WorkflowRun::new(&spec);
                if built.is_ok() {
                    self.observations
                        .push(KernelObservation::WorkflowNodesSubmitted {
                            turn: self.turn,
                            base: 0,
                            count: node_count as u32,
                            submitter: submitter_agent_id.map(str::to_string),
                        });
                }
                self.install_workflow(built, "start_workflow", submitter_agent_id, None)
            }
        }
    }

    fn install_workflow(
        &mut self,
        built: crate::types::error::Result<crate::orchestration::workflow::WorkflowRun>,
        operation: &str,
        subject: Option<&str>,
        caller: Option<crate::scheduler::tcb::TaskId>,
    ) -> LoopAction {
        match built {
            Ok(mut run) => {
                run.set_scheduler_policy(self.scheduler_policy);
                self.workflow = Some(run);
                self.drive_workflow(None, caller)
            }
            Err(err) => {
                let note = super::super::rollback::build_control_rejection_note(
                    operation,
                    &err.to_string(),
                    self.ctx.config.verbose_control_notes,
                );
                self.ctx.push_signal(note);
                self.observations
                    .push(KernelObservation::ControlRequestRejected {
                        turn: self.turn,
                        operation: operation.to_string(),
                        subject: subject.map(str::to_string),
                        reason: err.to_string(),
                    });
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
        caller: Option<&crate::scheduler::tcb::TaskId>,
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
                let operation = format!(
                    "workflow-node:{}",
                    crate::orchestration::workflow::node_agent_id(node)
                );
                let note = super::super::rollback::build_control_rejection_note(
                    &operation,
                    "quarantine: quarantined node requested write-capable isolation",
                    self.ctx.config.verbose_control_notes,
                );
                self.ctx.push_signal(note);
                continue;
            }
            // Owned manifest — releases the immutable `self.workflow` borrow before the gate.
            let manifest = match self.workflow.as_ref() {
                Some(w) => w.manifest_for(node),
                None => continue,
            };
            match self.evaluate_spawn_quota_deferrable(caller.map(|id| id.as_str()), &manifest) {
                Disposition::Allow => {
                    let agent_id = manifest.agent_id.to_string();
                    // §10.4: mint identity here and stop at `PendingLaunch` — the child is a
                    // committed kernel fact, not yet a running process.
                    // SPC-019-02: canonical callers arrive from execution focus/syscall
                    // causation. Legacy entrypoints omit one and retain the structural root.
                    let parent = caller.cloned().or_else(|| self.tasks.root_id());
                    let Some(parent) = parent else {
                        if let Some(run) = self.workflow.as_mut() {
                            run.mark_denied(node);
                        }
                        continue;
                    };
                    if let Err(error) = self.tasks.spawn_child(
                        parent.as_str(),
                        &manifest,
                        self.policy.clone(),
                        TaskLifecycle::PendingLaunch,
                    ) {
                        if let Some(run) = self.workflow.as_mut() {
                            run.mark_denied(node);
                        }
                        let reason = match error {
                            TaskSpawnError::UnknownCaller => "unknown caller",
                            TaskSpawnError::CallerTerminal => "terminal caller",
                            TaskSpawnError::DuplicateTask => "duplicate task identity",
                        };
                        self.observations
                            .push(KernelObservation::ControlRequestRejected {
                                turn: self.turn,
                                operation: "spawn_workflow_node".to_string(),
                                subject: Some(agent_id),
                                reason: format!("kernel-owned child creation rejected {reason}"),
                            });
                        continue;
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
    fn drive_workflow(
        &mut self,
        just_completed: Option<String>,
        caller: Option<crate::scheduler::tcb::TaskId>,
    ) -> LoopAction {
        // Drop the just-completed node from the running set (its TCB is already terminal).
        if let Some(id) = just_completed.as_deref() {
            if let Some(SuspendState::SubAgentAwait { agent_ids }) = self.suspend_state.as_mut() {
                agent_ids.retain(|a| a != id);
            }
        }

        // Spawn everything ready that fits under the concurrency cap right now.
        let (spawned_ids, spawned_infos) = self.spawn_ready_workflow_nodes(caller.as_ref());
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
    /// **fork on root kind** (spec §6.1 invariant 7, §10.1). Shared by the all-gated path and the
    /// drained-no-more-ready path.
    ///
    /// * a workflow *nested* inside an agent root resumes the parent agent — the completion is one
    ///   step of that agent's turn, so the loop calls the provider again;
    /// * a **root** workflow's completion *is* the operation's terminal. It emits no provider call:
    ///   the historical unconditional `emit_call_llm()` here is the sole reason `CompleteRun`
    ///   existed, because a host had to commit the terminal before that extra call was executed.
    ///   The root task is closed and the driver reads the `WorkflowCompleted` observation to build
    ///   the workflow terminal.
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
        if self.root_workflow {
            self.set_lifecycle(
                TaskLifecycle::Done(crate::types::result::TerminationReason::Completed),
                None,
            );
            return LoopAction::AwaitingResume;
        }
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
        self.drive_workflow(Some(agent_id.clone()), Some(agent_id.into()))
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
            // spc_005-05: a no-op today (workflow-node spawns never set `requested_budget`), kept
            // for parity with every other terminal-transition site so a future workflow-budget
            // wiring does not have to remember to add it here too.
            self.tasks.return_child_budget(failure.agent_id.as_str());
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
            // §10.4 / §15.3: this acknowledgement is the *only* transition that makes a task
            // `Running`. Before it the task was `PendingLaunch`/`Starting` — identity the kernel
            // minted — so the host confirms an execution, it never creates one.
            if let Some(task) = self.tasks.get_mut(node.agent_id.as_str())
                && !task.state.is_terminal()
            {
                task.state = TaskLifecycle::Running;
            }
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
            self.drive_workflow(None, None)
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
