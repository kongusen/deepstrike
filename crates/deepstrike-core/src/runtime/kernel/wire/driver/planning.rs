use super::*;

impl CanonicalOperationDriver {
    // ----- the plan function -----

    /// Plan one input.
    ///
    /// Pass this to [`KernelTransaction::prepare`](super::super::transaction::KernelTransaction::prepare).
    /// The focus/root-kind fold does **not** advance here — call [`Self::note_committed`] once the
    /// host's append and the transaction's commit have both succeeded.
    pub fn plan(&mut self, context: &PlanContext<'_>) -> Result<PlannedStep, KernelFault> {
        if let Some(fault) = &self.poison {
            return Err(fault.clone());
        }
        if let Some(staged) = &self.staged {
            let staged_seq = staged.step_seq;
            return Err(self.poison_with(KernelFault::new(
                KernelFaultCode::TransactionConflict,
                format!(
                    "the driver still holds the plan of step {staged_seq}; its transition never \
                     committed while the semantic kernel already advanced under it, so this \
                     runtime no longer describes the journal — rebuild from the records"
                ),
            )));
        }
        let mut step = self.plan_inner(context)?;
        step.observations = self
            .engine
            .as_mut()
            .map(LoopStateMachine::take_observations)
            .unwrap_or_default();
        self.staged = Some(StagedFocus {
            step_seq: context.step_seq,
            root_kind: step.root_kind,
            focus: step.focus.clone(),
        });
        Ok(step)
    }

    /// Install the staged fold after the transaction committed the record (§7.4: a focus moves only
    /// on a committed transition).
    pub fn note_committed(&mut self, step_seq: WireU64) -> Result<(), KernelFault> {
        if let Some(fault) = &self.poison {
            return Err(fault.clone());
        }
        let Some(staged) = self.staged.take() else {
            return Err(self.poison_with(KernelFault::new(
                KernelFaultCode::TransactionConflict,
                format!("step {step_seq} committed, but the driver planned no such step"),
            )));
        };
        if staged.step_seq != step_seq {
            let planned = staged.step_seq;
            return Err(self.poison_with(KernelFault::new(
                KernelFaultCode::TransactionConflict,
                format!("step {step_seq} committed, but the driver planned step {planned}"),
            )));
        }
        if let Some(kind) = staged.root_kind {
            self.root_kind = Some(kind);
        }
        self.focus = staged.focus;
        Ok(())
    }

    /// Plan **and** fold in one call — the shape
    /// [`rebuild_from_records`](super::super::transaction::KernelTransaction::rebuild_from_records) needs,
    /// where every record it replays is by definition already durable.
    pub fn fold(&mut self, context: &PlanContext<'_>) -> Result<PlannedStep, KernelFault> {
        let step = self.plan(context)?;
        self.note_committed(context.step_seq)?;
        Ok(step)
    }

    // ----- §10.2 · the agent-authored workflow seam -----

    /// Enter a workflow the agent asked for, inside an agent root (§10.2).
    ///
    /// This is the P1 reduction point Task 10 wires its `SyscallRequest::SubmitWorkflow` gate to;
    /// the authority rules it enforces are already the final ones:
    ///
    /// * the root kind stays `Agent` — a syscall never re-roots an operation;
    /// * the focus moves to `WorkflowController { parent_task_id: Some(agent task) }`;
    /// * depth is at most 1. Asking for a workflow while the focus already *is* a
    ///   `WorkflowController` is an `InvalidAuthority` fault with zero mutation — workflows do not
    ///   stack (§15.4).
    pub fn begin_nested_workflow(
        &mut self,
        context: &PlanContext<'_>,
        spec: &WireSpec,
    ) -> Result<PlannedStep, KernelFault> {
        if let Some(fault) = &self.poison {
            return Err(fault.clone());
        }
        let mut index = 0;
        let outcome = self
            .enter_nested_workflow(context, spec, &mut index)
            .map_err(|refusal| match refusal {
                SyscallRefusal::Fault(fault) => fault,
                // A direct caller has no observation channel, so the gate's denial becomes the
                // transition's refusal. Through the P1 path the same denial is an audit fact.
                SyscallRefusal::Rejected(rejected) => {
                    KernelFault::new(KernelFaultCode::ResourceLimitExceeded, rejected.reason)
                }
            })?;
        let step = PlannedStep {
            root_kind: Some(RootKind::Agent),
            focus: outcome.focus,
            observations: self
                .engine
                .as_mut()
                .map(LoopStateMachine::take_observations)
                .unwrap_or_default(),
            disposition: StepDisposition::Effects(EffectsDisposition {
                effects: outcome.effects,
            }),
        };
        self.staged = Some(StagedFocus {
            step_seq: context.step_seq,
            root_kind: step.root_kind,
            focus: step.focus.clone(),
        });
        Ok(step)
    }

    /// The body of [`Self::begin_nested_workflow`], without the staging — so a syscall batch that
    /// also carries other requests composes it instead of racing it for the staging slot.
    pub(super) fn enter_nested_workflow(
        &mut self,
        context: &PlanContext<'_>,
        spec: &WireSpec,
        effect_index: &mut u32,
    ) -> Result<SyscallOutcome, SyscallRefusal> {
        let staged = self.staged.as_ref().map(|staged| staged.focus.clone());
        let focus = staged.as_ref().unwrap_or(&self.focus);
        let root_kind = self.root_kind;

        let parent_task_id = match (root_kind, focus) {
            (Some(RootKind::Agent), Some(ExecutionFocus::AgentTurn(turn))) => turn.task_id.clone(),
            (Some(RootKind::Agent), Some(ExecutionFocus::WorkflowController(_))) => {
                return Err(authority(
                    "a workflow is already the execution focus; workflows do not stack, so a \
                     second start request is refused with no spawn effect (§7.4 focus depth ≤ 1)",
                ));
            }
            (Some(RootKind::Workflow), _) => {
                return Err(authority(
                    "this operation's root is a workflow; its focus never moves, and a nested \
                     workflow start is not a transition it admits (§7.4)",
                ));
            }
            _ => {
                return Err(SyscallRefusal::Fault(KernelFault::new(
                    KernelFaultCode::InvalidLifecycle,
                    "no root has started, so there is no agent turn to suspend".to_string(),
                )));
            }
        };

        for node in &spec.nodes {
            self.require_known_contract(context.config, node.run_spec.as_ref())
                .map_err(|fault| {
                    SyscallRefusal::Rejected(SyscallRejection::new("start_workflow", fault.message))
                })?;
        }
        let core_spec = build_core_spec(spec).map_err(SyscallRefusal::Fault)?;
        let node_ids = wire_node_ids(spec);
        let workflow_id = mint_workflow_id(&context.input.operation_id, context.step_seq);
        self.require_effect_support(context.config, EffectKindTag::SpawnTasks)
            .map_err(SyscallRefusal::Fault)?;

        // §10.2 · the resource gate runs before the DAG is installed, so a denial commits with no
        // spawn effect at all rather than with a workflow the run cannot afford.
        let engine = self.engine_mut().map_err(SyscallRefusal::Fault)?;
        let disposition = engine.gate_syscall(&CoreSyscall::LoadWorkflow {
            node_count: spec.nodes.len(),
        });
        if !disposition.is_allowed() {
            return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                "start_workflow",
                denial_reason(&disposition, "workflow authoring denied"),
            )));
        }

        // ----- past this line the semantic engine advances -----
        engine.set_root_workflow(false);
        let action = engine.load_workflow_as(core_spec, parent_task_id.as_str());
        self.node_ids = node_ids;
        self.workflow_nodes = spec.nodes.clone();
        self.workflow_id = Some(workflow_id.clone());
        let disposition = self
            .disposition_for_at(context, action, RootKind::Agent, effect_index)
            .map_err(SyscallRefusal::Fault)?;
        let StepDisposition::Effects(effects) = disposition else {
            return Err(SyscallRefusal::Fault(KernelFault::new(
                KernelFaultCode::InvalidLifecycle,
                "entering a nested workflow cannot terminate the operation".to_string(),
            )));
        };
        Ok(SyscallOutcome {
            effects: effects.effects,
            focus: Some(ExecutionFocus::workflow_controller(
                workflow_id,
                Some(parent_task_id),
            )),
            needs_workflow_round: false,
            ack: None,
        })
    }

    pub(super) fn mint_effect(
        &self,
        context: &PlanContext<'_>,
        effect: EffectKind,
        effect_index: &mut u32,
    ) -> KernelEffect {
        let effect_id =
            mint_effect_id(&context.input.operation_id, context.step_seq, *effect_index);
        *effect_index += 1;
        KernelEffect {
            effect_id,
            causation_input_id: context.input.input_id.clone(),
            effect,
        }
    }

    /// Fold one engine action's effects into an accumulating step.
    pub(super) fn extend_with_action(
        &mut self,
        context: &PlanContext<'_>,
        action: LoopAction,
        root_kind: RootKind,
        effect_index: &mut u32,
        effects: &mut Vec<KernelEffect>,
    ) -> Result<(), KernelFault> {
        match self.disposition_for_at(context, action, root_kind, effect_index)? {
            StepDisposition::Effects(published) => {
                effects.extend(published.effects);
                Ok(())
            }
            StepDisposition::Terminal(_) => Err(KernelFault::new(
                KernelFaultCode::InvalidLifecycle,
                "a syscall batch cannot terminate the operation; §7.12 admits effects or a \
                 terminal, never both in one step"
                    .to_string(),
            )),
        }
    }

    // ----- internals -----

    pub(super) fn plan_inner(
        &mut self,
        context: &PlanContext<'_>,
    ) -> Result<PlannedStep, KernelFault> {
        // Every transition reads the observations *its own* semantic call produced. `start`/`feed`
        // clear the buffer themselves; `load_workflow` and `resolve_workflow_spawn` do not, so the
        // driver clears it here rather than letting a stale `WorkflowCompleted` from an earlier
        // step decide a later one's disposition.
        if let Some(engine) = self.engine.as_mut() {
            engine.take_observations();
            // §11.2 · the envelope's accepted time is this operation's only clock, and it is fed
            // once, here, before any semantic call. Every clock-dependent decision the step makes
            // (signal TTL and deadline escalation, rate-limit windows, the wall-time budget axis)
            // therefore reads a fact the journal already holds, so a replay decides identically.
            engine.observe_accepted_time(context.input.observed_at_ms.get());
            let woken = engine
                .task_table_mut()
                .wake_expired_timers(context.input.observed_at_ms.get());
            if !woken.is_empty() {
                engine.observe_local_runnable_tasks();
            }
        }
        match &context.input.input {
            NormalizedPayload::ConfigureOperation(configure) => {
                self.plan_configure(&configure.config)
            }
            NormalizedPayload::StartOperation(start) => {
                self.plan_start(context, &start.entry, &start.initial_context)
            }
            NormalizedPayload::ResolveEffect(resolve) => self.plan_resolve_effect(context, resolve),
            NormalizedPayload::DeliverExternalEvent(event) => {
                self.plan_external_event(context, &event.event)
            }
            NormalizedPayload::HostControl(control) => {
                self.plan_host_control(context, &control.command)
            }
        }
    }

    /// §6.1.2 · genesis. The engine is built from the **resolved** configuration the record froze,
    /// never from this binary's defaults, so a rebuild on a newer kernel plans the same step.
    pub(super) fn plan_configure(
        &mut self,
        config: &ResolvedOperationConfig,
    ) -> Result<PlannedStep, KernelFault> {
        self.engine = Some(build_engine(config));
        // §13.2 · the live-mutable half starts at revision 0, holding exactly what the genesis
        // record froze. A patch rebases onto this, never onto a compile-time default.
        self.policy = Some(LivePolicyState::new(config.clone()));
        Ok(PlannedStep::quiet(None, None))
    }

    /// §7.4 · the one atomic root start.
    ///
    /// Both arms are ordered the same way and for the same reason: every refusal this transition
    /// can raise is decided while nothing has moved, and only then does the semantic engine
    /// advance. A rejected root start therefore leaves an operation that is still `Configured` and
    /// still free to choose a root.
    pub(super) fn plan_start(
        &mut self,
        context: &PlanContext<'_>,
        entry: &RootEntry,
        initial: &InitialContext,
    ) -> Result<PlannedStep, KernelFault> {
        if self.root_kind.is_some() || self.staged.is_some() {
            return Err(KernelFault::new(
                KernelFaultCode::InvalidLifecycle,
                "this operation already has a root; a root entry is chosen once and is immutable \
                 (§6.1.3–6.1.5)"
                    .to_string(),
            ));
        }

        match entry {
            RootEntry::Agent(agent) => {
                self.require_effect_support(context.config, EffectKindTag::CallProvider)?;
                self.require_known_contract(context.config, agent.run_spec.as_ref())?;
                let task = runtime_task(&agent.task);
                let run_spec = agent.run_spec.as_ref().map(agent_run_spec);

                // ----- past this line the semantic engine advances -----
                // The cascade is installed before `start`, which is the engine's own precondition:
                // a contract loaded afterwards would leave phase 0 already behind the run.
                self.load_verification_contract(context.config, agent.run_spec.as_ref())?;
                let engine = self.engine_mut()?;
                seed_initial_context(engine, initial);
                engine.run_spec = run_spec;
                let action = engine.start(task);
                let disposition = self.disposition_for(context, action, RootKind::Agent)?;
                if !publishes(&disposition, EffectKindTag::CallProvider) {
                    return Err(KernelFault::new(
                        KernelFaultCode::InvalidLifecycle,
                        "an agent root's first committed step must publish a provider call (§7.4)"
                            .to_string(),
                    ));
                }
                Ok(PlannedStep {
                    root_kind: Some(RootKind::Agent),
                    focus: Some(ExecutionFocus::agent_turn(root_task_id())),
                    observations: Vec::new(),
                    disposition,
                })
            }
            RootEntry::Workflow(workflow) => {
                self.require_effect_support(context.config, EffectKindTag::SpawnTasks)?;
                for node in &workflow.spec.nodes {
                    self.require_known_contract(context.config, node.run_spec.as_ref())?;
                }
                if workflow.spec.nodes.is_empty() {
                    return Err(KernelFault::new(
                        KernelFaultCode::InvalidConfig,
                        "a workflow root with no nodes has no first task to spawn; a root entry \
                         must be able to publish its first effect (§10.1)"
                            .to_string(),
                    ));
                }
                let core_spec = build_core_spec(&workflow.spec)?;
                let node_ids = wire_node_ids(&workflow.spec);
                let workflow_id = mint_workflow_id(&context.input.operation_id, context.step_seq);

                // ----- past this line the semantic engine advances -----
                let engine = self.engine_mut()?;
                seed_initial_context(engine, initial);
                // §6.1.7 — this DAG *is* the root, so its completion is the operation's terminal
                // rather than one more turn of a parent agent loop.
                engine.set_root_workflow(true);
                let action = engine.load_workflow_as(core_spec, ROOT_TASK_ID);
                self.node_ids = node_ids;
                self.workflow_nodes = workflow.spec.nodes.clone();
                self.workflow_id = Some(workflow_id.clone());
                let disposition = self.disposition_for(context, action, RootKind::Workflow)?;
                if !publishes(&disposition, EffectKindTag::SpawnTasks) {
                    return Err(KernelFault::new(
                        KernelFaultCode::InvalidLifecycle,
                        "a workflow root's first committed step must publish a task spawn, never a \
                         provider call (§10.1)"
                            .to_string(),
                    ));
                }
                Ok(PlannedStep {
                    root_kind: Some(RootKind::Workflow),
                    focus: Some(ExecutionFocus::workflow_controller(workflow_id, None)),
                    observations: Vec::new(),
                    disposition,
                })
            }
            RootEntry::DynamicWorkflow(_) => {
                // A dynamic root intentionally publishes no first spawn effect. The host script
                // supplies its first batch through `AppendWorkflowNodes`; the explicit open bit
                // prevents the empty DAG from self-terminating in the state machine.
                let workflow_id = mint_workflow_id(&context.input.operation_id, context.step_seq);
                let engine = self.engine_mut()?;
                seed_initial_context(engine, initial);
                engine.set_root_workflow(true);
                engine.set_dynamic_workflow_open(true);
                let action = engine.load_workflow_as(
                    crate::orchestration::workflow::WorkflowSpec::default(),
                    ROOT_TASK_ID,
                );
                debug_assert!(matches!(action, LoopAction::AwaitingResume));
                self.node_ids.clear();
                self.workflow_nodes.clear();
                self.workflow_id = Some(workflow_id.clone());
                Ok(PlannedStep {
                    root_kind: Some(RootKind::Workflow),
                    focus: Some(ExecutionFocus::workflow_controller(workflow_id, None)),
                    observations: Vec::new(),
                    disposition: self.quiet_step().disposition,
                })
            }
        }
    }
}
