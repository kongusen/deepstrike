use super::*;

impl CanonicalOperationDriver {
    /// §7.9 · effect resolution — the single entry every pending effect is answered through.
    ///
    /// Two arms and no third, mirroring [`EffectOutcome`]. The transaction has already decided
    /// *admissibility* (still pending, kind matches, not a conflicting duplicate — §15.3), so what
    /// is left here is purely semantic: which internal mechanism the outcome feeds.
    pub(super) fn plan_resolve_effect(
        &mut self,
        context: &PlanContext<'_>,
        resolve: &ResolveEffect,
    ) -> Result<PlannedStep, KernelFault> {
        let planned = match &resolve.outcome {
            EffectOutcome::Succeeded(success) => {
                self.plan_effect_success(context, &resolve.effect_id, &success.result)
            }
            EffectOutcome::Failed(failed) => {
                self.plan_effect_failure(context, &resolve.effect_id, &failed.failure)
            }
        };
        if planned.is_ok() {
            self.engine_mut()?
                .task_table_mut()
                .notify(&WaitKey::Effect(resolve.effect_id.clone()));
        }
        planned
    }

    /// The success half: each variant reduces onto the mechanism that already owns that fact.
    pub(super) fn plan_effect_success(
        &mut self,
        context: &PlanContext<'_>,
        effect_id: &EffectId,
        success: &EffectSuccess,
    ) -> Result<PlannedStep, KernelFault> {
        match success {
            EffectSuccess::Provider(provider) => match &provider.outcome {
                ProviderOutcome::Completed(completed) => {
                    self.plan_provider_completed(context, effect_id, completed)
                }
                // §7.9 · a *semantic* outcome, not a transport failure: the kernel compacts and
                // re-emits a provider call, and no vendor text is read to decide that (§22.8).
                ProviderOutcome::ContextOverflow(overflow) => {
                    let root_kind = self.require_root_kind()?;
                    self.require_pending_provider_call(effect_id)?;
                    let engine = self.engine_mut()?;
                    if let Some(tokens) = overflow.observed_input_tokens {
                        engine.ctx.set_observed_prompt_tokens(tokens);
                    }
                    let action = engine.recover_from_context_overflow();
                    self.provider_calls.remove(effect_id);
                    self.continue_after(context, action, root_kind)
                }
            },
            EffectSuccess::Tools(tools) => {
                let root_kind = self.require_root_kind()?;
                // Zero-mutation discipline: the whole batch is adjudicated against the payload
                // policy before any of it reaches the engine, so a batch with one illegal result
                // does not half-land.
                for payload in &tools.results {
                    check_payload_policy(payload, &context.config.payload_policy)?;
                }
                let mut results: Vec<ToolResult> =
                    tools.results.iter().map(core_tool_result).collect();
                results.extend(self.close_out_fatal_batch(&tools.results)?);
                let measurements = tools.results.iter().filter_map(|payload| match payload {
                    WireToolResultPayload::Inline(inline) => inline.result.tokens.map(|tokens| {
                        crate::context::measurement::ToolMeasurement::new(
                            inline.call_id.as_str(),
                            tokens,
                        )
                    }),
                    WireToolResultPayload::External(_) => None,
                });
                let mut action = self
                    .engine_mut()?
                    .feed_tool_results_with_measurements(results, measurements);
                self.record_external_payloads(&tools.results)?;
                self.engine_mut()?.refresh_call_llm_action(&mut action);
                self.continue_after(context, action, root_kind)
            }
            EffectSuccess::Approval(approval) => {
                let root_kind = self.require_root_kind()?;
                let approved = approval
                    .approved_call_ids
                    .iter()
                    .map(|id| id.as_str().to_string())
                    .collect();
                let denied = approval
                    .denied_call_ids
                    .iter()
                    .map(|id| id.as_str().to_string())
                    .collect();
                let action = self.engine_mut()?.resolve_approval(approved, denied);
                self.engine_mut()?
                    .task_table_mut()
                    .notify(&WaitKey::Approval(ApprovalId("pending".into())));
                self.continue_after(context, action, root_kind)
            }
            EffectSuccess::TasksSpawned(spawned) => {
                let mut started: Vec<String> = Vec::new();
                let mut failures: Vec<WorkflowSpawnFailure> = Vec::new();
                for attempt in &spawned.attempts {
                    let agent_id = attempt.task_id.as_str().to_string();
                    match &attempt.outcome {
                        super::super::effect::TaskLaunchStatus::Started(_) => {
                            started.push(agent_id)
                        }
                        super::super::effect::TaskLaunchStatus::Failed(failed) => {
                            // §10.4 · a failed launch terminates the attempt. Dropping it here is
                            // what makes a later completion naming it a stale causation rather than
                            // a resurrection of a task that never started.
                            self.attempts.remove(&agent_id);
                            failures.push(WorkflowSpawnFailure {
                                agent_id,
                                error: failed.failure.message.clone(),
                            });
                        }
                    }
                }
                let root_kind = self.require_root_kind()?;
                let action = self.engine_mut()?.resolve_workflow_spawn(started, failures);
                self.continue_after(context, action, root_kind)
            }
            EffectSuccess::TasksPreempted(preempted) => {
                let root_kind = self.require_root_kind()?;
                // §10.4 · every attempt named here is spent, whichever way it went: a preempted
                // child is gone, and one that had already finished is finished. Either way a later
                // completion naming it is a stale causation.
                for attempt in &preempted.attempts {
                    self.attempts.remove(attempt.task_id.as_str());
                }
                let action = self.engine_mut()?.resolve_preempt();
                self.continue_after(context, action, root_kind)
            }
            EffectSuccess::MemoryPersisted(persisted) => {
                self.commit_memory_write(effect_id, Some(&persisted.receipt), None)
            }
            EffectSuccess::MemoryQueried(queried) => {
                let root_kind = self.require_root_kind()?;
                let Some(query) = self.pending_memory_queries.remove(effect_id) else {
                    return Err(unowned_resolution(effect_id, "memory query"));
                };
                let turn = self.engine_mut()?.turn;
                let mut recalled = Vec::with_capacity(queried.recalls.len());
                for recall in &queried.recalls {
                    // §22.13 · the host answers with records, never with authority: the recall is
                    // rendered into history as content the model reads, and nothing about it
                    // rewrites this operation's binding, trust or provenance.
                    let content = format!(
                        "[MEMORY record_ref={} kind={}] {}",
                        recall.record_ref,
                        wire_memory_kind_label(recall.kind),
                        recall.content
                    );
                    let engine = self.engine_mut()?;
                    let tokens = engine.ctx.engine.count(&content).max(1);
                    engine.ctx.push_history(CoreMessage::user(content), tokens);
                    recalled.push(recall.record_ref.as_str().to_string());
                }
                // The recalls are in history now, so the turn that asked for them resumes with a
                // rendered context that contains them. The observation is pushed *after* the
                // resume, which clears the buffer at its head: a fact recorded before it would be
                // erased by the very continuation it describes.
                let engine = self.engine_mut()?;
                let action = engine.resume_after_preload();
                // §5k · other effects this kernel published are still outstanding — resuming the
                // turn now would emit a provider call that outruns work the host still owes (the
                // sibling effect of a mixed syscall batch). The last of them to settle re-runs
                // this resume with a free hand.
                let action = if context.pending.is_empty() {
                    action
                } else {
                    LoopAction::AwaitingResume
                };
                engine.observations.push(KernelObservation::MemoryQueried {
                    turn,
                    scope: binding_scope(&query.binding_id),
                    query: query.text.clone(),
                    requested_k: query.requested_k as usize,
                    requires_async_response: false,
                });
                let mut step = self.continue_after(context, action, root_kind)?;
                step.focus = self.focus.clone();
                Ok(step)
            }
            EffectSuccess::PageOutArchived(archived) => {
                let root_kind = self.require_root_kind()?;
                self.verify_page_out_receipt(context, effect_id, &archived.receipt)?;
                let receipt = &archived.receipt;
                let action = self
                    .engine_mut()?
                    .commit_page_out_archive(Some(receipt.payload_ref.as_str().to_string()));
                // B19 · the archived body is `PagedOut`, not `External`: it *was* resident and left
                // under pressure. Both states page in through the same `LoadPayload` effect, and
                // keeping them apart is what lets a restore say which of the two happened.
                let engine = self.engine_mut()?;
                let turn = engine.turn;
                let previous = engine.ctx.set_payload_residency(
                    receipt.handle_id.as_str(),
                    HandleKind::MemoryPage,
                    0,
                    Residency::PagedOut {
                        payload_ref: receipt.payload_ref.as_str().to_string(),
                        digest: receipt.digest.as_str().to_string(),
                    },
                );
                engine
                    .observations
                    .push(KernelObservation::PayloadResidencyChanged {
                        turn,
                        handle_id: receipt.handle_id.as_str().to_string(),
                        from: previous.map(|residency| residency.label().to_string()),
                        to: "paged_out".to_string(),
                        payload_ref: Some(receipt.payload_ref.as_str().to_string()),
                        original_size: receipt.original_size.get(),
                    });
                self.continue_after(context, action, root_kind)
            }
            EffectSuccess::MilestoneEvaluated(evaluated) => {
                let root_kind = self.require_root_kind()?;
                let action = self.engine_mut()?.feed(LoopEvent::MilestoneResult {
                    result: core_milestone_result(&evaluated.result),
                });
                self.continue_after(context, action, root_kind)
            }
            EffectSuccess::PayloadLoaded(loaded) => {
                self.commit_payload_load(context, effect_id, loaded)
            }
            EffectSuccess::PromptMeasured(_) => Err(KernelFault::new(
                KernelFaultCode::UnexpectedEffectOutcome,
                "prompt measurement outcomes are reserved but adaptive measurement has no scheduler producer",
            )),
        }
    }

    /// §7.10 · what a `fatal` disposition means on the batch that carried it.
    ///
    /// `fatal` is the host saying "the executor stopped here", so the calls this batch dispatched
    /// but never answered are not pending — they will never be answered at all. Left alone they
    /// would be orphan `tool_call`s: a provider replay with an assistant turn whose calls have no
    /// matching results, which is malformed on every vendor wire and is the exact failure mode the
    /// pairing repair in the SDKs exists to paper over.
    ///
    /// So the kernel closes them out, in the same feed, with the same shape the `ExecuteTools`
    /// *failure* arm uses for a batch that never ran: a committed, model-visible error result that
    /// says the call did not take effect. Same reasoning as v0.2.42's model-facing surface batch —
    /// the model adapts to a failure it can see and cannot adapt to an attempt that was erased.
    ///
    /// Returns an empty vector for an ordinary batch: no fatal result, nothing to close out.
    ///
    /// The fatality scan is **total over residency** (§7.10 rule 9): a call that failed fatally
    /// and spilled a large body is still a call that stopped the executor, so it stops the batch
    /// exactly as an inline one does. Reading only the inline arm would make the close-out depend
    /// on how big the failure's output happened to be.
    pub(super) fn close_out_fatal_batch(
        &mut self,
        submitted: &[WireToolResultPayload],
    ) -> Result<Vec<ToolResult>, KernelFault> {
        let fatal = submitted
            .iter()
            .any(|payload| payload.disposition().is_fatal());
        if !fatal {
            return Ok(Vec::new());
        }
        let answered: BTreeSet<&str> = submitted
            .iter()
            .map(|payload| payload.call_id().as_str())
            .collect();
        Ok(self
            .engine_mut()?
            .dispatched_tool_calls()
            .iter()
            .filter(|call| !answered.contains(call.id.as_str()))
            .map(|call| ToolResult {
                call_id: call.id.clone(),
                output: Content::Text(
                    "not executed: an earlier call in this batch failed fatally and the executor \
                     stopped. This call did not take effect — re-issue it only if the failure it \
                     followed does not make it pointless."
                        .to_string(),
                ),
                durable_content: None,
                is_error: true,
                is_fatal: false,
                error_kind: Some(ToolErrorKind::Fatal),
            })
            .collect())
    }

    /// The failure half (§7.9 · DEC-5).
    ///
    /// One policy decision per effect kind, taken **once**. The kernel never re-emits the same
    /// intent: a host that wants another attempt asks again with a new causation and keeps its own
    /// idempotency on the effect id / launch token. The kind comes from the effect the kernel
    /// itself published — `HostEffectFailure` is deliberately kind-agnostic (§7.9), so it is not,
    /// and must not be, the thing that selects the decision.
    pub(super) fn plan_effect_failure(
        &mut self,
        context: &PlanContext<'_>,
        effect_id: &EffectId,
        failure: &HostEffectFailure,
    ) -> Result<PlannedStep, KernelFault> {
        let Some(pending) = context.resolving else {
            return Err(unowned_resolution(effect_id, "effect"));
        };
        let tag = pending.tag();
        match tag {
            // The loop cannot advance without a provider turn, and asking again would be the
            // redispatch DEC-5 deletes. Terminal.
            EffectKindTag::CallProvider => {
                self.provider_calls.remove(effect_id);
                self.host_effect_terminal(tag, failure)
            }
            // An unverifiable phase must not advance — that is the whole point of the gate — and a
            // second evaluation is the same intent. Terminal.
            EffectKindTag::EvaluateMilestone => self.host_effect_terminal(tag, failure),
            // The batch did not run. Answer every dispatched call with a visible error result and
            // let the model adapt: that is a *different* next request, not a retry of this one.
            EffectKindTag::ExecuteTools => {
                let root_kind = self.require_root_kind()?;
                let engine = self.engine_mut()?;
                let results = engine
                    .dispatched_tool_calls()
                    .iter()
                    .map(|call| ToolResult {
                        call_id: call.id.clone(),
                        output: Content::Text(format!(
                            "not executed: the executor could not run this batch ({}). The call \
                             did not take effect — try a different approach or a smaller step.",
                            failure.kind.as_str()
                        )),
                        durable_content: None,
                        is_error: true,
                        is_fatal: false,
                        error_kind: Some(ToolErrorKind::Fatal),
                    })
                    .collect();
                let action = engine.feed(LoopEvent::ToolResults { results });
                self.continue_after(context, action, root_kind)
            }
            // Fail closed: no approval was obtained, so nothing gated is approved. `resolve_approval`
            // with an empty verdict denies exactly the gated calls and resumes the rest.
            EffectKindTag::RequestApproval => {
                let root_kind = self.require_root_kind()?;
                let engine = self.engine_mut()?;
                let turn = engine.turn;
                // Pushed after the resolution, which clears the buffer at its head: a fact
                // recorded before it would be erased by the continuation it describes.
                let action = engine.resolve_approval(Vec::new(), Vec::new());
                engine
                    .observations
                    .push(KernelObservation::ApprovalResolutionFailed {
                        turn,
                        error: host_failure_text(failure),
                    });
                self.continue_after(context, action, root_kind)
            }
            // No task in the batch started. Charge the failure against exactly the batch the kernel
            // published and let the DAG's own dependency policy decide what that starves.
            EffectKindTag::SpawnTasks => {
                let root_kind = self.require_root_kind()?;
                let error = host_failure_text(failure);
                let engine = self.engine_mut()?;
                let failures: Vec<WorkflowSpawnFailure> = engine
                    .pending_spawn_agent_ids()
                    .into_iter()
                    .map(|agent_id| WorkflowSpawnFailure {
                        agent_id,
                        error: error.clone(),
                    })
                    .collect();
                for failed in &failures {
                    self.attempts.remove(&failed.agent_id);
                }
                let action = self
                    .engine_mut()?
                    .resolve_workflow_spawn(Vec::new(), failures);
                self.continue_after(context, action, root_kind)
            }
            // The children were not stopped. Record it and resume: re-issuing the preemption is the
            // unbounded `retry_preempt` loop DEC-5 deletes.
            EffectKindTag::PreemptTasks => {
                // for the refusal, not for the value: a resolution with no root is not a resolution
                self.require_root_kind()?;
                let error = host_failure_text(failure);
                let engine = self.engine_mut()?;
                let turn = engine.turn;
                let agent_ids: Vec<String> = match &pending.effect {
                    EffectKind::PreemptTasks(preempt) => preempt
                        .attempts
                        .iter()
                        .map(|attempt| attempt.task_id.as_str().to_string())
                        .collect(),
                    _ => Vec::new(),
                };
                engine
                    .observations
                    .push(KernelObservation::AgentPreemptFailed {
                        turn,
                        agent_ids,
                        reason: match &pending.effect {
                            EffectKind::PreemptTasks(preempt) => preempt.reason.clone(),
                            _ => String::new(),
                        },
                        error,
                    });
                Ok(self.quiet_step())
            }
            EffectKindTag::PersistMemory => {
                self.commit_memory_write(effect_id, None, Some(host_failure_text(failure)))
            }
            // The recall did not happen. The turn that asked for it resumes without it — a memory
            // search that found nothing and a memory store that was unreachable are the same shape
            // to the model, and neither is worth stalling the run for.
            EffectKindTag::QueryMemory => {
                let root_kind = self.require_root_kind()?;
                let Some(query) = self.pending_memory_queries.remove(effect_id) else {
                    return Err(unowned_resolution(effect_id, "memory query"));
                };
                let error = host_failure_text(failure);
                let engine = self.engine_mut()?;
                let turn = engine.turn;
                let action = engine.resume_after_preload();
                engine
                    .observations
                    .push(KernelObservation::MemoryQueryFailed {
                        turn,
                        scope: binding_scope(&query.binding_id),
                        query: query.text,
                        error,
                    });
                let mut step = self.continue_after(context, action, root_kind)?;
                step.focus = self.focus.clone();
                Ok(step)
            }
            // Abandon the archive: its compaction already happened in this kernel, so the run stays
            // live and degraded rather than dying on a best-effort durability effect.
            EffectKindTag::ArchivePageOut => {
                let root_kind = self.require_root_kind()?;
                let action = self
                    .engine_mut()?
                    .abandon_page_out_archive(host_failure_text(failure));
                self.continue_after(context, action, root_kind)
            }
            // DEC-5 · abandon the read. A body the host cannot produce leaves the operation exactly
            // where it was — the preview is still in context and the handle still names the
            // reference — so the model is told the read failed and takes its next turn, rather than
            // the kernel re-issuing the same load or killing a live run over one page-in.
            EffectKindTag::LoadPayload => {
                let root_kind = self.require_root_kind()?;
                let handle_id = self
                    .pending_payload_loads
                    .remove(effect_id)
                    .map(|pending| pending.handle_id)
                    .unwrap_or_default();
                let error = host_failure_text(failure);
                let engine = self.engine_mut()?;
                let turn = engine.turn;
                let action = engine.resume_after_preload();
                engine
                    .observations
                    .push(KernelObservation::PayloadLoadFailed {
                        turn,
                        handle_id,
                        error,
                    });
                let mut step = self.continue_after(context, action, root_kind)?;
                step.focus = self.focus.clone();
                Ok(step)
            }
            EffectKindTag::MeasurePrompt => Err(KernelFault::new(
                KernelFaultCode::UnexpectedEffectOutcome,
                "prompt measurement failures are reserved but adaptive measurement has no scheduler producer",
            )),
        }
    }

    /// The two effect kinds whose absence makes the operation unsound rather than degraded.
    pub(super) fn host_effect_terminal(
        &mut self,
        tag: EffectKindTag,
        failure: &HostEffectFailure,
    ) -> Result<PlannedStep, KernelFault> {
        let root_kind = self.require_root_kind()?;
        let usage = self.usage_report();
        // The engine stops too: the operation is over, and a later input must not find a loop that
        // still believes it is running.
        if let Some(engine) = self.engine.as_mut() {
            engine.close_for_host_effect_failure();
        }
        Ok(PlannedStep {
            root_kind: Some(root_kind),
            focus: self.focus.clone(),
            observations: Vec::new(),
            disposition: StepDisposition::Terminal(TerminalDisposition {
                terminal: KernelTerminal::Failed(FailedTerminal {
                    failure: KernelFailure {
                        code: KernelFailureCode::HostEffectFailed,
                        message: format!(
                            "the host could not execute this operation's {tag} effect ({}){}",
                            failure.kind.as_str(),
                            if failure.message.is_empty() {
                                String::new()
                            } else {
                                format!(": {}", failure.message)
                            }
                        ),
                    },
                    usage,
                }),
            }),
        })
    }

    /// A transition that changes kernel state but publishes nothing.
    ///
    /// Reads the folded root kind rather than taking one: a control command is admissible while the
    /// operation is still only `Configured`, so the value it reports may legitimately be `None`,
    /// and every other caller already proved a root exists through `require_root_kind`.
    pub(super) fn quiet_step(&self) -> PlannedStep {
        PlannedStep {
            root_kind: self.root_kind,
            focus: self.focus.clone(),
            observations: Vec::new(),
            disposition: StepDisposition::Effects(EffectsDisposition::default()),
        }
    }

    /// §22.13 · the memory write resolution. The record the kernel authored is the record; the
    /// host receipt contributes only its own opaque locator and digest, and never a name, kind,
    /// size, trust or provenance the kernel did not derive.
    pub(super) fn commit_memory_write(
        &mut self,
        effect_id: &EffectId,
        receipt: Option<&super::super::effect::MemoryPersistReceipt>,
        failure: Option<String>,
    ) -> Result<PlannedStep, KernelFault> {
        // for the refusal, not for the value: a resolution with no root is not a resolution
        self.require_root_kind()?;
        let Some(authored) = self.pending_memory_writes.remove(effect_id) else {
            return Err(unowned_resolution(effect_id, "memory write"));
        };
        let engine = self.engine_mut()?;
        let turn = engine.turn;
        match (receipt, failure) {
            (Some(receipt), _) => {
                engine.observations.push(KernelObservation::MemoryWritten {
                    turn,
                    record_id: receipt.record_ref.as_str().to_string(),
                    scope: binding_scope(&authored.binding_id),
                    memory_kind: core_memory_kind(authored.kind),
                    name: authored.name,
                    size_bytes: authored.size_bytes,
                });
            }
            (None, Some(error)) => {
                engine
                    .observations
                    .push(KernelObservation::MemoryWriteFailed {
                        turn,
                        // No record exists to name, so the audit fact names the *intent* — the
                        // kernel-authored key — instead of a host id it never received.
                        record_id: authored.name,
                        error,
                    });
            }
            (None, None) => unreachable!("a memory resolution is either a receipt or a failure"),
        }
        Ok(self.quiet_step())
    }

    /// §7.10 rule 4 · a body the host paged back in.
    ///
    /// Three things are checked before a byte enters context, and all three are the same question
    /// asked from different sides: *is this the body that left?* The effect must be one this kernel
    /// published, the outcome must name the handle that effect addressed, and the content must
    /// reproduce the digest the residency recorded. The kernel never saw the body, so the digest is
    /// the only evidence there is — which is why a mismatch is an
    /// [`UnexpectedEffectOutcome`](KernelFaultCode::UnexpectedEffectOutcome) with zero mutation and
    /// not a degraded read.
    pub(super) fn commit_payload_load(
        &mut self,
        context: &PlanContext<'_>,
        effect_id: &EffectId,
        loaded: &super::super::effect::PayloadLoadedSuccess,
    ) -> Result<PlannedStep, KernelFault> {
        let root_kind = self.require_root_kind()?;
        let Some(pending) = self.pending_payload_loads.get(effect_id).cloned() else {
            return Err(unowned_resolution(effect_id, "payload load"));
        };
        let mismatch = |what: &str| {
            Err(KernelFault::new(
                KernelFaultCode::UnexpectedEffectOutcome,
                format!(
                    "the payload loaded for effect {effect_id} is not the body the kernel paged \
                     out: {what}"
                ),
            ))
        };
        if loaded.handle_id.as_str() != pending.handle_id {
            return mismatch(&format!(
                "it names handle {}, but the effect addressed {}",
                loaded.handle_id, pending.handle_id
            ));
        }
        let content = loaded.payload.content.as_str();
        if loaded.payload.original_size.get() != content.len() as u64 {
            return mismatch(&format!(
                "it declares {} bytes and carries {}",
                loaded.payload.original_size,
                content.len()
            ));
        }
        if let Some(original_size) = pending.original_size
            && loaded.payload.original_size.get() != original_size
        {
            return mismatch(&format!(
                "it carries {} bytes and the handle records {original_size}",
                loaded.payload.original_size
            ));
        }
        let digest = super::super::record::canonical_digest(content.as_bytes());
        if digest.as_str() != pending.digest {
            return mismatch(&format!(
                "its content digests to {digest}, and the handle records {}",
                pending.digest
            ));
        }

        // ----- past this line the semantic engine advances -----
        self.pending_payload_loads.remove(effect_id);
        let engine = self.engine_mut()?;
        let turn = engine.turn;
        // The body enters as its own unit of history, exactly as a memory recall does: the preview
        // that stands in for it is left untouched, so nothing that was already rendered is
        // rewritten and the model reads the page-in as the answer to the read it asked for.
        let body = format!("[PAYLOAD handle_id={}]\n{content}", pending.handle_id);
        let tokens = engine.ctx.engine.count(&body).max(1);
        engine.ctx.push_history(CoreMessage::user(body), tokens);
        let previous = engine.ctx.set_payload_residency(
            &pending.handle_id,
            HandleKind::ToolResult,
            tokens,
            Residency::Resident,
        );
        let mut action = engine.resume_after_preload();
        // §5k · same rule as the memory-query resume: a sibling effect still pending means the
        // turn resumes when the last of them settles, not now.
        if !context.pending.is_empty() {
            action = LoopAction::AwaitingResume;
        }
        engine
            .observations
            .push(KernelObservation::PayloadResidencyChanged {
                turn,
                handle_id: pending.handle_id.clone(),
                from: previous.map(|residency| residency.label().to_string()),
                to: "resident".to_string(),
                payload_ref: None,
                original_size: loaded.payload.original_size.get(),
            });
        let mut step = self.continue_after(context, action, root_kind)?;
        step.focus = self.focus.clone();
        Ok(step)
    }

    /// §7.10 rule 3 / §25.9 · move each external result's P3 handle onto the reference the host
    /// supplied, and record the transfer as a fact.
    ///
    /// Runs **after** the engine accepted the batch, because the handle this moves is minted by the
    /// engine as the result enters history — there is nothing to address before that. What lands in
    /// context is the preview; what the handle now says is where the body actually is.
    pub(super) fn record_external_payloads(
        &mut self,
        payloads: &[WireToolResultPayload],
    ) -> Result<(), KernelFault> {
        for payload in payloads {
            let WireToolResultPayload::External(external) = payload else {
                continue;
            };
            let engine = self.engine_mut()?;
            let turn = engine.turn;
            let previous = engine.ctx.set_payload_residency(
                external.call_id.as_str(),
                HandleKind::ToolResult,
                // The body was never resident: only the preview is, and it is the anchored
                // message's own weight, not this handle's.
                0,
                Residency::External {
                    payload_ref: external.payload_ref.as_str().to_string(),
                    digest: external.digest.as_str().to_string(),
                    original_size: external.original_size.get(),
                },
            );
            engine
                .observations
                .push(KernelObservation::PayloadResidencyChanged {
                    turn,
                    handle_id: external.call_id.as_str().to_string(),
                    from: previous.map(|residency| residency.label().to_string()),
                    to: "external".to_string(),
                    payload_ref: Some(external.payload_ref.as_str().to_string()),
                    original_size: external.original_size.get(),
                });
        }
        Ok(())
    }

    /// The page-out receipt must describe the body the kernel handed over. A host that answers with
    /// a different handle or digest has archived something else, and accepting it would make a
    /// later page-in restore content this operation never evicted.
    pub(super) fn verify_page_out_receipt(
        &self,
        context: &PlanContext<'_>,
        effect_id: &EffectId,
        receipt: &super::super::effect::ArchiveReceipt,
    ) -> Result<(), KernelFault> {
        let Some(KernelEffect {
            effect: EffectKind::ArchivePageOut(published),
            ..
        }) = context.resolving
        else {
            return Err(unowned_resolution(effect_id, "page-out archive"));
        };
        if receipt.handle_id != published.handle_id
            || receipt.digest != published.payload.digest
            || receipt.original_size != published.payload.original_size
        {
            return Err(KernelFault::new(
                KernelFaultCode::UnexpectedEffectOutcome,
                format!(
                    "the archive receipt for effect {effect_id} names handle {} / digest {}, but \
                     the kernel published handle {} / digest {}",
                    receipt.handle_id,
                    receipt.digest,
                    published.handle_id,
                    published.payload.digest
                ),
            ));
        }
        Ok(())
    }

    /// Build the page-out effect for one compaction's archived body.
    pub(super) fn page_out_effect(
        &self,
        context: &PlanContext<'_>,
        summary: Option<&str>,
        archived: &[CoreMessage],
        effect_index: u32,
    ) -> Result<ArchivePageOutEffect, KernelFault> {
        let content = serde_json::to_string(archived).map_err(|error| {
            KernelFault::new(
                KernelFaultCode::MalformedEnvelope,
                format!("archived history is not serialisable: {error}"),
            )
        })?;
        let preview_bytes = context.config.payload_policy.preview_bytes as usize;
        let preview = summary
            .map(str::to_string)
            .unwrap_or_else(|| truncate_on_char_boundary(&content, preview_bytes));
        let handle_id = super::super::scalar::HandleId::new(format!(
            "{}:step:{}:page-out:{effect_index}",
            context.input.operation_id, context.step_seq
        ))
        .map_err(malformed)?;
        Ok(ArchivePageOutEffect {
            handle_id,
            payload: PageOutPayload {
                digest: super::super::record::canonical_digest(content.as_bytes()),
                original_size: WireU64::new(content.len() as u64),
                content,
                preview,
            },
        })
    }

    pub(super) fn require_pending_provider_call(
        &self,
        effect_id: &EffectId,
    ) -> Result<&PendingProviderCall, KernelFault> {
        self.provider_calls.get(effect_id).ok_or_else(|| {
            KernelFault::new(
                KernelFaultCode::InvalidAuthority,
                format!(
                    "effect {effect_id} is not a provider call this kernel published, so its \
                     result has no turn to continue (§7.6)"
                ),
            )
        })
    }
}
