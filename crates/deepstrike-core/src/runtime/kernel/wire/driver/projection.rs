use super::*;

impl CanonicalOperationDriver {
    // ----- §12.1 · the logical-state projection -----

    /// Project the three driver-owned partitions of §12.1, plus the two transition fields the
    /// driver rather than the transaction owns.
    ///
    /// Explicitly a **projection**, not a serialisation: every value below is read through a named
    /// accessor and written into a canonical DTO field. That is the whole point of §12.1 — adding a
    /// field to [`LoopStateMachine`] must not change the checkpoint format, and a checkpoint field
    /// must not silently vanish because an internal one was renamed. It is also why the internal
    /// enums travel as their `label()` plus their carried data: `TaskLifecycle::Done(reason)` and
    /// `Residency::External { .. }` are semantic-kernel shapes, and mirroring them would make the
    /// checkpoint a checkpoint of a private layout.
    pub fn project_logical_state(&self) -> LogicalStateProjection {
        LogicalStateProjection {
            root_kind: self.root_kind,
            focus: self.focus.clone(),
            syscall: self.project_syscall_state(),
            scheduler: self.project_scheduler_state(),
            context_vm: self.project_context_vm_state(),
        }
    }

    pub(super) fn project_syscall_state(&self) -> SyscallState {
        SyscallState {
            policy_revision: self.policy.as_ref().map(LivePolicyState::revision),
            live_config: self.policy.as_ref().map(|policy| policy.config().clone()),
            provider_calls: self
                .provider_calls
                .iter()
                .map(|(effect_id, call)| PendingProviderCallState {
                    effect_id: effect_id.clone(),
                    task_id: call.task_id.clone(),
                    exposed_tools: call.exposed_tools.iter().cloned().collect(),
                })
                .collect(),
            consumed_call_ids: self.consumed_calls.iter().cloned().collect(),
            authored_memory_writes: self
                .pending_memory_writes
                .iter()
                .map(|(effect_id, write)| AuthoredMemoryWriteState {
                    effect_id: effect_id.clone(),
                    binding_id: write.binding_id.clone(),
                    name: write.name.clone(),
                    kind: write.kind,
                    size_bytes: write.size_bytes,
                    model_facing: write.model_facing,
                })
                .collect(),
            authored_memory_queries: self
                .pending_memory_queries
                .iter()
                .map(|(effect_id, query)| AuthoredMemoryQueryState {
                    effect_id: effect_id.clone(),
                    binding_id: query.binding_id.clone(),
                    text: query.text.clone(),
                    requested_k: query.requested_k,
                    record_id: query.record_id.clone(),
                    recall: query.recall,
                })
                .collect(),
            memory_write_window_ms: self
                .engine
                .as_ref()
                .map(|engine| {
                    engine
                        .memory_write_window()
                        .iter()
                        .copied()
                        .map(WireU64::new)
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    pub(super) fn project_scheduler_state(&self) -> SchedulerState {
        let Some(engine) = self.engine.as_ref() else {
            return SchedulerState::default();
        };
        let (total_tokens, subagents_spawned, rounds_completed) = engine.local_budget_usage();
        let signal_state = engine.signal_checkpoint_state();
        let entropy_state = engine.entropy_checkpoint_state();
        let workflow = engine.workflow_checkpoint_nodes().map(|runtime_nodes| {
            let workflow_id = self
                .workflow_id
                .as_ref()
                .expect("an active canonical workflow has a logical identity");
            assert_eq!(
                runtime_nodes.len(),
                self.workflow_nodes.len(),
                "the semantic workflow and its canonical source DAG stay index-aligned"
            );
            WorkflowGraphState {
                workflow_id: workflow_id.clone(),
                nodes: runtime_nodes
                    .into_iter()
                    .zip(self.workflow_nodes.iter())
                    .map(|(runtime, wire)| WorkflowNodeState {
                        node_id: wire.node_id.clone(),
                        task: wire.task.clone(),
                        depends_on: wire.depends_on.clone(),
                        run_spec: wire.run_spec.clone(),
                        status: workflow_status_label(runtime.status).to_string(),
                        active_agent_id: runtime.active_agent_id,
                    })
                    .collect(),
            }
        });
        SchedulerState {
            run_spec: engine.run_spec.as_ref().map(logical_agent_run_spec),
            advertised_tool_ids: engine.advertised_tool_ids(),
            turn: engine.turn,
            total_tokens: WireU64::new(total_tokens),
            rounds_completed,
            subagents_spawned,
            started_at_ms: engine.started_at_ms().map(WireU64::new),
            wall_budget_ms: engine.wall_budget().map(WireU64::new),
            tasks: engine
                .task_table()
                .all()
                .iter()
                .map(|tcb| TaskControlState {
                    task_id: TaskId::new(tcb.id.as_str())
                        .expect("an internal task id is always a legal branded ref"),
                    parent_task_id: tcb
                        .parent
                        .as_ref()
                        .and_then(|parent| TaskId::new(parent.as_str()).ok()),
                    lifecycle: tcb.state.label().to_string(),
                    runnable_cause: tcb.runnable_cause,
                    termination: match tcb.state {
                        TaskLifecycle::Done(reason) => Some(reason.label().to_string()),
                        _ => None,
                    },
                    wait_set: tcb.wait_set.as_ref().map(project_wait_set),
                    capability_ids: tcb.caps.iter().map(|cap| cap.to_string()).collect(),
                    capabilities: tcb.capabilities.clone(),
                    process: tcb.proc.as_ref().map(|process| ChildProcessState {
                        role: agent_role_label(process.role).to_string(),
                        isolation: agent_isolation_label(process.isolation).to_string(),
                        context_inheritance: context_inheritance_label(process.context_inheritance)
                            .to_string(),
                        join_result: process.result.as_ref().map(|result| {
                            super::super::scalar::BoundedJson::new(
                                serde_json::to_value(result)
                                    .expect("a child join result is serializable"),
                            )
                            .expect("a child join result is bounded")
                        }),
                    }),
                    supervision: tcb.supervision.clone(),
                    supervision_events: tcb.supervision_events.clone(),
                    tokens_used: WireU64::new(tcb.budget.total_tokens),
                    turns_used: tcb.budget.turns,
                    child_budget_remaining: tcb.child_budget_remaining,
                    budget_grant: tcb.budget_grant.clone(),
                    mailbox: tcb.mailbox.clone(),
                })
                .collect(),
            attempts: self
                .attempts
                .iter()
                .filter_map(|(task_id, attempt_id)| {
                    Some(TaskAttemptState {
                        task_id: TaskId::new(task_id.as_str()).ok()?,
                        attempt_id: attempt_id.clone(),
                    })
                })
                .collect(),
            workflow,
            queued_signals: signal_state
                .queued
                .into_iter()
                .map(|queued| queued_signal_state(&queued))
                .collect(),
            signal_dedupe_keys: signal_state
                .seen_order
                .into_iter()
                .map(|key| key.to_string())
                .collect(),
            milestone: self
                .loaded_contract_id
                .as_ref()
                .map(|contract_id| MilestoneState {
                    contract_id: contract_id.clone(),
                    phase_id: engine.current_milestone_phase_id().map(str::to_string),
                    complete: engine.is_milestone_complete(),
                    blocked_count: engine.milestone_blocked_count(),
                }),
            entropy: EntropyState {
                window: entropy_state
                    .window
                    .into_iter()
                    .map(|entry| EntropyTurnState {
                        errored_results: entry.errored_results,
                        total_results: entry.total_results,
                        rollbacks: entry.rollbacks,
                    })
                    .collect(),
                rollbacks_pending: entropy_state.rollbacks_pending,
                disarmed: entropy_state.disarmed,
                last_alert_turn: entropy_state.last_alert_turn,
            },
            channels: engine
                .task_table()
                .channels()
                .iter()
                .map(|(channel_id, channel)| LocalChannelState {
                    channel_id: channel_id.0.to_string(),
                    channel: channel.clone(),
                })
                .collect(),
            objects: engine.task_table().objects().values().cloned().collect(),
        }
    }

    pub(super) fn project_context_vm_state(&self) -> ContextVmState {
        let Some(engine) = self.engine.as_ref() else {
            return ContextVmState::default();
        };
        let ctx = &engine.ctx;
        ContextVmState {
            system_measurements: ctx.partitions.system.measurements().to_vec(),
            history_measurements: ctx.partitions.history.measurements().to_vec(),
            knowledge_reference_step: ctx.knowledge_checkpoint_state().0,
            knowledge_budget_warned: ctx.knowledge_checkpoint_state().1,
            prefix_sent: ctx.cache_checkpoint_state().0,
            cache_invalidated_tokens: ctx.cache_checkpoint_state().1,
            cache_ledger: ctx.cache_ledger.clone(),
            evicted_knowledge_keys: ctx.partitions.knowledge.evicted_keys.iter().cloned().collect(),
            knowledge_regret: ctx.partitions.knowledge.regret,
            knowledge_wasted_tokens: ctx.partitions.knowledge.wasted_tokens,
            recall_top_k: ctx.recall_top_k,
            recall_due: ctx.recall_due,
            dependency_bodies: ctx.dependency_bodies.clone(),
            handles: ctx
                .handles
                .all()
                .iter()
                .map(|handle| {
                    let (payload_ref, digest, original_size) = match &handle.residency {
                        Residency::External {
                            payload_ref,
                            digest,
                            original_size,
                        } => (
                            Some(payload_ref.clone()),
                            Some(digest.clone()),
                            Some(WireU64::new(*original_size)),
                        ),
                        Residency::PagedOut {
                            payload_ref,
                            digest,
                        } => (Some(payload_ref.clone()), Some(digest.clone()), None),
                        Residency::Resident | Residency::Collapsed => (None, None, None),
                    };
                    HandleState {
                        handle_id: handle.id,
                        kind: handle_kind_label(&handle.kind).to_string(),
                        residency: handle.residency.label().to_string(),
                        payload_ref,
                        digest,
                        original_size,
                        tokens: handle.tokens,
                        source: handle.source.as_ref().map(|source| source.to_string()),
                        item: handle.item,
                    }
                })
                .collect(),
            next_handle_id: ctx.next_handle_id(),
            pending_payload_loads: self
                .pending_payload_loads
                .iter()
                .map(|(effect_id, load)| PendingPayloadLoadState {
                    effect_id: effect_id.clone(),
                    handle_id: load.handle_id.clone(),
                    digest: load.digest.clone(),
                    original_size: load.original_size.map(WireU64::new),
                })
                .collect(),
            active_skills: ctx
                .active_skills
                .iter()
                .map(|(skill, lease)| SkillLeaseState {
                    skill: skill.to_string(),
                    lease_until_turn: *lease,
                })
                .collect(),
            knowledge: ctx
                .partitions
                .knowledge
                .entries
                .iter()
                .map(|entry| KnowledgeSlotState {
                    id: entry.id,
                    key: entry.key.as_ref().map(|key| key.to_string()),
                    role: role_label(entry.message.role).to_string(),
                    body: self.project_body(&entry.message),
                    tokens: entry.tokens,
                    pinned: entry.pinned,
                    evict_at_boundary: entry.is_leaving(),
                    tool_calls: entry
                        .message
                        .tool_calls
                        .iter()
                        .map(|call| LogicalToolCall {
                            call_id: call.id.to_string(),
                            name: call.name.to_string(),
                            arguments: call.arguments.to_string(),
                        })
                        .collect(),
                    pending: entry.pending.as_ref().map(|pending| {
                        Box::new(self.project_message(
                            MessagePartition::System,
                            &pending.0,
                            pending.1,
                        ))
                    }),
                    use_count: entry.value.ref_count,
                    last_used_step: entry.value.last_referenced_step,
                    staged: entry.staged,
                    origin: Some(entry.origin),
                    lease: Some(entry.lease),
                    leaving: entry.leaving,
                    injected_turn: entry.value.injected_turn,
                    last_referenced_turn: entry.value.last_referenced_turn,
                    expanded: entry.value.expanded,
                    paged_in_after_evict: entry.value.paged_in_after_evict,
                    strong_refs: entry.value.strong_refs,
                    lexical_refs: entry.value.lexical_refs,
                })
                .collect(),
            retired_items: ctx.partitions.knowledge.retired.clone(),
            item_turn: ctx.partitions.knowledge.turn,
            history_items: (0..ctx.partitions.history.len())
                .map(|index| ctx.partitions.history.tag(index))
                .collect(),
            last_item_id: ctx.last_item_id,
            item_edges: ctx.graph.edges().to_vec(),
            item_edges_omitted: ctx.graph.omitted(),
            item_external_edges: ctx.graph.external().to_vec(),
            history_tally: ctx.history_tally.values().copied().collect(),
            retired_history: ctx.retired_history.iter().copied().collect(),
            retired_history_omitted: ctx.retired_history_omitted,
            last_request_estimate: ctx.last_request_estimate,
            item_replays: ctx
                .item_replays
                .iter()
                .map(|(item, (replay, _))| super::super::checkpoint::ItemReplayState {
                    item: *item,
                    replay: replay.clone(),
                })
                .collect(),
            estimate_samples: ctx
                .estimate_samples
                .iter()
                .map(|&(estimate, count)| [estimate, count])
                .collect(),
            held_history: ctx
                .held_history
                .iter()
                .map(|(message, tokens)| {
                    self.project_message(MessagePartition::History, message, *tokens)
                })
                .collect(),
            signals: ctx.partitions.signals.clone(),
            messages: ctx
                .partitions
                .system
                .messages()
                .iter()
                .enumerate()
                .map(|(index, message)| {
                    self.project_message(
                        MessagePartition::System,
                        message,
                        ctx.partitions
                            .system
                            .measurements()
                            .get(index)
                            .map(|measurement| measurement.tokens)
                            .unwrap_or_else(|| ctx.engine.count_message(message)),
                    )
                })
                .chain(ctx.partitions.history.messages().iter().enumerate().map(
                    |(index, message)| {
                        self.project_message(
                            MessagePartition::History,
                            message,
                            ctx.partitions
                                .history
                                .measurements()
                                .get(index)
                                .map(|measurement| measurement.tokens)
                                .unwrap_or_else(|| ctx.engine.count_message(message)),
                        )
                    },
                ))
                .collect(),
            task_state: project_task_state(&ctx.partitions.task_state),
            partition_tokens: PartitionTokenState {
                system: ctx.partitions.system.token_count(),
                knowledge: ctx.partitions.knowledge.token_count,
                history: ctx.partitions.history.token_count(),
            },
            history_len: ctx.partitions.history.len() as u32,
            frozen_history_len: ctx.frozen_history_len() as u32,
            last_activity_ms: WireU64::new(ctx.last_activity_ms),
            last_compact_ms: ctx.last_compact_ms.map(WireU64::new),
        }
    }

    /// §5q-2 · one stored message, projected.
    pub(super) fn project_message(
        &self,
        partition: MessagePartition,
        message: &CoreMessage,
        tokens: u32,
    ) -> StoredMessageState {
        StoredMessageState {
            partition,
            role: role_label(message.role).to_string(),
            body: self.project_body(message),
            tool_calls: message
                .tool_calls
                .iter()
                .map(|call| LogicalToolCall {
                    call_id: call.id.to_string(),
                    name: call.name.to_string(),
                    arguments: call.arguments.to_string(),
                })
                .collect(),
            tokens,
        }
    }

    /// §7.10 · inline or by reference, decided by the message's own residency.
    ///
    /// The rule is not "is this body big" but "where does this body live": a result whose handle
    /// says `External` was never resident in the first place (only its preview is in the message),
    /// and one that says `PagedOut` left under pressure and lives with the host now. Either way the
    /// checkpoint carries the reference and the digest that verifies a page-in — putting the bytes
    /// back would re-create exactly the round trip §7.10 exists to delete.
    pub(super) fn project_body(&self, message: &CoreMessage) -> StoredMessageBody {
        // External/paged-out tool results must retain their reference form. Their text projection
        // is represented as a `DurableToolResult`, but choosing that form first
        // would discard the handle digest and make the body unreachable after restore.
        let single_tool_result_is_external =
            match &message.content {
                Content::Parts(parts) if parts.len() == 1 => match &parts[0] {
                    ContentPart::ToolResult { call_id, .. } => {
                        self.engine
                            .as_ref()
                            .and_then(|engine| {
                                engine.ctx.handles.all().iter().find(|handle| {
                                    handle.source.as_deref() == Some(call_id.as_str())
                                })
                            })
                            .is_some_and(|handle| handle.residency.digest().is_some())
                    }
                    _ => false,
                },
                _ => false,
            };
        if !single_tool_result_is_external {
            if let Some(results) = durable_tool_results_from_content(&message.content) {
                return StoredMessageBody::Structured(StructuredMessageBody {
                    durable_content: None,
                    durable_tool_results: results,
                });
            }
            if let Some(result) = durable_tool_result_from_content(&message.content) {
                return StoredMessageBody::Structured(StructuredMessageBody {
                    durable_content: None,
                    durable_tool_results: vec![result],
                });
            }
        }
        let Some((text, tool_call_id, is_error)) = message_body_parts(message) else {
            let durable_content = content_to_durable(&message.content)
                .expect("every non-tool message uses the canonical durable content model");
            return StoredMessageBody::Structured(StructuredMessageBody {
                durable_content: Some(durable_content),
                durable_tool_results: Vec::new(),
            });
        };
        let Some(call_id) = tool_call_id.as_deref() else {
            return StoredMessageBody::Inline(InlineMessageBody {
                text,
                tool_call_id,
                is_error,
            });
        };
        let referenced = self.engine.as_ref().and_then(|engine| {
            let handle = engine
                .ctx
                .handles
                .all()
                .iter()
                .find(|handle| handle.source.as_deref() == Some(call_id))?;
            let digest = handle.residency.digest()?;
            Some((
                handle.id,
                digest.to_string(),
                matches!(handle.residency, Residency::PagedOut { .. }),
            ))
        });
        match referenced {
            Some((handle_id, digest, is_paged_out)) => {
                StoredMessageBody::Reference(ReferencedMessageBody {
                    handle_id,
                    digest,
                    preview: if is_paged_out {
                        crate::context::renderer::collapse_preview(&text, call_id)
                    } else {
                        truncate_on_char_boundary(&text, self.preview_bytes())
                    },
                    tool_call_id,
                    is_error,
                })
            }
            None => StoredMessageBody::Inline(InlineMessageBody {
                text,
                tool_call_id,
                is_error,
            }),
        }
    }

    pub(super) fn preview_bytes(&self) -> usize {
        self.policy
            .as_ref()
            .map(|policy| policy.config().payload_policy.preview_bytes as usize)
            .unwrap_or(2 * 1024)
    }

    // ----- §12.2 · the logical-state restore -----

    /// Rebuild a driver from a checkpoint's logical state (§12.2 line 3).
    ///
    /// The exact inverse of [`Self::project_logical_state`], and deliberately nothing more: every
    /// value written here is a value the projection reads back, so "did the restore work" is not a
    /// judgement call — [`super::super::restore::restore_operation`] re-projects immediately afterwards and
    /// compares the digest. A field this function forgets therefore fails the restore rather than
    /// producing a runtime that is quietly one field short of the one that crashed.
    ///
    /// Task 16b makes every scheduler branch invertible here: workflow source nodes rebuild their
    /// private graph indexes, queued signals rebuild priority and dedupe state, and child process
    /// identity is restored without re-running permission defaults. Unknown labels and inconsistent
    /// relationships still fail closed as `CheckpointIncompatible`.
    pub fn restore_logical_state(
        genesis_config: &ResolvedOperationConfig,
        state: &LogicalKernelState,
    ) -> Result<Self, KernelFault> {
        let mut driver = Self::new();
        let live_config = state
            .syscall
            .live_config
            .clone()
            .unwrap_or_else(|| genesis_config.clone());

        // The engine is built from the configuration and then *moved* onto the checkpointed facts;
        // it is never deserialised. Boot-only axes come from the genesis configuration the record
        // froze, live-mutable ones from the patched configuration the checkpoint carries.
        let mut engine = build_engine(genesis_config);
        install_live_policies(&mut engine, &live_config);
        engine.set_root_workflow(state.transition.root_kind == Some(RootKind::Workflow));
        driver.policy = Some(LivePolicyState::restore(
            state.syscall.policy_revision.unwrap_or(WireU64::ZERO),
            live_config.clone(),
        ));

        restore_scheduler(&mut engine, &live_config, &state.scheduler)?;
        if let Some(preempt) =
            state
                .transition
                .pending_effects
                .iter()
                .find_map(|effect| match &effect.effect {
                    EffectKind::PreemptTasks(preempt) => Some(preempt),
                    _ => None,
                })
        {
            engine.restore_pending_preempt(
                preempt
                    .attempts
                    .iter()
                    .map(|attempt| attempt.task_id.as_str().to_string())
                    .collect(),
                preempt.reason.clone(),
            );
        }
        restore_context_vm(&mut engine, &state.context_vm)?;
        engine.restore_memory_write_window(
            state
                .syscall
                .memory_write_window_ms
                .iter()
                .map(|at| at.get())
                .collect(),
        );

        if let Some(milestone) = &state.scheduler.milestone {
            let contract = live_config
                .verification_contract(&milestone.contract_id)
                .ok_or_else(|| {
                    KernelFault::new(
                        KernelFaultCode::CheckpointIncompatible,
                        format!(
                            "the checkpoint runs verification contract {:?}, which this \
                             operation's configuration no longer declares",
                            milestone.contract_id
                        ),
                    )
                })?;
            engine.load_milestone_contract(core_milestone_contract(contract, &live_config));
            if !engine
                .restore_milestone_cursor(milestone.phase_id.as_deref(), milestone.blocked_count)
            {
                return Err(KernelFault::new(
                    KernelFaultCode::CheckpointIncompatible,
                    format!(
                        "the checkpoint sits on milestone phase {:?} of contract {:?}, which that \
                         contract does not declare",
                        milestone.phase_id, milestone.contract_id
                    ),
                ));
            }
            driver.loaded_contract_id = Some(milestone.contract_id.clone());
        }

        driver.engine = Some(engine);
        driver.root_kind = state.transition.root_kind;
        driver.focus = state.transition.focus.clone();
        if let Some(workflow) = &state.scheduler.workflow {
            driver.workflow_id = Some(workflow.workflow_id.clone());
            driver.node_ids = workflow
                .nodes
                .iter()
                .map(|node| node.node_id.clone())
                .collect();
            driver.workflow_nodes = workflow
                .nodes
                .iter()
                .map(|node| WireNode {
                    node_id: node.node_id.clone(),
                    task: node.task.clone(),
                    depends_on: node.depends_on.clone(),
                    run_spec: node.run_spec.clone(),
                })
                .collect();
        }
        driver.attempts = state
            .scheduler
            .attempts
            .iter()
            .map(|attempt| {
                (
                    attempt.task_id.as_str().to_string(),
                    attempt.attempt_id.clone(),
                )
            })
            .collect();
        driver.provider_calls = state
            .syscall
            .provider_calls
            .iter()
            .map(|call| {
                (
                    call.effect_id.clone(),
                    PendingProviderCall {
                        task_id: call.task_id.clone(),
                        exposed_tools: call.exposed_tools.iter().cloned().collect(),
                    },
                )
            })
            .collect();
        driver.consumed_calls = state.syscall.consumed_call_ids.iter().cloned().collect();
        driver.pending_memory_writes = state
            .syscall
            .authored_memory_writes
            .iter()
            .map(|write| {
                (
                    write.effect_id.clone(),
                    AuthoredMemoryWrite {
                        binding_id: write.binding_id.clone(),
                        name: write.name.clone(),
                        kind: write.kind,
                        size_bytes: write.size_bytes,
                        model_facing: write.model_facing,
                    },
                )
            })
            .collect();
        driver.pending_memory_queries = state
            .syscall
            .authored_memory_queries
            .iter()
            .map(|query| {
                (
                    query.effect_id.clone(),
                    AuthoredMemoryQuery {
                        binding_id: query.binding_id.clone(),
                        text: query.text.clone(),
                        requested_k: query.requested_k,
                        record_id: query.record_id.clone(),
                        recall: query.recall,
                    },
                )
            })
            .collect();
        driver.pending_payload_loads = state
            .context_vm
            .pending_payload_loads
            .iter()
            .map(|load| {
                (
                    load.effect_id.clone(),
                    PendingPayloadLoad {
                        handle_id: load.handle_id.clone(),
                        digest: load.digest.clone(),
                        original_size: load.original_size.map(WireU64::get),
                    },
                )
            })
            .collect();
        Ok(driver)
    }
}
