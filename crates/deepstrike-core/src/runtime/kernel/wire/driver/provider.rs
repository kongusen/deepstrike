use super::*;

impl CanonicalOperationDriver {
    // ----- §7.6 · P1 syscalls and caller causation -----

    /// Derive the caller of every syscall a provider result carries.
    ///
    /// This is the whole of "the host does not declare a caller". The kernel already knows which
    /// provider effect it is resolving and which surface that call advertised; a `ProviderTool`
    /// causation is that knowledge written down. Three refusals, all before anything moves:
    ///
    /// * the effect is not one this driver published as a provider call — there is no turn to
    ///   attribute the request to;
    /// * the tool name was never exposed on that turn — a result may not invent a surface, and a
    ///   forged `start_workflow` in a run that never offered one dies here;
    /// * the call id already produced a syscall — a causation is spent once, so re-delivering the
    ///   same result under a fresh input id buys nothing.
    pub(super) fn derive_provider_syscalls(
        &self,
        effect_id: &EffectId,
        calls: &[WireToolCall],
    ) -> Result<Vec<(SyscallCausation, WireToolCall)>, KernelFault> {
        let Some(pending) = self.provider_calls.get(effect_id) else {
            return Err(KernelFault::new(
                KernelFaultCode::InvalidAuthority,
                format!(
                    "effect {effect_id} is not a provider call this kernel published, so a tool \
                     call inside its result has no caller to derive (§7.6)"
                ),
            ));
        };
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut derived = Vec::with_capacity(calls.len());
        for call in calls {
            if !pending.exposed_tools.contains(&call.name) {
                return Err(KernelFault::new(
                    KernelFaultCode::InvalidAuthority,
                    format!(
                        "the turn behind effect {effect_id} exposed no tool named {:?}; a caller is \
                         derived from the surface the kernel published, never from the name a \
                         result carries (§7.6)",
                        call.name
                    ),
                ));
            }
            if self.consumed_calls.contains(call.call_id.as_str())
                || !seen.insert(call.call_id.as_str())
            {
                return Err(KernelFault::new(
                    KernelFaultCode::InvalidAuthority,
                    format!(
                        "call {} already produced a syscall; a causation is consumed once (§7.6)",
                        call.call_id
                    ),
                ));
            }
            derived.push((
                SyscallCausation::ProviderTool(ProviderToolCausation {
                    provider_effect_id: effect_id.clone(),
                    call_id: call.call_id.clone(),
                    task_id: pending.task_id.clone(),
                }),
                call.clone(),
            ));
        }
        Ok(derived)
    }

    /// §7.9 + §7.6 · one completed provider turn, whole.
    ///
    /// The batch a model returns can mix two populations the kernel must not confuse: **P1
    /// syscalls**, which the kernel adjudicates itself, and **host tool calls**, which it
    /// dispatches. Both halves happen here, in this order and for this reason:
    ///
    /// 1. the syscalls are adjudicated *before* the turn is fed, because they change what the next
    ///    rendered context contains — an activated skill, an edited plan, a grown DAG;
    /// 2. the assistant message is fed **whole**, tool calls included, so the model reads back the
    ///    turn it actually emitted; each syscall is closed with the kernel's own answer so every
    ///    call still has a result (the trained convention);
    /// 3. the continuation is the engine's, not this function's — [`LoopStateMachine::feed`] stays
    ///    the single place that decides what a provider turn means. All this reduction contributes
    ///    is *what the kernel already has outstanding*, which is the §5k question: a pure
    ///    control-plane batch publishes no effect, so without an answer the operation would stall.
    pub(super) fn plan_provider_completed(
        &mut self,
        context: &PlanContext<'_>,
        effect_id: &EffectId,
        completed: &ProviderCompleted,
    ) -> Result<PlannedStep, KernelFault> {
        let root_kind = self.require_root_kind()?;
        self.require_pending_provider_call(effect_id)?;
        let syscalls: Vec<WireToolCall> = completed
            .message
            .tool_calls
            .iter()
            .filter(|call| is_syscall_tool(&call.name))
            .cloned()
            .collect();
        let derived = self.derive_provider_syscalls(effect_id, &syscalls)?;
        let message = core_provider_message(&completed.message)?;

        // ----- past this line the semantic engine advances -----
        let engine = self.engine_mut()?;
        if let Some(tokens) = completed.observed_input_tokens {
            engine.ctx.set_observed_prompt_tokens(tokens);
        }
        // §22.8 · the typed stop reason answers the one question the loop asks, so no vendor text
        // is classified anywhere on this path.
        engine.set_output_truncated(matches!(
            completed.stop_reason,
            Some(super::super::effect::ProviderStopReason::MaxTokens)
        ));

        let mut index = 0u32;
        let mut effects = Vec::new();
        let mut syscall_focus: Option<ExecutionFocus> = None;
        let mut answered: Vec<AnsweredCall> = Vec::with_capacity(derived.len());
        let mut round_caller: Option<TaskId> = None;
        for (causation, call) in &derived {
            let outcome = match decode_syscall(call) {
                Ok(request) => self.apply_syscall(context, causation, &request, &mut index),
                Err(rejection) => Err(SyscallRefusal::Rejected(rejection)),
            };
            match outcome {
                Ok(outcome) => {
                    answered.push(AnsweredCall {
                        call_id: call.call_id.as_str().into(),
                        output: outcome
                            .ack
                            .clone()
                            .unwrap_or_else(|| syscall_ack(&call.name).to_string()),
                        is_error: false,
                    });
                    effects.extend(outcome.effects);
                    if let Some(next) = outcome.focus {
                        syscall_focus = Some(next);
                    }
                    if outcome.needs_workflow_round {
                        round_caller = Some(causation_task(causation));
                    }
                }
                Err(SyscallRefusal::Fault(fault)) => return Err(fault),
                Err(SyscallRefusal::Rejected(rejection)) => {
                    answered.push(AnsweredCall {
                        call_id: call.call_id.as_str().into(),
                        output: rejection.reason.clone(),
                        is_error: true,
                    });
                    self.note_rejection(rejection.by(&causation_task(causation)))
                }
            }
        }
        // §10.3 · a batch that grew the DAG owes it a spawn round, and the author waits for the
        // work it just authored rather than taking another turn.
        let mut awaits_kernel_work = !effects.is_empty();
        if let Some(caller) = round_caller {
            awaits_kernel_work = true;
            let action = self.engine_mut()?.drive_workflow_round(caller.as_str());
            self.extend_with_action(context, action, root_kind, &mut index, &mut effects)?;
        }

        let engine = self.engine_mut()?;
        engine.stage_adjudicated_turn(AdjudicatedTurn {
            answered_calls: answered,
            idle_continuation: if awaits_kernel_work {
                IdleContinuation::Await
            } else {
                IdleContinuation::CallProvider
            },
        });
        // The syscalls' own observations happened before `feed`, which clears the buffer at the
        // head of every event — carry them across, exactly as the child-completion path does.
        let syscall_observations = engine.take_observations();
        let action = engine.feed(LoopEvent::LLMResponse { message });
        let mut step = self.continue_after_at(context, action, root_kind, &mut index)?;
        if let Some(engine) = self.engine.as_mut() {
            engine.observations.splice(0..0, syscall_observations);
        }
        if syscall_focus.is_some() {
            step.focus = syscall_focus;
        }
        if !effects.is_empty() {
            match &mut step.disposition {
                StepDisposition::Effects(published) => {
                    // A tool batch the turn dispatched is NOT published alongside the syscalls'
                    // own effects. Publishing both would hand the host a two-effect step, and the
                    // batch cannot be re-derived later either: §15.3 admits at most one pending
                    // effect per kind, so the moment the syscall effect resolves, a resume that
                    // rebuilds the batch from history would collide with the copy still pending.
                    // The engine already dispatched the calls (its phase holds them unanswered),
                    // so the honest shape is: this step publishes only the syscalls' effects, the
                    // kernel awaits their resolution, and `resume_after_preload` re-derives the
                    // batch — with the kind slot free again — once the last of them settles.
                    published
                        .effects
                        .retain(|effect| !matches!(effect.effect, EffectKind::ExecuteTools(_)));
                    let mut merged = effects;
                    merged.append(&mut published.effects);
                    published.effects = merged;
                }
                StepDisposition::Terminal(_) => {
                    return Err(KernelFault::new(
                        KernelFaultCode::InvalidLifecycle,
                        "a provider turn that terminates the operation cannot also publish the \
                         effects its syscalls asked for (§7.12)"
                            .to_string(),
                    ));
                }
            }
        }

        // The causation ledger is settled last, so a transition that ends in a fault does not leave
        // this driver claiming a spent call id and a resolved provider surface that the journal has
        // no record of. (The engine advance above is guarded the same way every other plan is — by
        // the staging slot, which fails closed on a plan that never commits.)
        self.provider_calls.remove(effect_id);
        for (_, call) in &derived {
            self.consumed_calls
                .insert(call.call_id.as_str().to_string());
        }
        Ok(step)
    }
}
