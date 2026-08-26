use super::*;

impl CanonicalOperationDriver {
    /// §7.7 · host-observed facts. Two arms and no third: a signal the host observed, and a child
    /// attempt that finished.
    pub(super) fn plan_external_event(
        &mut self,
        context: &PlanContext<'_>,
        event: &ExternalEvent,
    ) -> Result<PlannedStep, KernelFault> {
        match event {
            ExternalEvent::DeliverSignal(delivery) => self.plan_signal(context, delivery),
            ExternalEvent::ChildCompleted(completed) => {
                self.plan_child_completed(context, completed)
            }
        }
    }

    /// §7.7 · one signal delivery.
    ///
    /// Everything decidable is decided before the router is touched, so a refused delivery is a
    /// genuine zero-mutation rejection:
    ///
    /// * a delivery needs a root to interrupt — a signal to an operation that has not started has
    ///   no attention to compete for;
    /// * `attempt` is 1-based: attempt 0 is a host that did not count its own redelivery, and the
    ///   delivery/attempt pair is the only thing that makes "one signal delivered three times"
    ///   distinguishable from "three signals" (§14.1);
    /// * a task target must name a task this kernel issued and that is still live. `SignalTarget`
    ///   is the kernel's own address space — a host session, process or thread is not a target and
    ///   has no representation on the wire (§5.2).
    ///
    /// Admission itself (TTL sweep, dedupe, deadline escalation, queue displacement) belongs to the
    /// router and runs on the **envelope's accepted time**, already fed in `plan_inner`. The
    /// signal's own `source_timestamp_ms` is audit metadata and never a clock (§11.2).
    ///
    /// The disposition is a *fact*: queueing, dropping, expiring and displacing produce
    /// observations and no effect. The one host action a signal can cause is preempting running
    /// children, and that is published as a `PreemptTasks` effect through the ordinary
    /// action-to-disposition path — committed only when its resolution comes back (§7.8).
    pub(super) fn plan_signal(
        &mut self,
        context: &PlanContext<'_>,
        delivery: &DeliverSignal,
    ) -> Result<PlannedStep, KernelFault> {
        let root_kind = self.require_root_kind()?;
        if delivery.attempt == 0 {
            return Err(KernelFault::new(
                KernelFaultCode::MalformedEnvelope,
                format!(
                    "delivery {} carries attempt 0; attempts are 1-based, and a delivery that \
                     cannot say which attempt it is cannot be told apart from a redelivery (§7.7)",
                    delivery.delivery_id
                ),
            ));
        }
        if let SignalTarget::Task(target) = &delivery.signal.target {
            let live = self
                .engine
                .as_ref()
                .and_then(|engine| engine.task_lifecycle(target.task_id.as_str()))
                .is_some_and(|lifecycle| !lifecycle.is_terminal());
            if !live {
                return Err(KernelFault::new(
                    KernelFaultCode::InvalidAuthority,
                    format!(
                        "signal {} targets task {}, which this operation has no live attempt for; \
                         a signal addresses the operation or one of its own logical tasks (§7.7)",
                        delivery.signal.signal_id, target.task_id
                    ),
                ));
            }
        }
        // DEC-8 · the one effect a delivery can publish is the preemption of running children, and
        // that is reachable only for a critical signal while this kernel holds live attempts.
        // Checked here, before the router moves, so a host that cannot stop its children refuses
        // the delivery instead of planning an effect it could never execute.
        //
        // The urgency read is the **effective** one: a signal carrying `escalate_after_ms: 0`
        // becomes critical at admission when the operation enabled deadline escalation, so reading
        // the bare wire value would let it reach the router and only then discover the host has no
        // preemption path. A deadline that comes due *later* escalates inside the queue, and that
        // path is covered by the effect-minting `require_effect_support` — a fault rather than a
        // zero-mutation refusal, which is the correct asymmetry: at admission nothing has moved.
        let escalation_enabled = context.config.signal_policy.deadline_escalation;
        if delivery.signal.effective_urgency(escalation_enabled) == SignalUrgency::Critical
            && !self.attempts.is_empty()
        {
            self.require_effect_support(context.config, EffectKindTag::PreemptTasks)?;
        }

        // ----- past this line the semantic engine advances -----
        // DEC-3 · at most one pending effect per kind, so a signal may only force a fresh provider
        // request when this operation is not already waiting on one.
        let may_issue_request = self.provider_calls.is_empty();
        let signal = runtime_signal(&delivery.signal, context.input.observed_at_ms);
        let engine = self.engine_mut()?;
        let action = engine.signal_event(
            context.input.operation_id.as_str().to_string(),
            delivery.delivery_id.as_str().to_string(),
            delivery.attempt,
            signal,
            may_issue_request,
        );
        let woken = engine
            .task_table_mut()
            .notify(&WaitKey::Signal(SignalFilter(
                delivery.signal.signal_id.as_str().into(),
            )));
        if !woken.is_empty() {
            engine.observe_local_runnable_tasks();
        }
        match action {
            Some(action) => self.continue_after(context, action, root_kind),
            // Queued / observed / ignored / dropped: the disposition observation is the whole of
            // what happened, and the host is asked for nothing.
            None => Ok(self.quiet_step()),
        }
    }

    /// §7.7 · a child completion is the event that drains a workflow DAG.
    pub(super) fn plan_child_completed(
        &mut self,
        context: &PlanContext<'_>,
        completed: &ChildCompleted,
    ) -> Result<PlannedStep, KernelFault> {
        let root_kind = self.require_root_kind()?;
        match self.attempts.get(completed.task_id.as_str()) {
            Some(minted) if minted == &completed.attempt_id => {}
            issued => {
                return Err(KernelFault::new(
                    KernelFaultCode::InvalidAuthority,
                    format!(
                        "task {} has attempt {} in this kernel, but the completion names {}; a \
                         host does not mint or rewrite child identity (§10.4)",
                        completed.task_id,
                        issued.map_or("none", AttemptId::as_str),
                        completed.attempt_id,
                    ),
                ));
            }
        }

        let mut effective_completion = completed.clone();
        if completed.result.status == ChildStatus::Failed {
            let attempt = attempt_ordinal(&completed.attempt_id).ok_or_else(|| {
                KernelFault::new(
                    KernelFaultCode::InvalidAuthority,
                    format!("attempt {} has no canonical ordinal", completed.attempt_id),
                )
            })?;
            let reason = completed
                .result
                .error
                .clone()
                .unwrap_or_else(|| "child attempt failed".to_string());
            let (strategy, max_restarts, relaunches) = {
                let engine = self.engine_mut()?;
                let task = engine
                    .task_table()
                    .get(completed.task_id.as_str())
                    .ok_or_else(|| {
                        KernelFault::new(
                            KernelFaultCode::InvalidAuthority,
                            format!("unknown completed task {}", completed.task_id),
                        )
                    })?;
                let parent = task.parent.as_ref().and_then(|id| {
                    engine.task_table().get(id.as_str()).map(|parent| {
                        (
                            parent.supervision.child_failure,
                            parent.supervision.max_restarts,
                        )
                    })
                });
                let (strategy, max_restarts) = parent.unwrap_or_default();
                let relaunches = task
                    .supervision_events
                    .iter()
                    .filter(|event| event.relaunched)
                    .count() as u32;
                (strategy, max_restarts, relaunches)
            };
            // Relaunch is opt-in *and bounded*: a restart/retry policy without an explicit maximum
            // records the failure but cannot generate an unbounded host-effect loop.
            let relaunch = matches!(
                strategy,
                crate::scheduler::tcb::ChildFailurePolicy::Restart
                    | crate::scheduler::tcb::ChildFailurePolicy::Retry
            ) && max_restarts.is_some_and(|max| relaunches < max);

            if relaunch {
                self.require_effect_support(context.config, EffectKindTag::SpawnTasks)?;
                let info = self
                    .engine
                    .as_ref()
                    .and_then(|engine| {
                        engine.workflow_spawn_info_for_agent(completed.task_id.as_str())
                    })
                    .ok_or_else(|| {
                        KernelFault::new(
                            KernelFaultCode::InvalidLifecycle,
                            format!(
                                "task {} has no active workflow launch descriptor to relaunch",
                                completed.task_id
                            ),
                        )
                    })?;
                let next_attempt = attempt.checked_add(1).ok_or_else(|| {
                    KernelFault::new(
                        KernelFaultCode::InvalidLifecycle,
                        "child attempt ordinal overflow".to_string(),
                    )
                })?;
                let launch = self.task_launch_attempt(
                    &context.input.operation_id,
                    context.step_seq,
                    &info,
                    next_attempt,
                )?;
                let event = crate::scheduler::tcb::SupervisionEvent {
                    attempt,
                    strategy,
                    reason: reason.clone().into(),
                    terminal: true,
                    relaunched: true,
                };
                let engine = self.engine_mut()?;
                engine
                    .task_table_mut()
                    .get_mut(completed.task_id.as_str())
                    .expect("validated task exists")
                    .supervision_events
                    .push(event);
                engine
                    .task_table_mut()
                    .prepare_supervised_relaunch(completed.task_id.as_str(), strategy);
                engine.mark_tasks_starting(&[completed.task_id.as_str().to_string()]);
                engine
                    .observations
                    .push(KernelObservation::ChildSupervised {
                        turn: engine.turn,
                        task_id: completed.task_id.as_str().to_string(),
                        attempt,
                        strategy: supervision_label(strategy).to_string(),
                        reason,
                        terminal: true,
                        relaunched: true,
                    });
                return Ok(PlannedStep {
                    root_kind: self.root_kind,
                    focus: self.focus.clone(),
                    observations: Vec::new(),
                    disposition: StepDisposition::Effects(EffectsDisposition {
                        effects: vec![KernelEffect {
                            effect_id: mint_effect_id(
                                &context.input.operation_id,
                                context.step_seq,
                                0,
                            ),
                            causation_input_id: context.input.input_id.clone(),
                            effect: EffectKind::SpawnTasks(SpawnTasksEffect {
                                tasks: vec![launch],
                                budget: None,
                            }),
                        }],
                    }),
                });
            }

            let event = crate::scheduler::tcb::SupervisionEvent {
                attempt,
                strategy,
                reason: reason.clone().into(),
                terminal: true,
                relaunched: false,
            };
            let engine = self.engine_mut()?;
            engine
                .task_table_mut()
                .get_mut(completed.task_id.as_str())
                .expect("validated task exists")
                .supervision_events
                .push(event);
            engine
                .observations
                .push(KernelObservation::ChildSupervised {
                    turn: engine.turn,
                    task_id: completed.task_id.as_str().to_string(),
                    attempt,
                    strategy: supervision_label(strategy).to_string(),
                    reason,
                    terminal: true,
                    relaunched: false,
                });
            if strategy == crate::scheduler::tcb::ChildFailurePolicy::Ignore {
                effective_completion.result.status = ChildStatus::Completed;
                effective_completion.result.error = None;
            }
        }

        // ----- past this line the semantic engine advances -----
        self.engine_mut()?
            .task_table_mut()
            .notify(&WaitKey::Child(completed.task_id.as_str().into()));
        // §10.4 · the attempt is spent. A second completion naming it — and any `parent_requests`
        // riding on that second completion — is a stale causation, refused by the check above.
        self.attempts.remove(completed.task_id.as_str());

        // §7.7 · the requests enter P1 first, with `ChildAttempt` causation, and the completion is
        // fed afterwards so its own drive produces the single next ready batch.
        //
        // GAP-4: this loop can neither fail the transition nor skip the completion. Each request is
        // adjudicated on its own; a refused one leaves a structured rejection observation and
        // changes neither its siblings nor the fact that the child ran.
        //
        // No arm can move the focus here, which is why only `effects` is collected: the sole
        // focus-moving syscall is `SubmitWorkflow`'s *bootstrap*, and a child attempt only exists
        // while its DAG is in flight — so that request always takes the flatten arm instead.
        let mut index = 0u32;
        let mut effects = Vec::new();
        for (seq, request) in completed.parent_requests.iter().enumerate() {
            let causation = SyscallCausation::ChildAttempt(ChildAttemptCausation {
                task_id: completed.task_id.clone(),
                attempt_id: completed.attempt_id.clone(),
                request_seq: seq as u32,
            });
            match self.apply_syscall(context, &causation, request, &mut index) {
                Ok(outcome) => effects.extend(outcome.effects),
                Err(SyscallRefusal::Fault(fault)) => self.note_rejection(
                    SyscallRejection::new(
                        "parent_request",
                        format!("request {seq} refused: {}", fault.message),
                    )
                    .by(&completed.task_id),
                ),
                Err(SyscallRefusal::Rejected(rejection)) => {
                    self.note_rejection(rejection.by(&completed.task_id))
                }
            }
        }

        // `feed` clears the observation buffer at the head of every event, and these facts happened
        // *before* it. Carrying them across is what keeps the audit trail of a refused parent
        // request from being erased by the very completion it rode in on.
        let syscall_observations = self
            .engine_mut()?
            .take_observations()
            .into_iter()
            .collect::<Vec<_>>();

        let result = sub_agent_result(&effective_completion);
        let engine = self.engine_mut()?;
        let action = engine.feed(LoopEvent::SubAgentCompleted { result });
        let mut step = self.continue_after_at(context, action, root_kind, &mut index)?;
        if let Some(engine) = self.engine.as_mut() {
            engine.observations.splice(0..0, syscall_observations);
        }
        if !effects.is_empty() {
            match &mut step.disposition {
                StepDisposition::Effects(published) => {
                    let mut merged = effects;
                    merged.append(&mut published.effects);
                    published.effects = merged;
                }
                StepDisposition::Terminal(_) => {
                    return Err(KernelFault::new(
                        KernelFaultCode::InvalidLifecycle,
                        "a completion that terminates the operation cannot also publish the \
                         effects its parent requests asked for (§7.12)"
                            .to_string(),
                    ));
                }
            }
        }
        Ok(step)
    }

    // ----- §7.5 · the live control plane -----

    /// §7.5 · reduce one host command onto the mechanism that already owns that state.
    ///
    /// Every arm here carries **host authority**, which is what separates this from the P1 syscall
    /// path in [`Self::apply_syscall`]: a host command is not gated, not quarantine-coerced and not
    /// attributed to a caller, because the host *is* the authority. `UpdateTask` is the pair that
    /// makes the split visible — the same `TaskUpdate` payload, two input classes, two authorities,
    /// preserving the authority distinction that the retired shared task-update input could not
    /// express (§7.5 现状注记).
    ///
    /// Refusals are faults rather than model-facing rejections: a control command is a host bug, so
    /// it is answered to the host and never becomes a note the model reads.
    pub(super) fn plan_host_control(
        &mut self,
        context: &PlanContext<'_>,
        command: &HostCommand,
    ) -> Result<PlannedStep, KernelFault> {
        match command {
            HostCommand::Cancel(cancel) => self.plan_cancel(context, cancel),
            HostCommand::ForceCompact(_) => {
                let root_kind = self.require_root_kind()?;
                self.engine_mut()?.force_compact();
                // A compaction that archived history owes a `page_out` effect; `continue_after`
                // externalises it through the same one path an in-turn compaction takes.
                self.continue_after(context, LoopAction::AwaitingResume, root_kind)
            }
            HostCommand::UpdateTask(update) => self.plan_host_task_update(update),
            HostCommand::ApplyCapabilityPatch(patch) => self.plan_capability_patch(patch),
            HostCommand::ApplyKnowledgeMutation(mutation) => self.plan_knowledge_mutation(mutation),
            HostCommand::SeedKnowledge(seed) => self.plan_seed_knowledge(seed),
            HostCommand::ApplySkillActivation(activation) => self.plan_skill_activation(activation),
            HostCommand::ApplyPolicyPatch(patch) => self.plan_policy_patch(patch),
            HostCommand::UpdateDeadline(deadline) => self.plan_update_deadline(deadline),
        }
    }

    /// §11.1 · the cancellation ladder, and the only path an operation is cancelled through.
    ///
    /// The order is downstream-first and it happens **inside one transition**, because §7.12 admits
    /// effects or a terminal and never both:
    ///
    /// 1. every child attempt this kernel issued is settled and its wait torn down, every pending
    ///    workflow batch and deferred host effect dropped (`cancel_operation`);
    /// 2. the driver's own ledger of live attempts and pending calls is spent with them, so a late
    ///    completion or resolution naming one is a stale causation rather than a resurrection;
    /// 3. only then is the root terminal minted.
    ///
    /// The *real* I/O stop is the host's: §11.1 has the host stop provider, tool and child I/O
    /// before it submits this command, and the kernel adjudicates the terminal. That is why this
    /// step publishes no `PreemptTasks` effect — asking the host to stop what it already stopped
    /// would need a second transition, and the operation would not be cancelled until it came back.
    ///
    /// The reason is the host's, not the loop's: `cancel_operation` terminates the semantic loop
    /// with its own internal `UserAbort`, but a `Deadline` or `HostShutdown` cancellation must not
    /// be reported as a user abort, so the terminal is built from the command.
    pub(super) fn plan_cancel(
        &mut self,
        context: &PlanContext<'_>,
        cancel: &CancelCommand,
    ) -> Result<PlannedStep, KernelFault> {
        let root_kind = self.root_kind;
        let focus = self.focus.clone();
        let reason = cancel.reason;
        // §7.5 · the cancel command carries no operation id of its own; the envelope owns it.
        let operation_id = context.input.operation_id.as_str().to_string();
        let engine = self.engine_mut()?;
        let action = engine.cancel_operation(
            operation_id,
            reason,
            cancel
                .pending_call_ids
                .iter()
                .map(|call_id| call_id.as_str().to_string())
                .collect(),
        );
        // Downstream identity is spent in the same transition that settles the tasks it names.
        self.attempts.clear();
        self.provider_calls.clear();
        self.pending_memory_writes.clear();
        self.pending_memory_queries.clear();

        let LoopAction::Done { result } = action else {
            return Err(KernelFault::new(
                KernelFaultCode::InvalidLifecycle,
                format!(
                    "cancelling the operation produced {} instead of a terminal; cancellation is \
                     the one control command that always ends the operation (§11.1)",
                    loop_action_label(&action)
                ),
            ));
        };
        Ok(PlannedStep {
            root_kind,
            focus,
            observations: Vec::new(),
            disposition: StepDisposition::Terminal(TerminalDisposition {
                terminal: KernelTerminal::Cancelled(CancelledTerminal {
                    reason,
                    usage: UsageReport {
                        input_tokens: WireU64::new(result.total_tokens_used),
                        output_tokens: WireU64::ZERO,
                        turns: result.turns_used,
                        cached_input_tokens: None,
                    },
                }),
            }),
        })
    }

    /// §7.5 · the host's own plan edit. Same payload as the model's `update_task` syscall, applied
    /// through the same context mechanism — and deliberately *not* through the P1 gate, because the
    /// authority is the host's rather than a derived caller's.
    pub(super) fn plan_host_task_update(
        &mut self,
        update: &UpdateTaskCommand,
    ) -> Result<PlannedStep, KernelFault> {
        let engine = self.engine_mut()?;
        engine.ctx.update_task(core_task_update(&update.update));
        Ok(self.quiet_step())
    }

    /// §13.2 · mount/unmount in one command so a swap is atomic. Unmounting something absent errs
    /// open (it is already not mounted), which is the only shape that makes a retry safe.
    pub(super) fn plan_capability_patch(
        &mut self,
        patch: &ApplyCapabilityPatchCommand,
    ) -> Result<PlannedStep, KernelFault> {
        let engine = self.engine_mut()?;
        for grant in &patch.patch.mount {
            engine.mount_capability(
                crate::types::capability::CapabilityDescriptor {
                    id: grant.id.as_str().into(),
                    kind: core_capability_kind(grant.kind),
                    description: grant.description.clone().unwrap_or_default(),
                    tool_schema: None,
                    skill: None,
                    metadata: serde_json::Value::Null,
                    lease: None,
                    is_pinned: false,
                    version: None,
                    mounted_by: None,
                    mount_reason: None,
                },
                None,
                None,
            );
        }
        for reference in &patch.patch.unmount {
            engine.unmount_capability(core_capability_kind(reference.kind), &reference.id);
        }
        Ok(self.quiet_step())
    }

    /// §13.2 · keyed knowledge upsert + removal. Both directions are boundary-deferred by the
    /// partition itself, so the model never sees system bytes change mid-turn.
    pub(super) fn plan_knowledge_mutation(
        &mut self,
        mutation: &ApplyKnowledgeMutationCommand,
    ) -> Result<PlannedStep, KernelFault> {
        let engine = self.engine_mut()?;
        seed_knowledge(engine, &mutation.mutation.upsert);
        for key in &mutation.mutation.remove {
            engine.ctx.remove_knowledge(key);
        }
        Ok(self.quiet_step())
    }

    /// DEC-9 · the host seeding the knowledge partition. Same mechanism as the initial context's
    /// `knowledge`, and named apart from the P1 `PageIn { handle_id }` on purpose: the two are
    /// opposite directions and must never share a name again (§7.5).
    pub(super) fn plan_seed_knowledge(
        &mut self,
        seed: &SeedKnowledgeCommand,
    ) -> Result<PlannedStep, KernelFault> {
        let engine = self.engine_mut()?;
        seed_knowledge(engine, &seed.entries);
        Ok(self.quiet_step())
    }

    /// §13.2 · skill activation state, validated whole before anything moves.
    ///
    /// A name outside the operation's declared catalog is refused rather than invented — the same
    /// rule the model's `ActivateSkill` syscall obeys — and because the command is atomic, one bad
    /// name refuses the whole swap instead of leaving half of it applied.
    pub(super) fn plan_skill_activation(
        &mut self,
        activation: &ApplySkillActivationCommand,
    ) -> Result<PlannedStep, KernelFault> {
        let engine = self.engine_mut()?;
        for activate in &activation.activate {
            if !engine.ctx.skill_available(&activate.name) {
                return Err(KernelFault::new(
                    KernelFaultCode::InvalidConfig,
                    format!(
                        "this operation declares no skill named {:?}; activating one is a \
                         capability mutation and is refused rather than invented (§13.2)",
                        activate.name
                    ),
                ));
            }
            ensure_skill_grants_are_attenuated(
                engine.ctx.skill_capability_grants(&activate.name),
                engine.root_capabilities(),
            )
            .map_err(|violations| {
                KernelFault::new(
                    KernelFaultCode::InvalidAuthority,
                    skill_grant_attenuation_message(&activate.name, &violations),
                )
            })?;
        }
        let turn = engine.turn;
        for activate in &activation.activate {
            let expires_at_turn = activate.lease_turns.map(|turns| turn.saturating_add(turns));
            engine
                .ctx
                .activate_skill_leased(activate.name.as_str(), expires_at_turn);
        }
        for name in &activation.deactivate {
            engine.ctx.deactivate_skill(name);
        }
        Ok(self.quiet_step())
    }

    /// §13.2 / DEC-6 · one revision-guarded policy patch.
    ///
    /// [`LivePolicyState::apply`] is all-or-nothing: a stale revision, a widened quota or a policy
    /// that fails its boot validator leaves both the configuration and the revision exactly as they
    /// were, so a refused patch is a zero-mutation rejection and the writer rebases instead of
    /// silently overwriting whoever won the race. Only after it succeeds are the changed policies
    /// re-installed into the running engine, through the same installers the genesis build uses.
    pub(super) fn plan_policy_patch(
        &mut self,
        patch: &ApplyPolicyPatchCommand,
    ) -> Result<PlannedStep, KernelFault> {
        let Some(policy) = self.policy.as_mut() else {
            return Err(KernelFault::new(
                KernelFaultCode::InvalidLifecycle,
                "the operation has no genesis configuration, so it has no policy to patch"
                    .to_string(),
            ));
        };
        let revision = policy.apply(patch).map_err(|rejection| {
            KernelFault::new(KernelFaultCode::InvalidConfig, rejection.message)
        })?;
        let config = policy.config().clone();
        let engine = self.engine_mut()?;
        install_live_policies(engine, &config);
        let turn = engine.turn;
        engine
            .observations
            .push(KernelObservation::LivePolicyChanged {
                turn,
                policy: live_policy_label(&patch.patch).to_string(),
                revision: revision.get(),
            });
        Ok(self.quiet_step())
    }

    /// §13.2 · the operation's absolute deadline, projected onto the wall-time budget axis the
    /// scheduler already owns.
    ///
    /// The axis is a duration measured from the operation's first accepted time, so an absolute
    /// deadline becomes `deadline − start`. A deadline already in the past is not an error: it
    /// yields a zero-length budget, and the next scheduling decision terminates on `Deadline` —
    /// the same verdict the axis would have reached on its own a moment later.
    pub(super) fn plan_update_deadline(
        &mut self,
        deadline: &UpdateDeadlineCommand,
    ) -> Result<PlannedStep, KernelFault> {
        let engine = self.engine_mut()?;
        let started_at_ms = engine.started_at_ms();
        let budget = match (deadline.deadline_ms, started_at_ms) {
            (None, _) => None,
            (Some(deadline_ms), Some(started_at_ms)) => {
                Some(deadline_ms.get().saturating_sub(started_at_ms))
            }
            (Some(_), None) => {
                return Err(KernelFault::new(
                    KernelFaultCode::InvalidLifecycle,
                    "this operation has accepted no timed input yet, so an absolute deadline has \
                     no start to measure from (§11.2)"
                        .to_string(),
                ));
            }
        };
        engine.set_wall_budget(budget);
        Ok(self.quiet_step())
    }
}
