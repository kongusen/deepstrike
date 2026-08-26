use super::*;

impl CanonicalOperationDriver {
    /// Apply one already-attributed request. Every arm reads its caller from `causation` and
    /// nothing else — there is no parameter through which a host could name a different one.
    pub(super) fn apply_syscall(
        &mut self,
        context: &PlanContext<'_>,
        causation: &SyscallCausation,
        request: &SyscallRequest,
        effect_index: &mut u32,
    ) -> Result<SyscallOutcome, SyscallRefusal> {
        let caller = causation_task(causation);
        let quarantined = self
            .engine
            .as_ref()
            .is_some_and(|engine| engine.task_quarantined(caller.as_str()));
        if quarantined && let Some(family) = privileged_family(request) {
            // §7.6 · a quarantined task read untrusted content. Letting it grow the DAG, mutate its
            // capability surface or reach memory would make the untrusted content the author of the
            // escalation. Fails closed on the whole family rather than trusting per-request
            // coercion to be exhaustive.
            return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                family,
                format!(
                    "quarantine: task {caller} is quarantined and may not widen its authority \
                     through a {family} syscall"
                ),
            )));
        }

        match request {
            SyscallRequest::SubmitWorkflow(submit) => {
                if submit.spec.nodes.is_empty() {
                    return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                        "start_workflow",
                        "an authored workflow with no nodes has nothing to spawn",
                    )));
                }
                if self
                    .engine
                    .as_ref()
                    .is_some_and(LoopStateMachine::workflow_active)
                {
                    // §10.2 flatten: the caller is already inside a DAG, so its spec grows that DAG
                    // rather than stacking a second one. One root lifecycle, one quota.
                    self.append_nodes(
                        context.config,
                        &submit.spec.nodes,
                        &caller,
                        CoreSyscall::LoadWorkflow { node_count: 0 },
                        "start_workflow",
                    )
                } else {
                    self.enter_nested_workflow(context, &submit.spec, effect_index)
                }
            }
            SyscallRequest::AppendWorkflowNodes(append) => {
                if append.nodes.is_empty() {
                    return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                        "submit_workflow_nodes",
                        "an empty submission appends nothing",
                    )));
                }
                if !self
                    .engine
                    .as_ref()
                    .is_some_and(LoopStateMachine::workflow_active)
                {
                    return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                        "submit_workflow_nodes",
                        "no workflow is in flight, so there is no graph to append to",
                    )));
                }
                self.append_nodes(
                    context.config,
                    &append.nodes,
                    &caller,
                    CoreSyscall::SubmitNodes { count: 0 },
                    "submit_workflow_nodes",
                )
            }
            SyscallRequest::ActivateSkill(activate) => {
                let engine = self.engine_mut().map_err(SyscallRefusal::Fault)?;
                if !engine.ctx.skill_available(&activate.name) {
                    return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                        "skill",
                        format!(
                            "this operation declares no skill named {:?}; activation is a \
                             capability mutation and is refused rather than invented",
                            activate.name
                        ),
                    )));
                }
                ensure_skill_grants_are_attenuated(
                    engine.ctx.skill_capability_grants(&activate.name),
                    engine.task_capabilities(caller.as_str()),
                )
                .map_err(|violations| {
                    SyscallRefusal::Rejected(SyscallRejection::new(
                        "skill",
                        skill_grant_attenuation_message(&activate.name, &violations),
                    ))
                })?;
                let expires_at_turn = activate
                    .lease_turns
                    .map(|turns| engine.turn.saturating_add(turns));
                engine
                    .ctx
                    .activate_skill_leased(activate.name.as_str(), expires_at_turn);
                Ok(SyscallOutcome::default())
            }
            SyscallRequest::UpdateTask(update) => {
                let engine = self.engine_mut().map_err(SyscallRefusal::Fault)?;
                engine.ctx.update_task(core_task_update(&update.update));
                Ok(SyscallOutcome::default())
            }
            SyscallRequest::RequestMemoryWrite(write) => {
                self.plan_memory_write(context, causation, &write.proposal, effect_index)
            }
            SyscallRequest::RequestMemoryQuery(query) => {
                self.plan_memory_query(context, causation, &query.query, effect_index)
            }
            SyscallRequest::PageIn(page_in) => {
                self.plan_page_in(context, &caller, &page_in.handle_id, effect_index)
            }
            SyscallRequest::SendMessage(send) => self.plan_send_message(&caller, send),
            SyscallRequest::PublishChannel(publish) => self.plan_publish_channel(&caller, publish),
            SyscallRequest::ReceiveMailbox(receive) => {
                self.plan_receive_mailbox(&caller, receive.limit)
            }
            SyscallRequest::ReceiveChannel(receive) => {
                self.plan_receive_channel(&caller, &receive.channel_id)
            }
            SyscallRequest::ReadObject(read) => self.plan_read_object(&caller, read.object_id),
        }
    }

    pub(super) fn plan_send_message(
        &mut self,
        caller: &TaskId,
        request: &super::super::syscall::SendMessageRequest,
    ) -> Result<SyscallOutcome, SyscallRefusal> {
        validate_ipc_labels(&request.message_id, &request.message_kind)?;
        let engine = self.engine_mut().map_err(SyscallRefusal::Fault)?;
        let handle = resolve_ipc_handle(engine, &request.payload_handle)?;
        let descriptor =
            crate::mm::handle::ObjectDescriptor::from_handle(caller.as_str().into(), &handle, 1);
        if engine
            .task_table()
            .object(descriptor.id)
            .is_some_and(|existing| existing != &descriptor)
        {
            return Err(local_ipc_refusal(
                crate::scheduler::tcb::LocalIpcError::ObjectConflict,
            ));
        }
        let now = crate::scheduler::mailbox::LogicalTime(engine.turn);
        let message = crate::scheduler::mailbox::MailboxMessage {
            id: request.message_id.as_str().into(),
            from: caller.as_str().into(),
            to: request.to.as_str().into(),
            kind: request.message_kind.as_str().into(),
            payload_handle: handle.id,
            priority: crate::types::signal::Urgency::Normal,
            timestamp: now,
            expires_at: request
                .ttl_turns
                .map(|ttl| crate::scheduler::mailbox::LogicalTime(engine.turn.saturating_add(ttl))),
        };
        let accepted = engine
            .task_table_mut()
            .send_message_from(caller.as_str(), message, now)
            .map_err(local_ipc_refusal)?;
        engine
            .task_table_mut()
            .register_object(caller.as_str(), descriptor)
            .map_err(local_ipc_refusal)?;
        engine.observe_local_runnable_tasks();
        Ok(local_ipc_outcome(accepted))
    }

    pub(super) fn plan_publish_channel(
        &mut self,
        caller: &TaskId,
        request: &super::super::syscall::PublishChannelRequest,
    ) -> Result<SyscallOutcome, SyscallRefusal> {
        validate_ipc_labels(&request.message_id, &request.message_kind)?;
        if request.channel_id.is_empty() || request.subscribers.is_empty() {
            return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                "publish_channel",
                "channel_id and subscribers must be non-empty",
            )));
        }
        let mut subscribers: Vec<_> = request
            .subscribers
            .iter()
            .map(|id| id.as_str().into())
            .collect();
        subscribers.sort_unstable();
        subscribers.dedup();
        if subscribers.len() != request.subscribers.len() {
            return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                "publish_channel",
                "channel subscribers must be unique",
            )));
        }
        let engine = self.engine_mut().map_err(SyscallRefusal::Fault)?;
        let handle = resolve_ipc_handle(engine, &request.payload_handle)?;
        let descriptor =
            crate::mm::handle::ObjectDescriptor::from_handle(caller.as_str().into(), &handle, 1);
        if engine
            .task_table()
            .object(descriptor.id)
            .is_some_and(|existing| existing != &descriptor)
        {
            return Err(local_ipc_refusal(
                crate::scheduler::tcb::LocalIpcError::ObjectConflict,
            ));
        }
        let now = crate::scheduler::mailbox::LogicalTime(engine.turn);
        let message = crate::scheduler::mailbox::MailboxMessage {
            id: request.message_id.as_str().into(),
            from: caller.as_str().into(),
            to: request.channel_id.as_str().into(),
            kind: request.message_kind.as_str().into(),
            payload_handle: handle.id,
            priority: crate::types::signal::Urgency::Normal,
            timestamp: now,
            expires_at: request
                .ttl_turns
                .map(|ttl| crate::scheduler::mailbox::LogicalTime(engine.turn.saturating_add(ttl))),
        };
        let accepted = engine
            .task_table_mut()
            .publish_channel(
                caller.as_str(),
                ChannelId(request.channel_id.as_str().into()),
                subscribers,
                message,
                now,
            )
            .map_err(local_ipc_refusal)?;
        engine
            .task_table_mut()
            .register_object(caller.as_str(), descriptor)
            .map_err(local_ipc_refusal)?;
        engine.observe_local_runnable_tasks();
        Ok(local_ipc_outcome(accepted))
    }

    pub(super) fn plan_receive_mailbox(
        &mut self,
        caller: &TaskId,
        limit: u32,
    ) -> Result<SyscallOutcome, SyscallRefusal> {
        if limit == 0 || limit > 64 {
            return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                "receive_mailbox",
                "limit must be between 1 and 64",
            )));
        }
        let engine = self.engine_mut().map_err(SyscallRefusal::Fault)?;
        let now = crate::scheduler::mailbox::LogicalTime(engine.turn);
        let messages = engine
            .task_table_mut()
            .receive_mailbox(caller.as_str(), now, limit as usize)
            .map_err(local_ipc_refusal)?;
        Ok(ipc_messages_outcome(&messages))
    }

    pub(super) fn plan_receive_channel(
        &mut self,
        caller: &TaskId,
        channel_id: &str,
    ) -> Result<SyscallOutcome, SyscallRefusal> {
        let engine = self.engine_mut().map_err(SyscallRefusal::Fault)?;
        let now = crate::scheduler::mailbox::LogicalTime(engine.turn);
        let messages = engine
            .task_table_mut()
            .receive_channel(caller.as_str(), &ChannelId(channel_id.into()), now)
            .map_err(local_ipc_refusal)?;
        Ok(ipc_messages_outcome(&messages))
    }

    pub(super) fn plan_read_object(
        &mut self,
        caller: &TaskId,
        object_id: crate::mm::handle::ObjectId,
    ) -> Result<SyscallOutcome, SyscallRefusal> {
        let engine = self.engine_mut().map_err(SyscallRefusal::Fault)?;
        let descriptor = engine
            .task_table()
            .object(object_id)
            .cloned()
            .ok_or_else(|| {
                SyscallRefusal::Rejected(SyscallRejection::new(
                    "read_object",
                    format!("object {object_id} is not registered"),
                ))
            })?;
        if descriptor.owner.as_str() != caller.as_str()
            && !crate::mm::handle::object_access_allowed_at(
                engine.task_capabilities(caller.as_str()),
                "read",
                &descriptor,
                engine.turn,
            )
        {
            return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                "read_object",
                format!("caller {caller} has no read capability for object {object_id}"),
            )));
        }
        Ok(SyscallOutcome {
            ack: Some(
                serde_json::to_string(&descriptor)
                    .expect("canonical object descriptors are serializable"),
            ),
            ..SyscallOutcome::default()
        })
    }

    /// §10.3 · gate + trust-aware append, with the spawn round deferred to the caller.
    pub(super) fn append_nodes(
        &mut self,
        config: &ResolvedOperationConfig,
        nodes: &[WireNode],
        caller: &TaskId,
        syscall: CoreSyscall,
        label: &'static str,
    ) -> Result<SyscallOutcome, SyscallRefusal> {
        // §7.3 · an authored node may not name a contract the operation never declared. Same rule
        // as a root spec, and checked on the same side of the gate as identity/acyclicity: a batch
        // with a dangling reference is malformed and must not spend quota.
        for node in nodes {
            self.require_known_contract(config, node.run_spec.as_ref())
                .map_err(|fault| {
                    SyscallRefusal::Rejected(SyscallRejection::new(label, fault.message))
                })?;
        }
        // Batch-relative identity and acyclicity are checked before the gate sees a count, so a
        // malformed batch never spends quota.
        let core_nodes = build_core_spec(&WireSpec {
            name: String::new(),
            nodes: nodes.to_vec(),
        })
        .map_err(|fault| SyscallRefusal::Rejected(SyscallRejection::new(label, fault.message)))?
        .nodes;

        let engine = self.engine_mut().map_err(SyscallRefusal::Fault)?;
        let admitted = engine.append_workflow_nodes(
            core_nodes,
            // §7.6 · the submitter is the derived caller, never an optional host field. The
            // historical `submitter_agent_id: Option<String>` erred open — omitting it skipped the
            // quarantine coercion entirely — and there is no shape here that can omit it.
            Some(caller.as_str()),
            // `append_workflow_nodes` refills the count from the batch, so the two entry points
            // cannot disagree about what they meter.
            syscall,
            label,
        );
        if !admitted {
            // `append_workflow_nodes` already pushed the rejection observation and the model-facing
            // note; re-recording it here would double the audit fact.
            return Ok(SyscallOutcome::default());
        }
        // §10.3 · deterministic node identity is a kernel fact. An appended batch lands at the end
        // of the index-addressed DAG, so mirroring its wire ids here is what lets a later spawn
        // effect and the workflow terminal still name the node the caller declared — rather than
        // falling back to the internal `wf-nodeN` id.
        self.node_ids
            .extend(nodes.iter().map(|node| node.node_id.clone()));
        self.workflow_nodes.extend_from_slice(nodes);
        Ok(SyscallOutcome {
            effects: Vec::new(),
            focus: None,
            needs_workflow_round: true,
            ack: None,
        })
    }

    pub(super) fn plan_memory_write(
        &mut self,
        context: &PlanContext<'_>,
        causation: &SyscallCausation,
        proposal: &super::super::syscall::MemoryWriteProposal,
        effect_index: &mut u32,
    ) -> Result<SyscallOutcome, SyscallRefusal> {
        let Some(binding) = context.config.memory_access.clone() else {
            return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                "write_memory",
                "this operation holds no memory binding, so it can author no memory record",
            )));
        };
        if !binding.capabilities.write {
            return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                "write_memory",
                "this operation's memory binding is read-only",
            )));
        }
        self.require_effect_support(context.config, EffectKindTag::PersistMemory)
            .map_err(SyscallRefusal::Fault)?;
        let engine = self.engine_mut().map_err(SyscallRefusal::Fault)?;
        let disposition = engine.gate_memory_write_proposal();
        if !disposition.is_allowed() {
            return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                "write_memory",
                denial_reason(&disposition, "memory write denied"),
            )));
        }
        // §22.13 · the proposal contributed name/kind/content/evidence and nothing else. Tenant,
        // author, trust, timestamp and provenance are authored here, from the operation's binding,
        // the envelope's accepted time and the derived causation.
        let authored = AuthoredMemoryWrite {
            binding_id: binding.binding_id.clone(),
            name: proposal.name.clone(),
            kind: proposal.kind,
            size_bytes: proposal.content.len() as u32,
        };
        let effect = EffectKind::PersistMemory(PersistMemoryEffect {
            binding,
            memory: CanonicalMemoryWrite {
                name: proposal.name.clone(),
                kind: proposal.kind,
                content: proposal.content.clone(),
                description: proposal.description.clone(),
                evidence_refs: proposal.evidence_refs.clone(),
                accepted_at_ms: context.input.observed_at_ms,
                causation: causation.clone(),
            },
        });
        let published = self.mint_effect(context, effect, effect_index);
        self.pending_memory_writes
            .insert(published.effect_id.clone(), authored);
        Ok(SyscallOutcome {
            effects: vec![published],
            focus: None,
            needs_workflow_round: false,
            ack: None,
        })
    }

    pub(super) fn plan_memory_query(
        &mut self,
        context: &PlanContext<'_>,
        causation: &SyscallCausation,
        proposal: &super::super::syscall::MemoryQueryProposal,
        effect_index: &mut u32,
    ) -> Result<SyscallOutcome, SyscallRefusal> {
        let Some(binding) = context.config.memory_access.clone() else {
            return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                "query_memory",
                "this operation holds no memory binding, so it can read no memory record",
            )));
        };
        if !binding.capabilities.read {
            return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                "query_memory",
                "this operation's memory binding is write-only",
            )));
        }
        self.require_effect_support(context.config, EffectKindTag::QueryMemory)
            .map_err(SyscallRefusal::Fault)?;
        // The retrieval width is the operation's policy, clamped — the host does not re-decide it
        // and a model cannot widen it by asking for more.
        let ceiling = context.config.memory_policy.retrieval_top_k;
        let requested_k = proposal.limit.unwrap_or(ceiling).clamp(1, ceiling);
        let authored = AuthoredMemoryQuery {
            binding_id: binding.binding_id.clone(),
            text: proposal.text.clone(),
            requested_k,
        };
        let effect = EffectKind::QueryMemory(QueryMemoryEffect {
            binding,
            query: CanonicalMemoryQuery {
                text: proposal.text.clone(),
                kinds: proposal.kinds.clone(),
                accepted_at_ms: context.input.observed_at_ms,
                causation: causation.clone(),
            },
            requested_k,
        });
        let published = self.mint_effect(context, effect, effect_index);
        self.pending_memory_queries
            .insert(published.effect_id.clone(), authored);
        Ok(SyscallOutcome {
            effects: vec![published],
            focus: None,
            needs_workflow_round: false,
            ack: None,
        })
    }

    /// §7.6 / §7.10 rule 4 · `read_result` reduces to exactly one thing: a `LoadPayload` effect for
    /// a body the caller already holds an address for.
    ///
    /// Two refusals, both *rejections* rather than faults — the caller was established, so what is
    /// refused is the address it named, and the transition that carried it still commits with an
    /// audit fact the model reads on its next turn:
    ///
    /// - an address that is not in this operation's handle table. A page-in reaches only what the
    ///   caller already holds, which is what stops `read_result` from becoming a general read
    ///   primitive — the historical SDK answered it by scanning a spool directory and then the
    ///   session log, so any path-shaped string was a readable address.
    /// - an address whose body core still holds. `Resident` and `Collapsed` are not paged out at
    ///   all. Neither yields a locator the kernel could hand back,
    ///   and fabricating one is exactly the confusion the closed union removes.
    pub(super) fn plan_page_in(
        &mut self,
        context: &PlanContext<'_>,
        caller: &TaskId,
        handle_id: &super::super::scalar::HandleId,
        effect_index: &mut u32,
    ) -> Result<SyscallOutcome, SyscallRefusal> {
        let engine = self.engine_mut().map_err(SyscallRefusal::Fault)?;
        if let Some(handle) = engine.ctx.handles.all().iter().find(|handle| {
            handle.source.as_deref() == Some(handle_id.as_str())
                || handle.id.to_string() == handle_id.as_str()
        }) && let Some(descriptor) = engine.task_table().object(handle.id)
            && descriptor.owner.as_str() != caller.as_str()
            && !crate::mm::handle::object_access_allowed_at(
                engine.task_capabilities(caller.as_str()),
                "read",
                descriptor,
                engine.turn,
            )
        {
            return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                READ_RESULT_TOOL_NAME,
                format!(
                    "caller {caller} has no read capability for shared object {}",
                    descriptor.id
                ),
            )));
        }
        let Some(residency) = engine.ctx.payload_residency(handle_id.as_str()).cloned() else {
            return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                READ_RESULT_TOOL_NAME,
                format!(
                    "handle {handle_id} is not reachable in this operation's handle table; a \
                     page-in addresses only what the caller already holds"
                ),
            )));
        };
        let (Some(payload_ref), Some(digest)) = (residency.payload_ref(), residency.digest())
        else {
            return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                READ_RESULT_TOOL_NAME,
                format!(
                    "handle {handle_id} is {} — its body is held by this kernel, not by the \
                     payload store, so there is nothing to page in",
                    residency.label()
                ),
            )));
        };
        let payload_ref = PayloadRef::new(payload_ref).map_err(|error| {
            SyscallRefusal::Fault(KernelFault::new(
                KernelFaultCode::MalformedEnvelope,
                format!(
                    "handle {handle_id} records an unusable payload locator: {}",
                    error.message
                ),
            ))
        })?;
        self.require_effect_support(context.config, EffectKindTag::LoadPayload)
            .map_err(SyscallRefusal::Fault)?;
        let effect = EffectKind::LoadPayload(LoadPayloadEffect {
            handle_id: handle_id.clone(),
            payload_ref,
        });
        let published = self.mint_effect(context, effect, effect_index);
        self.pending_payload_loads.insert(
            published.effect_id.clone(),
            PendingPayloadLoad {
                handle_id: handle_id.as_str().to_string(),
                digest: digest.to_string(),
                original_size: match &residency {
                    Residency::External { original_size, .. } => Some(*original_size),
                    _ => None,
                },
            },
        );
        Ok(SyscallOutcome {
            effects: vec![published],
            focus: None,
            needs_workflow_round: false,
            ack: None,
        })
    }

    /// Record a refused request as an audit fact the model reads on its next turn (§7.6, §7.7).
    pub(super) fn note_rejection(&mut self, rejection: SyscallRejection) {
        let Some(engine) = self.engine.as_mut() else {
            return;
        };
        let note = crate::scheduler::rollback::build_control_rejection_note(
            rejection.operation,
            &rejection.reason,
            engine.ctx.config.verbose_control_notes,
        );
        engine.ctx.push_signal(note);
        let turn = engine.turn;
        engine
            .observations
            .push(KernelObservation::ControlRequestRejected {
                turn,
                operation: rejection.operation.to_string(),
                subject: rejection.subject,
                reason: rejection.reason,
            });
    }
}
