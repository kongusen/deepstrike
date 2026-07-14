use super::*;
/// Pure kernel runtime wrapper. SDKs should migrate toward feeding
/// `KernelInput` values here instead of directly driving `LoopStateMachine`.
pub struct KernelRuntime {
    sm: LoopStateMachine,
}

impl KernelRuntime {
    pub fn new(policy: SchedulerBudget) -> Self {
        Self {
            sm: LoopStateMachine::new(policy),
        }
    }

    pub fn state_machine(&self) -> &LoopStateMachine {
        &self.sm
    }

    pub fn state_machine_mut(&mut self) -> &mut LoopStateMachine {
        &mut self.sm
    }

    pub fn is_terminal(&self) -> bool {
        self.sm.is_terminal()
    }

    /// L1 (RunGroup): this vehicle's cumulative sub-agent spawns this run, read back by the SDK at run
    /// end to charge the group ledger (so the next member's cumulative spawn cap is seeded correctly).
    pub fn local_subagents_spawned(&self) -> u32 {
        self.sm.local_subagents_spawned()
    }

    pub fn step(&mut self, input: KernelInput) -> KernelStep {
        // The ABI version stamped by `KernelInput::new` is checked, not ceremonial: a payload
        // built against a different kernel ABI is rejected instead of being silently
        // reinterpreted under this version's semantics.
        if input.version != KERNEL_ABI_VERSION {
            self.sm.observations.push(KernelObservation::ToolGated {
                turn: self.sm.turn,
                call_id: String::new(),
                tool: "kernel_abi".to_string(),
                reason: format!(
                    "kernel ABI version mismatch: input v{}, kernel v{}",
                    input.version, KERNEL_ABI_VERSION
                ),
            });
            return KernelStep::empty(self.sm.take_observations());
        }
        let action = match input.event {
            KernelInputEvent::SetTools { tools } => {
                self.sm.tools = tools;
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::SetAvailableSkills { skills } => {
                self.sm.ctx.set_available_skills(skills);
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::SkillActivated { name, lease_turns } => {
                // B1: record the activation (B2 reads it in emit_call_llm to narrow tools).
                // The returned `changed` flag is the epoch boundary for D's cache re-anchor.
                // K3: a lease converts to an absolute expiry turn here (the manager is turn-blind).
                let expires_at_turn = lease_turns.map(|n| self.sm.turn.saturating_add(n));
                self.sm.ctx.activate_skill_leased(name, expires_at_turn);
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::SkillDeactivated { name } => {
                self.sm.ctx.deactivate_skill(&name);
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::SetStableCoreTools { tool_ids } => {
                self.sm
                    .ctx
                    .set_stable_core_tools(tool_ids.into_iter().map(Into::into));
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::SetMemoryEnabled { enabled } => {
                self.sm.ctx.set_memory_enabled(enabled);
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::SetKnowledgeEnabled { enabled } => {
                self.sm.ctx.set_knowledge_enabled(enabled);
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::SetPlanToolEnabled { enabled } => {
                self.sm.ctx.set_plan_tool_enabled(enabled);
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::SetTokenizer { .. } => {
                // Local BPE tokenisers are no longer used — accuracy comes from
                // observed_input_tokens reported by the provider API (P0-1 Step 2).
                // char_approx is always used for pre-flight truncation estimates.
                self.sm.ctx.engine = ContextTokenEngine::char_approx();
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::AddSystemMessage { content, tokens } => {
                self.sm
                    .ctx
                    .partitions
                    .system
                    .push(Message::system(content), tokens.max(1));
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::AddKnowledgeMessage {
                content,
                tokens,
                key,
                pinned,
            } => {
                // P1-B2 cache contract: the knowledge partition renders into the cached system[1]
                // block. Appending here is the right home for *stable* reference material (skill
                // defs, durable artifacts) — it's append-only, so the existing prefix stays
                // byte-stable, and a fresh append costs only a one-time system[1] re-cache. Do NOT
                // route *per-turn* retrievals (a memory/knowledge lookup that changes every turn)
                // through here: each would rewrite the cached block and invalidate it plus the
                // history cache every turn. Volatile per-turn context belongs on the signal/tail
                // path (`push_signal` → state_turn), which is uncached *and* high-attention (P1-F).
                //
                // K1: a `key` gives the entry identity — a same-key push stages a boundary-deferred
                // upsert instead of appending a duplicate (the cache contract above is why the swap
                // waits for the next compaction/renewal boundary).
                self.sm.ctx.push_knowledge_entry(
                    key.map(compact_str::CompactString::from),
                    Message::system(content),
                    tokens.max(1),
                    pinned,
                );
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::RemoveKnowledge { key } => {
                self.sm.ctx.remove_knowledge(&key);
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::AddHistoryMessage { message, tokens } => {
                let tokens = tokens.unwrap_or_else(|| self.sm.ctx.engine.count_message(&message));
                self.sm.ctx.push_history(message, tokens.max(1));
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::PreloadHistory { messages } => {
                self.sm.preload_history(messages);
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::MountCapability { capability } => {
                self.sm.mount_capability(capability, None, None);
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::UnmountCapability {
                capability_kind,
                id,
            } => {
                self.sm.unmount_capability(capability_kind, &id);
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::LoadMilestoneContract { contract } => {
                self.sm.load_milestone_contract(contract);
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::LoadGovernancePolicy {
                default_action,
                rules,
                vetoed_tools,
                rate_limits,
                constraints,
            } => {
                self.sm.set_governance(build_governance_pipeline(
                    default_action,
                    rules,
                    vetoed_tools,
                    rate_limits,
                    constraints,
                ));
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::ConfigureRun { config } => {
                // K2: apply a bundle of run-setup config in one event (tools / governance / attention /
                // quota / scheduler / toggles), replacing the ~10 separate `Set*` / `Load*` events the
                // SDK used to fire one-by-one. Each field is optional; an absent field is left untouched.
                // The individual events remain for runtime mutation (skill mount, mid-run budget change).
                // Each branch delegates to exactly the method its granular event uses, so the two paths
                // can never diverge.
                let RunConfig {
                    tools,
                    available_skills,
                    stable_core_tools,
                    memory_enabled,
                    knowledge_enabled,
                    plan_tool_enabled,
                    tokenizer,
                    governance,
                    attention_max_queue_size,
                    scheduler_max_wall_ms,
                    resource_quota,
                    group_tokens_base,
                    group_spawns_base,
                    group_rounds_base,
                    repeat_fuse,
                    criteria_gate,
                    knowledge_budget_ratio,
                    entropy_watch,
                } = config;
                if let Some(tools) = tools {
                    self.sm.tools = tools;
                }
                if let Some(skills) = available_skills {
                    self.sm.ctx.set_available_skills(skills);
                }
                if let Some(ids) = stable_core_tools {
                    self.sm
                        .ctx
                        .set_stable_core_tools(ids.into_iter().map(Into::into));
                }
                if let Some(enabled) = memory_enabled {
                    self.sm.ctx.set_memory_enabled(enabled);
                }
                if let Some(enabled) = knowledge_enabled {
                    self.sm.ctx.set_knowledge_enabled(enabled);
                }
                if let Some(enabled) = plan_tool_enabled {
                    self.sm.ctx.set_plan_tool_enabled(enabled);
                }
                if tokenizer.is_some() {
                    self.sm.ctx.engine = ContextTokenEngine::char_approx();
                }
                if let Some(g) = governance {
                    self.sm.set_governance(build_governance_pipeline(
                        g.default_action,
                        g.rules,
                        g.vetoed_tools,
                        g.rate_limits,
                        g.constraints,
                    ));
                }
                if let Some(max_queue) = attention_max_queue_size {
                    self.sm.set_attention(max_queue as usize);
                }
                if let Some(ms) = scheduler_max_wall_ms {
                    self.sm.set_wall_budget(Some(ms));
                }
                if let Some(quota) = resource_quota {
                    self.sm.set_resource_quota(quota);
                }
                if let Some(base) = group_tokens_base {
                    self.sm.seed_group_budget(base);
                }
                if let Some(base) = group_spawns_base {
                    self.sm.seed_group_spawns(base);
                }
                if let Some(base) = group_rounds_base {
                    self.sm.seed_group_rounds(base);
                }
                if let Some(fuse) = repeat_fuse {
                    self.sm.set_repeat_fuse(fuse);
                }
                if let Some(enabled) = criteria_gate {
                    self.sm.set_criteria_gate(enabled);
                }
                if let Some(ratio) = knowledge_budget_ratio {
                    self.sm.ctx.config.knowledge_budget_ratio = ratio;
                }
                if let Some(watch) = entropy_watch {
                    self.sm.set_entropy_watch(watch);
                }
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::SetAttentionPolicy { max_queue_size } => {
                self.sm.set_attention(max_queue_size as usize);
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::PageIn { entries } => {
                self.sm.apply_page_in(&entries);
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::ForceCompact => {
                self.sm.force_compact();
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::UpdateTask { update } => {
                self.sm.ctx.update_task(update);
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::StartRun { task, run_spec } => {
                self.sm.run_spec = run_spec;
                self.sm.start(task)
            }
            KernelInputEvent::CapabilityCommand { command } => {
                self.sm.execute_capability_command(command);
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::Resume {
                approved_calls,
                denied_calls,
            } => self.sm.resume_from_suspend(approved_calls, denied_calls),
            KernelInputEvent::SetSchedulerBudget { max_wall_ms } => {
                self.sm.set_wall_budget(max_wall_ms);
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::SetResourceQuota { quota } => {
                self.sm.set_resource_quota(quota);
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::SetCriteriaGate { enabled } => {
                self.sm.set_criteria_gate(enabled);
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::SetKnowledgeBudget { ratio } => {
                self.sm.ctx.config.knowledge_budget_ratio = ratio;
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::SetRepeatFuse {
                enabled,
                deny_after,
                terminate_after,
            } => {
                let mut cfg = self.sm.repeat_fuse_config();
                if let Some(e) = enabled {
                    cfg.enabled = e;
                }
                if let Some(d) = deny_after {
                    cfg.deny_after = d;
                }
                if let Some(t) = terminate_after {
                    cfg.terminate_after = t;
                }
                self.sm.set_repeat_fuse(cfg);
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::SetEntropyWatch {
                enabled,
                threshold,
                hysteresis,
                cooldown_turns,
                notify_model,
            } => {
                let mut cfg = self.sm.entropy_watch_config();
                if let Some(e) = enabled {
                    cfg.enabled = e;
                }
                if let Some(t) = threshold {
                    cfg.threshold = t;
                }
                if let Some(h) = hysteresis {
                    cfg.hysteresis = h;
                }
                if let Some(c) = cooldown_turns {
                    cfg.cooldown_turns = c;
                }
                if let Some(n) = notify_model {
                    cfg.notify_model = n;
                }
                self.sm.set_entropy_watch(cfg);
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::ProviderResult {
                message,
                observed_input_tokens,
                observed_output_tokens: _,
                now_ms,
                stop_reason,
            } => {
                if let Some(tokens) = observed_input_tokens {
                    self.sm.ctx.set_observed_prompt_tokens(tokens);
                }
                // Feed the clock before the governance gate fires inside `feed`, so the
                // rate limiter sees a real timestamp (no-op when no policy is loaded).
                if let Some(ms) = now_ms {
                    self.sm.set_observed_time(ms);
                }
                // Stash stop_reason so `feed` can detect an output-cap truncation and drive recovery.
                self.sm.set_pending_stop_reason(stop_reason);
                self.sm.feed(LoopEvent::LLMResponse { message })
            }
            KernelInputEvent::ToolResults { results } => {
                self.sm.feed(LoopEvent::ToolResults { results })
            }
            KernelInputEvent::ProviderError { message } => {
                // Reactive recovery is a kernel decision: classify + bounded compact-and-retry,
                // returning the next action (retry or honest terminal) through the common tail.
                self.sm.recover_from_provider_error(&message)
            }
            KernelInputEvent::Signal { signal } => match self.sm.signal_event(signal) {
                Some(action) => action,
                // Non-actionable disposition (queued / observed / ignored / dropped):
                // no provider call this step, just the SignalDisposed observation.
                None => return KernelStep::empty(self.sm.take_observations()),
            },
            KernelInputEvent::MilestoneResult { result } => {
                self.sm.feed(LoopEvent::MilestoneResult { result })
            }
            KernelInputEvent::SpawnSubAgent {
                spec,
                parent_session_id,
            } => self.sm.spawn_sub_agent(spec, &parent_session_id),
            KernelInputEvent::LoadWorkflow {
                spec,
                parent_session_id,
                resumed_completed,
                resumed_submissions,
                resumed_submission_bases,
                resumed_results,
            } => {
                // K1: self-bootstrap the run if the host never fired `StartRun` (stateless
                // `runWorkflow` caller). Parity with the agent-reachable `SubmitWorkflow`, which already
                // bootstraps. Idempotent no-op once the root task has left `Ready`.
                self.sm.ensure_started_for_workflow(&spec);
                if resumed_completed.is_empty()
                    && resumed_results.is_empty()
                    && resumed_submissions.is_empty()
                {
                    self.sm.load_workflow(spec, &parent_session_id)
                } else {
                    // W-1: merge legacy bare ids with signal-carrying records (records win).
                    use crate::orchestration::workflow::ResumedCompletion;
                    let mut completed: Vec<ResumedCompletion> = resumed_completed
                        .iter()
                        .filter(|id| resumed_results.iter().all(|r| &r.agent_id != *id))
                        .map(ResumedCompletion::bare)
                        .collect();
                    completed.extend(resumed_results);
                    self.sm.load_workflow_resumed(
                        spec,
                        &parent_session_id,
                        &resumed_submissions,
                        &resumed_submission_bases,
                        &completed,
                    )
                }
            }
            KernelInputEvent::SubAgentCompleted { result } => {
                self.sm.feed(LoopEvent::SubAgentCompleted { result })
            }
            KernelInputEvent::SubmitWorkflowNodes {
                nodes,
                submitter_agent_id,
            } => self
                .sm
                .submit_workflow_nodes(nodes, submitter_agent_id.as_deref()),
            KernelInputEvent::SubmitWorkflow {
                spec,
                parent_session_id,
                submitter_agent_id,
            } => self
                .sm
                .submit_workflow(spec, &parent_session_id, submitter_agent_id.as_deref()),
            KernelInputEvent::SetMemoryPolicy {
                memory_path,
                stale_warning_days,
                retrieval_top_k,
                validation_enabled,
                max_content_bytes,
                max_name_length,
            } => {
                // Phase 7: install the memory policy. The kernel enforces validation_enabled +
                // retrieval_top_k + size/name overrides at the WriteMemory/QueryMemory traps;
                // memory_path / stale_warning_days are carried for the SDK's recall I/O.
                self.sm.set_memory_policy(crate::mm::memory::MemoryPolicy {
                    memory_path,
                    stale_warning_days,
                    retrieval_top_k,
                    validation_enabled,
                    max_content_bytes,
                    max_name_length,
                });
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::WriteMemory { memory } => {
                // Phase 7: Validate memory write request.
                // Kernel validates; SDK performs I/O.
                use crate::mm::memory::validate_memory_write;
                let turn = self.sm.turn;
                // M2: route the write through the syscall trap so the resource quota (write-rate
                // limit) applies. A rate-limited / denied write surfaces as a validation failure
                // (the write does not happen) and short-circuits before validation.
                let disposition = self
                    .sm
                    .gate_syscall(&crate::syscall::Syscall::WriteMemory(memory.clone()));
                if !disposition.is_allowed() {
                    let error = match disposition {
                        crate::syscall::Disposition::RateLimited { retry_after_ms } => {
                            format!("memory write rate limited; retry after {retry_after_ms}ms")
                        }
                        crate::syscall::Disposition::Deny { reason, .. } => {
                            format!("memory write denied: {reason}")
                        }
                        _ => "memory write not permitted".to_string(),
                    };
                    self.sm
                        .observations
                        .push(KernelObservation::MemoryValidationFailed {
                            turn,
                            memory_id: memory.metadata.name.clone(),
                            error,
                        });
                    return KernelStep::empty(self.sm.take_observations());
                }
                // Validate honoring any installed memory policy: a policy with validation disabled
                // admits the write outright; a policy with size/name overrides validates against
                // those; no policy uses the default rules (pre-policy behavior).
                let validation_result = match self.sm.memory_policy() {
                    Some(p) if !p.validation_enabled => Ok(()),
                    Some(p) => p.validation().validate(&memory),
                    None => validate_memory_write(&memory),
                };
                match validation_result {
                    Ok(()) => {
                        // Emit observation for SDK to perform I/O
                        self.sm.observations.push(KernelObservation::MemoryWritten {
                            turn,
                            memory_id: memory.metadata.name.clone(),
                            // Kind is an optional caller-supplied label; the kernel does not
                            // guess taxonomy from metadata (P13: heuristic classifier deleted).
                            memory_kind: memory
                                .metadata
                                .kind
                                .map(|k| k.label())
                                .unwrap_or("unclassified")
                                .to_string(),
                            size_bytes: memory.content.len() as u32,
                        });
                    }
                    Err(err) => {
                        // Emit validation error observation
                        use crate::mm::memory::MemoryValidationError;
                        let error_msg = match err {
                            MemoryValidationError::MissingRequiredField { field } => {
                                format!("Missing required field: {}", field)
                            }
                            MemoryValidationError::ContentTooLarge { size, limit } => {
                                format!("Content too large: {} bytes (limit: {})", size, limit)
                            }
                            MemoryValidationError::ForbiddenPattern { pattern, reason } => {
                                format!("Forbidden pattern '{}': {}", pattern, reason)
                            }
                            MemoryValidationError::InvalidKind { kind } => {
                                format!("Invalid kind: {}", kind)
                            }
                            MemoryValidationError::NameTooLong { length, limit } => {
                                format!("Name too long: {} chars (limit: {})", length, limit)
                            }
                        };
                        self.sm
                            .observations
                            .push(KernelObservation::MemoryValidationFailed {
                                turn,
                                memory_id: memory.metadata.name.clone(),
                                error: error_msg,
                            });
                    }
                }
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::QueryMemory { query } => {
                // Phase 7: Query memory for context.
                // Kernel emits observation; SDK responds asynchronously.
                let turn = self.sm.turn;
                // An installed policy caps retrieval breadth: requested_k = min(query.top_k, policy).
                let requested_k = match self.sm.memory_policy() {
                    Some(p) => p.clamp_top_k(query.top_k),
                    None => query.top_k,
                };
                self.sm.observations.push(KernelObservation::MemoryQueried {
                    turn,
                    query_context: query.current_context.clone(),
                    requested_k,
                    requires_async_response: true,
                });
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::Timeout => self.sm.feed(LoopEvent::Timeout),
        };
        if matches!(action, LoopAction::AwaitingResume) {
            return KernelStep::empty(self.sm.take_observations());
        }
        KernelStep::single(action, self.sm.take_observations())
    }
}
