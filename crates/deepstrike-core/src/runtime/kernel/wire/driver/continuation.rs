use super::*;

impl CanonicalOperationDriver {
    /// The shared tail of every non-start transition: turn the engine's action into a disposition
    /// and re-derive the focus from what the engine actually did.
    pub(super) fn continue_after(
        &mut self,
        context: &PlanContext<'_>,
        action: LoopAction,
        root_kind: RootKind,
    ) -> Result<PlannedStep, KernelFault> {
        let mut index = 0;
        self.continue_after_at(context, action, root_kind, &mut index)
    }

    pub(super) fn continue_after_at(
        &mut self,
        context: &PlanContext<'_>,
        action: LoopAction,
        root_kind: RootKind,
        effect_index: &mut u32,
    ) -> Result<PlannedStep, KernelFault> {
        let workflow_finished = self
            .engine()
            .map(|engine| {
                engine
                    .observations
                    .iter()
                    .any(|o| matches!(o, KernelObservation::WorkflowCompleted { .. }))
            })
            .unwrap_or(false);
        let disposition = self.disposition_for_at(context, action, root_kind, effect_index)?;
        let focus = if workflow_finished {
            // §6.1.7 / §7.4: a nested workflow's completion restores the parent agent turn; a root
            // workflow's completion commits the terminal instead and its focus stops moving.
            match (root_kind, &self.focus) {
                (RootKind::Agent, Some(ExecutionFocus::WorkflowController(controller))) => {
                    controller
                        .parent_task_id
                        .clone()
                        .map(ExecutionFocus::agent_turn)
                }
                (_, current) => current.clone(),
            }
        } else {
            self.focus.clone()
        };
        Ok(PlannedStep {
            root_kind: Some(root_kind),
            focus,
            observations: Vec::new(),
            disposition,
        })
    }

    /// Project one [`LoopAction`] onto §7.12's closed step disposition.
    pub(super) fn disposition_for(
        &mut self,
        context: &PlanContext<'_>,
        action: LoopAction,
        root_kind: RootKind,
    ) -> Result<StepDisposition, KernelFault> {
        let mut index = 0;
        self.disposition_for_at(context, action, root_kind, &mut index)
    }

    /// The same projection, minting effect ids from a caller-owned counter. A step that reduces a
    /// syscall batch publishes several effects, and each one's identity has to stay unique.
    pub(super) fn disposition_for_at(
        &mut self,
        context: &PlanContext<'_>,
        action: LoopAction,
        root_kind: RootKind,
        effect_index: &mut u32,
    ) -> Result<StepDisposition, KernelFault> {
        let operation_id = &context.input.operation_id;
        let causation = context.input.input_id.clone();
        let step_seq = context.step_seq;

        // A durability effect produced *inside* the transition (a compaction's page-out) is
        // published first and holds the continuation. The
        // guard inside makes a second call in the same step a no-op, so a step never activates two.
        let mut action = self.engine_mut()?.externalize_pending_host_effect(action);
        // DEC-8 · an archive the host never declared it can perform is not a reason to refuse the
        // transition that produced it: the compaction already happened in-kernel and the archive is
        // best effort. It is abandoned through the same one-decision path a host failure takes, so
        // the audit fact is identical whether the host said "I cannot" or "I could not".
        while matches!(action, LoopAction::ArchivePageOut { .. })
            && !context
                .config
                .host_effect_support
                .supports(EffectKindTag::ArchivePageOut)
        {
            action = self.engine_mut()?.abandon_page_out_archive(
                "this operation's host declares no archive_page_out support".to_string(),
            );
        }

        let effect = match action {
            LoopAction::AwaitingResume => {
                // A root workflow that just drained its DAG terminates here, and publishes nothing.
                if root_kind == RootKind::Workflow
                    && let Some(terminal) = self.root_workflow_terminal()
                {
                    return Ok(StepDisposition::Terminal(TerminalDisposition { terminal }));
                }
                return Ok(StepDisposition::Effects(EffectsDisposition::default()));
            }
            LoopAction::Done { result } => {
                return Ok(StepDisposition::Terminal(TerminalDisposition {
                    terminal: agent_terminal(&result),
                }));
            }
            LoopAction::CallLLM {
                context: rendered,
                tools,
            } => {
                let context_fault = |error: String| {
                    KernelFault::new(
                        KernelFaultCode::InvalidLifecycle,
                        format!("provider context preparation failed: {error}"),
                    )
                };
                let policy_bytes =
                    super::super::record::canonical_bytes(&context.config.context_policy)
                        .map_err(|error| context_fault(error.to_string()))?;
                let policy_digest =
                    crate::evolution::ContentDigest::from_bytes(policy_bytes.as_slice());
                let engine = self
                    .engine()
                    .ok_or_else(|| context_fault("missing engine".into()))?;
                let (mut context_candidate, prepared) = engine
                    .ctx
                    .prepare_candidate(
                        operation_id.to_string(),
                        format!("{operation_id}:step:{step_seq}"),
                        step_seq.get(),
                        policy_digest,
                    )
                    .map_err(|error| context_fault(error.to_string()))?;
                let prepared_bytes = super::super::record::canonical_bytes(&prepared)
                    .map_err(|error| context_fault(error.to_string()))?;
                let actual_bytes = super::super::record::canonical_bytes(&rendered)
                    .map_err(|error| context_fault(error.to_string()))?;
                if prepared_bytes != actual_bytes {
                    return Err(context_fault(
                        "rendered projection changed after selection".into(),
                    ));
                }
                let wire_context = rendered_context(&rendered);
                let wire_tools = tools.iter().map(tool_schema).collect::<Vec<_>>();
                context_candidate.cache_prefix = wire_context
                    .frozen_prefix_len
                    .map(|entries| {
                        let prefix =
                            wire_context.turns.get(..entries as usize).ok_or_else(|| {
                                context_fault("cache prefix exceeds rendered projection".into())
                            })?;
                        let bytes = super::super::record::canonical_bytes(&(
                            &wire_context.system_stable,
                            &wire_context.system_knowledge,
                            prefix,
                        ))
                        .map_err(|error| context_fault(error.to_string()))?;
                        Ok(crate::context::execution::CachePrefixBoundary {
                            digest: crate::evolution::ContentDigest::from_bytes(bytes.as_slice()),
                            entries,
                        })
                    })
                    .transpose()?;
                let projection_bytes =
                    super::super::record::canonical_bytes(&(&wire_context, &wire_tools))
                        .map_err(|error| context_fault(error.to_string()))?;
                context_candidate.rendered_snapshot =
                    crate::evolution::ContentDigest::from_bytes(projection_bytes.as_slice());
                EffectKind::CallProvider(CallProviderEffect {
                    context_candidate: Box::new(context_candidate),
                    context: wire_context,
                    tools: wire_tools,
                })
            }
            LoopAction::ExecuteTools { calls } => EffectKind::ExecuteTools(ExecuteToolsEffect {
                calls: calls.iter().map(wire_tool_call).collect::<Result<_, _>>()?,
            }),
            LoopAction::RequestApproval { requests } => {
                EffectKind::RequestApproval(RequestApprovalEffect {
                    requests: requests
                        .iter()
                        .map(wire_approval_request)
                        .collect::<Result<_, _>>()?,
                })
            }
            LoopAction::SpawnWorkflow { nodes, budget } => {
                let mut tasks = Vec::with_capacity(nodes.len());
                for node in &nodes {
                    tasks.push(self.task_launch(operation_id, step_seq, node)?);
                }
                EffectKind::SpawnTasks(SpawnTasksEffect {
                    tasks,
                    budget: budget.as_ref().map(workflow_budget),
                })
            }
            LoopAction::PreemptSubAgents { agent_ids, reason } => {
                // §10.4 · a preemption names the attempt this kernel issued. A task with no live
                // attempt has nothing to preempt, so it is dropped rather than named with a
                // fabricated one.
                let attempts = agent_ids
                    .iter()
                    .filter_map(|agent_id| {
                        let attempt_id = self.attempts.get(agent_id)?.clone();
                        let task_id = TaskId::new(agent_id).ok()?;
                        Some(TaskAttemptRef {
                            task_id,
                            attempt_id,
                        })
                    })
                    .collect();
                EffectKind::PreemptTasks(PreemptTasksEffect { attempts, reason })
            }
            // Criteria, required evidence, and verifier execution are host-owned (§5.2,
            // adjudication §5m-3/§5p). The host resolves them from `(contract_id, phase_id)`, so
            // the effect carries that pair and no duplicate verifier data.
            LoopAction::EvaluateMilestone {
                phase_id,
                criteria: _,
                required_evidence: _,
                verifier: _,
            } => EffectKind::EvaluateMilestone(EvaluateMilestoneEffect {
                request: super::super::effect::MilestoneRequest {
                    contract_id: self.require_loaded_contract()?,
                    phase_id,
                },
            }),
            LoopAction::ArchivePageOut {
                summary, archived, ..
            } => EffectKind::ArchivePageOut(self.page_out_effect(
                context,
                summary.as_deref(),
                &archived,
                *effect_index,
            )?),
            // Memory effects are minted directly by the P1 syscall path (see
            // `plan_memory_write` / `plan_memory_query`), so reaching these semantic actions is an
            // invalid lifecycle transition.
            action @ (LoopAction::PersistMemory { .. } | LoopAction::QueryMemory { .. }) => {
                return Err(KernelFault::new(
                    KernelFaultCode::InvalidLifecycle,
                    format!(
                        "the semantic kernel emitted unexpected {}; memory effects must be minted \
                         by the P1 syscall that proposed them",
                        loop_action_label(&action)
                    ),
                ));
            }
        };

        let tag = effect.tag();
        self.require_effect_support(context.config, tag)?;
        let effect_id = mint_effect_id(operation_id, step_seq, *effect_index);
        *effect_index += 1;

        match &effect {
            // §7.6 · remember what this turn advertised and whose turn it was. That pair *is* the
            // `ProviderTool` causation a syscall in the result will be attributed to.
            EffectKind::CallProvider(call) => {
                self.provider_calls.insert(
                    effect_id.clone(),
                    PendingProviderCall {
                        task_id: self.turn_task_id(),
                        exposed_tools: call.tools.iter().map(|tool| tool.name.clone()).collect(),
                    },
                );
            }
            // §10.4 · the launch is published, so every task it names leaves `PendingLaunch`. Only
            // the host's acknowledgement moves them on to `Running`.
            EffectKind::SpawnTasks(spawn) => {
                let launched: Vec<String> = spawn
                    .tasks
                    .iter()
                    .map(|task| task.task_id.as_str().to_string())
                    .collect();
                if let Some(engine) = self.engine.as_mut() {
                    engine.mark_tasks_starting(&launched);
                }
            }
            _ => {}
        }

        Ok(StepDisposition::Effects(EffectsDisposition {
            effects: vec![KernelEffect {
                effect_id,
                causation_input_id: causation,
                effect,
            }],
        }))
    }

    /// The task whose turn is currently issuing provider calls. An agent focus names it directly;
    /// a workflow controller's provider call is the parent agent's turn resuming, and before the
    /// first focus is folded there is only the root.
    pub(super) fn turn_task_id(&self) -> TaskId {
        let staged = self
            .staged
            .as_ref()
            .and_then(|staged| staged.focus.as_ref());
        match staged.or(self.focus.as_ref()) {
            Some(ExecutionFocus::AgentTurn(turn)) => turn.task_id.clone(),
            Some(ExecutionFocus::WorkflowController(controller)) => controller
                .parent_task_id
                .clone()
                .unwrap_or_else(root_task_id),
            None => root_task_id(),
        }
    }

    /// Build the workflow terminal from the outcome the engine just published. Reads the
    /// observation rather than re-deriving it: the DAG's own `finish()` is the authority on which
    /// nodes completed.
    pub(super) fn root_workflow_terminal(&mut self) -> Option<KernelTerminal> {
        let workflow_id = self.workflow_id.clone()?;
        let engine = self.engine.as_mut()?;
        let outcomes = engine
            .observations
            .iter()
            .find_map(|observation| match observation {
                KernelObservation::WorkflowCompleted { node_outcomes, .. } => {
                    Some(node_outcomes.clone())
                }
                _ => None,
            })?;
        let mut completed = Vec::new();
        let mut failed = Vec::new();
        for outcome in &outcomes {
            let node_id = self.node_id_for(&outcome.node_id);
            match outcome.status {
                WorkflowNodeStatus::Completed | WorkflowNodeStatus::CompletedPartial => {
                    completed.push(node_id)
                }
                WorkflowNodeStatus::Failed | WorkflowNodeStatus::SkippedUpstreamFailed => {
                    failed.push(node_id)
                }
            }
        }
        let status = if failed.is_empty() {
            WorkflowStatus::Completed
        } else {
            WorkflowStatus::Failed
        };
        Some(KernelTerminal::Workflow(WorkflowTerminal {
            outcome: WorkflowOutcome {
                workflow_id,
                status,
                completed_nodes: completed,
                failed_nodes: failed,
            },
            usage: self.usage_report(),
        }))
    }

    pub(super) fn usage_report(&self) -> UsageReport {
        let Some(engine) = self.engine.as_ref() else {
            return UsageReport::default();
        };
        let (tokens, _, _) = engine.local_budget_usage();
        UsageReport {
            input_tokens: WireU64::new(tokens),
            output_tokens: WireU64::ZERO,
            turns: engine.turn,
            cached_input_tokens: None,
        }
    }

    /// One child launch, with kernel-minted identity. The task/attempt/launch triple exists as a
    /// committed fact before the host is ever asked to start anything (§10.4).
    pub(super) fn task_launch(
        &mut self,
        operation_id: &OperationId,
        step_seq: WireU64,
        info: &crate::orchestration::workflow::WorkflowSpawnInfo,
    ) -> Result<TaskLaunch, KernelFault> {
        self.task_launch_attempt(operation_id, step_seq, info, 1)
    }

    pub(super) fn task_launch_attempt(
        &mut self,
        operation_id: &OperationId,
        step_seq: WireU64,
        info: &crate::orchestration::workflow::WorkflowSpawnInfo,
        attempt: u32,
    ) -> Result<TaskLaunch, KernelFault> {
        let task_id = TaskId::new(&info.agent_id).map_err(malformed)?;
        let attempt_id =
            AttemptId::new(format!("{}:attempt:{attempt}", info.agent_id)).map_err(malformed)?;
        let launch_token = LaunchToken::new(if attempt == 1 {
            format!("{operation_id}:step:{step_seq}:launch:{}", info.agent_id)
        } else {
            format!(
                "{operation_id}:step:{step_seq}:launch:{}:attempt:{attempt}",
                info.agent_id
            )
        })
        .map_err(malformed)?;
        self.attempts
            .insert(info.agent_id.clone(), attempt_id.clone());
        let mut metadata = serde_json::Map::new();
        if let Some(model_hint) = &info.model_hint {
            metadata.insert(
                "model_hint".to_string(),
                serde_json::Value::String(model_hint.clone()),
            );
        }
        if let Some(output_schema) = &info.output_schema {
            metadata.insert("output_schema".to_string(), output_schema.clone());
        }
        if !info.input_agent_ids.is_empty() {
            metadata.insert(
                "input_agent_ids".to_string(),
                serde_json::Value::Array(
                    info.input_agent_ids
                        .iter()
                        .cloned()
                        .map(serde_json::Value::String)
                        .collect(),
                ),
            );
            let dependency_outputs = info
                .input_agent_ids
                .iter()
                .filter_map(|agent_id| {
                    let output = self
                        .engine
                        .as_ref()?
                        .task_table()
                        .get(agent_id)?
                        .proc
                        .as_ref()?
                        .result
                        .as_ref()?
                        .result
                        .final_message
                        .as_ref()
                        .and_then(message_body_parts)?
                        .0;
                    Some((agent_id.clone(), serde_json::Value::String(output)))
                })
                .collect();
            metadata.insert(
                "dependency_outputs".to_string(),
                serde_json::Value::Object(dependency_outputs),
            );
        }
        Ok(TaskLaunch {
            task_id,
            attempt_id,
            launch_token,
            node_id: self.node_id_for(&info.agent_id),
            spec: LogicalAgentSpec {
                goal: info.goal.clone(),
                role: parse_wire_role(&info.role),
                isolation: parse_wire_isolation(&info.isolation),
                context_inheritance: parse_wire_context_inheritance(&info.context_inheritance),
                verification_contract_id: None,
                capability_filter: Default::default(),
                exposure_baseline: None,
                loop_round: None,
                metadata: super::super::scalar::BoundedJson::new(serde_json::Value::Object(
                    metadata,
                ))
                .map_err(malformed)?,
            },
        })
    }

    /// Wire identity of the DAG node an internal agent id belongs to. Falls back to the internal id
    /// when the node came from a runtime append rather than the original spec (Task 10 territory).
    pub(super) fn node_id_for(&self, agent_id: &str) -> NodeId {
        parse_node_index(agent_id)
            .and_then(|index| self.node_ids.get(index).cloned())
            .unwrap_or_else(|| {
                NodeId::new(agent_id).expect("an internal agent id is a legal branded ref")
            })
    }

    /// §7.3 · a spec's `verification_contract_id` must name a contract this operation declared.
    ///
    /// A reference that resolves to nothing is a gate the run believes it has and does not: the
    /// agent would start with no phase cascade, never publish an `EvaluateMilestone`, and finish
    /// having passed a contract that was never evaluated. Refused before the engine moves, so a
    /// rejected start leaves the operation free to start again with a spec that resolves.
    pub(super) fn require_known_contract(
        &self,
        config: &ResolvedOperationConfig,
        spec: Option<&LogicalAgentSpec>,
    ) -> Result<(), KernelFault> {
        let Some(contract_id) = spec.and_then(|spec| spec.verification_contract_id.as_deref())
        else {
            return Ok(());
        };
        if config.verification_contract(contract_id).is_some() {
            return Ok(());
        }
        Err(KernelFault::new(
            KernelFaultCode::InvalidConfig,
            format!(
                "the run spec names verification contract {contract_id:?}, which this operation's \
                 catalog does not declare; a contract reference that resolves to nothing is a \
                 milestone gate the run believes it has (§7.3)"
            ),
        ))
    }

    /// Install the phase cascade an agent's contract reference selects.
    ///
    /// This is `EvaluateMilestone`'s canonical producer (Task 12 SPEC-ISSUE-4): without it the
    /// effect existed in the union, had a resolution path and a failure path, and nothing on the
    /// wire could ever cause the kernel to emit one.
    pub(super) fn load_verification_contract(
        &mut self,
        config: &ResolvedOperationConfig,
        spec: Option<&LogicalAgentSpec>,
    ) -> Result<(), KernelFault> {
        let Some(contract) = spec
            .and_then(|spec| spec.verification_contract_id.as_deref())
            .and_then(|id| config.verification_contract(id))
        else {
            return Ok(());
        };
        // A contract means the operation *will* ask for a verdict, so the effect it will publish
        // has to be declared now rather than faulting mid-cascade (DEC-8).
        self.require_effect_support(config, EffectKindTag::EvaluateMilestone)?;
        let contract_id = contract.contract_id.clone();
        let cascade = core_milestone_contract(contract, config);
        self.engine_mut()?.load_milestone_contract(cascade);
        // Remembered so every `EvaluateMilestone` the cascade produces can name the contract its
        // phase belongs to — the half of the host's lookup key the semantic engine does not carry.
        self.loaded_contract_id = Some(contract_id);
        Ok(())
    }

    /// The contract id every `EvaluateMilestone` this operation publishes belongs to.
    ///
    /// Fails closed rather than sending an empty id: a cascade can only be running because
    /// `load_verification_contract` installed one, so a milestone request with no contract behind
    /// it means the engine produced a phase the canonical wire never declared — and a request the
    /// host cannot resolve to a verifier is worse than no request at all.
    pub(super) fn require_loaded_contract(&self) -> Result<String, KernelFault> {
        self.loaded_contract_id.clone().ok_or_else(|| {
            KernelFault::new(
                KernelFaultCode::InvalidLifecycle,
                "the semantic kernel asked for a milestone verdict, but this operation installed \
                 no verification contract; a milestone request names the (contract_id, phase_id) \
                 pair the host looks its verifier up by (§7.8)"
                    .to_string(),
            )
        })
    }

    pub(super) fn require_effect_support(
        &self,
        config: &ResolvedOperationConfig,
        tag: EffectKindTag,
    ) -> Result<(), KernelFault> {
        if config.host_effect_support.supports(tag) {
            return Ok(());
        }
        Err(KernelFault::new(
            KernelFaultCode::UnsupportedEffect,
            format!(
                "this operation's host does not declare support for {tag} effects, so the \
                 transition that would publish one is refused before anything moves"
            ),
        ))
    }

    pub(super) fn require_root_kind(&self) -> Result<RootKind, KernelFault> {
        self.root_kind.ok_or_else(|| {
            KernelFault::new(
                KernelFaultCode::InvalidLifecycle,
                "no root has started, so there is nothing to advance".to_string(),
            )
        })
    }

    pub(super) fn engine_mut(&mut self) -> Result<&mut LoopStateMachine, KernelFault> {
        self.engine.as_mut().ok_or_else(|| {
            KernelFault::new(
                KernelFaultCode::InvalidLifecycle,
                "the operation has no genesis configuration, so it has no semantic kernel to drive"
                    .to_string(),
            )
        })
    }

    pub(super) fn poison_with(&mut self, fault: KernelFault) -> KernelFault {
        self.staged = None;
        self.poison.get_or_insert(fault).clone()
    }
}
