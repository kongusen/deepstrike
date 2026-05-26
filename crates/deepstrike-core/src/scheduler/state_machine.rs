use super::policy::LoopPolicy;
use crate::context::manager::ContextManager;
use crate::context::pressure::PressureAction;
use crate::context::renderer::RenderedContext;
use crate::types::message::{Content, ContentPart, Message, ToolCall, ToolResult, ToolSchema};
use crate::types::milestone::{MilestoneCheckResult, MilestoneContract};
use crate::types::result::{LoopResult, TerminationReason};
use crate::types::signal::{RuntimeSignal, Urgency};
use crate::types::task::RuntimeTask;

/// The phases of the L* execution loop.
#[derive(Debug, Clone)]
pub enum LoopPhase {
    Idle,
    Reason,
    Act { tool_calls: Vec<ToolCall> },
    Observe { results: Vec<ToolResult> },
    Delta { pressure: f64 },
    Terminal { result: LoopResult },
}

/// Events fed into the state machine from the SDK layer.
#[derive(Debug)]
pub enum LoopEvent {
    Start {
        task: RuntimeTask,
    },
    LLMResponse {
        message: Message,
    },
    ToolResults {
        results: Vec<ToolResult>,
    },
    /// Inbound signal from SignalRouter — Critical/High urgency may interrupt.
    Signal {
        signal: RuntimeSignal,
    },
    /// Result of evaluating the current milestone phase's criteria.
    /// Feed this back after handling `LoopAction::EvaluateMilestone`.
    MilestoneResult {
        result: MilestoneCheckResult,
    },
    Timeout,
}

/// Actions the state machine outputs — SDK layer executes the I/O.
#[derive(Debug)]
pub enum LoopAction {
    /// Structured context ready for a provider call.
    /// `context.system_text` → provider system param.
    /// `context.turns`       → provider messages array (strictly alternating).
    /// `tools`               → tool schemas (skill / memory / knowledge / user tools).
    CallLLM {
        context: RenderedContext,
        tools: Vec<ToolSchema>,
    },
    ExecuteTools {
        calls: Vec<ToolCall>,
    },
    Done {
        result: LoopResult,
    },
    /// Kernel requests the SDK to evaluate the current milestone phase.
    ///
    /// The SDK should assess `criteria` against the agent's output (via an
    /// external verifier or a machine check), then feed back
    /// `LoopEvent::MilestoneResult { result }`.
    EvaluateMilestone {
        phase_id: String,
        criteria: Vec<String>,
    },
}

/// One-shot observation emitted by the kernel during `feed`.
/// SDK drains this between calls for telemetry/UI updates.
#[derive(Debug, Clone)]
pub enum LoopObservation {
    Compressed {
        action: PressureAction,
        rho_after: f64,
        summary: Option<String>,
        archived: Vec<Message>,
    },
    /// Context renewal fired — a new sprint started to carry the conversation forward.
    Renewed { sprint: u32 },
    /// Rollback event indicating a turn execution failure led to restoring state
    Rollbacked {
        turn: u32,
        checkpoint_history_len: u32,
    },
    /// Capabilities dynamically updated
    CapabilityChanged {
        turn: u32,
        added: Vec<String>,
        removed: Vec<String>,
    },
    /// Milestone phase satisfied — capabilities unlocked, phase advanced.
    MilestoneAdvanced {
        turn: u32,
        phase_id: String,
        capabilities_unlocked: Vec<String>,
    },
    /// Milestone assertion failed — loop continues without phase advancement.
    MilestoneBlocked {
        turn: u32,
        phase_id: String,
        reason: String,
    },
}

/// Pure state machine for the L* execution loop. No I/O — only state transitions.
pub struct LoopStateMachine {
    pub phase: LoopPhase,
    pub turn: u32,
    pub ctx: ContextManager,
    pub tools: Vec<ToolSchema>,
    pub observations: Vec<LoopObservation>,
    policy: LoopPolicy,
    total_tokens: u64,
    /// When set, the next LLM call strips tools to force a text response,
    /// then terminates with this reason once the response arrives.
    pending_termination: Option<TerminationReason>,
    /// Number of history messages present at session start (after preload_history).
    /// drain_new_messages() returns the slice from this offset onward.
    session_history_baseline: usize,
    checkpoint_history_len: usize,
    checkpoint_working_len: usize,
    checkpoint_task_state: Option<crate::context::task_state::TaskState>,
    /// Optional milestone contract loaded before the run starts.
    milestone_contract: Option<MilestoneContract>,
    /// Index of the current (not-yet-passed) phase within `milestone_contract`.
    current_milestone_phase: usize,
}

impl LoopStateMachine {
    fn message_tokens(&self, message: &Message) -> u32 {
        message
            .token_count
            .unwrap_or_else(|| self.ctx.engine.count_message(message))
    }

    pub fn new(policy: LoopPolicy) -> Self {
        Self {
            phase: LoopPhase::Idle,
            turn: 0,
            ctx: ContextManager::new(policy.max_tokens),
            tools: Vec::new(),
            observations: Vec::new(),
            policy,
            total_tokens: 0,
            pending_termination: None,
            session_history_baseline: 0,
            checkpoint_history_len: 0,
            checkpoint_working_len: 0,
            checkpoint_task_state: None,
            milestone_contract: None,
            current_milestone_phase: 0,
        }
    }

    /// 强行进行一次最大力度的压缩归档。通常用于收到模型 API 413 (Prompt too long) 时做兜底重试。
    pub fn force_compact(&mut self) -> bool {
        let action = PressureAction::AutoCompact;
        let (saved, summary, archived) = self.ctx.force_compress();
        if saved > 0 {
            self.observations.push(LoopObservation::Compressed {
                action,
                rho_after: self.ctx.rho(),
                summary,
                archived,
            });
            true
        } else {
            false
        }
    }

    /// Pre-populate the history partition with messages from a prior session.
    ///
    /// Call **before** `start()` when resuming a conversation. Sets the baseline
    /// so `drain_new_messages()` returns only the messages from the current run.
    pub fn preload_history(&mut self, messages: Vec<Message>) {
        for msg in messages {
            let tokens = self.message_tokens(&msg);
            self.ctx.push_history(msg, tokens);
        }
        self.session_history_baseline = self.ctx.partitions.history.messages.len();
    }

    /// Continue from preloaded history without appending a new user turn.
    /// Use after `preload_history` when recovering a session that ended mid-run.
    ///
    /// If the last assistant turn has tool calls without matching tool results,
    /// resumes with `ExecuteTools` instead of calling the LLM again.
    pub fn resume_after_preload(&mut self) -> LoopAction {
        self.observations.clear();
        let calls = crate::runtime::repair::pending_tool_calls_from_messages(
            &self.ctx.partitions.history.messages,
        );
        if !calls.is_empty() {
            self.phase = LoopPhase::Act {
                tool_calls: calls.clone(),
            };
            return LoopAction::ExecuteTools { calls };
        }
        self.phase = LoopPhase::Reason;
        self.emit_call_llm()
    }

    /// Return all messages added to history during the current run
    /// (since the last `preload_history` call or since construction).
    ///
    /// Call after `LoopAction::Done` to get the complete turn transcript
    /// for persistence to a SessionStore.
    pub fn drain_new_messages(&self) -> Vec<Message> {
        let history = &self.ctx.partitions.history.messages;
        let start = self.session_history_baseline.min(history.len());
        history[start..].to_vec()
    }

    pub fn start(&mut self, task: RuntimeTask) -> LoopAction {
        self.observations.clear();
        self.ctx.init_task(task.goal.clone(), task.criteria.clone());

        let user_msg = if task.criteria.is_empty() {
            task.goal
        } else {
            let criteria_text = task
                .criteria
                .iter()
                .enumerate()
                .map(|(i, c)| format!("{}. {}", i + 1, c))
                .collect::<Vec<_>>()
                .join("\n");
            format!("{}\n\nCriteria:\n{}", task.goal, criteria_text)
        };

        // User message goes into history so it appears at the correct chronological
        // position: [prior turns...] → [current user message] — LLM reads left-to-right
        // and responds to the last message. working is reserved for runtime signals only.
        // Estimate tokens (1 token ≈ 4 chars) with a minimum of 1 so the renderer
        // does not skip this message (it skips zero-token entries).
        let user_tokens = self.ctx.engine.count(&user_msg).max(1);
        self.ctx.push_history(Message::user(user_msg), user_tokens);
        self.phase = LoopPhase::Reason;
        self.emit_call_llm()
    }

    pub fn feed(&mut self, event: LoopEvent) -> LoopAction {
        self.observations.clear();
        match event {
            LoopEvent::Start { task } => self.start(task),

            LoopEvent::LLMResponse { message } => {
                let tokens = self.message_tokens(&message);
                self.total_tokens += tokens as u64;

                if let Some(reason) = self.pending_termination.take() {
                    return self.terminate(reason, Some(message));
                }

                if message.tool_calls.is_empty() {
                    // When a milestone contract is active and not yet complete,
                    // request evaluation instead of terminating.
                    if !self.is_milestone_complete() {
                        let phase_id = self
                            .current_milestone_phase_id()
                            .unwrap_or("")
                            .to_string();
                        let criteria = self.current_milestone_criteria().to_vec();
                        let tokens = self.message_tokens(&message);
                        self.ctx.push_history(message, tokens);
                        return LoopAction::EvaluateMilestone { phase_id, criteria };
                    }
                    return self.terminate(TerminationReason::Completed, Some(message));
                }

                let calls = message.tool_calls.clone();
                self.ctx.push_history(message, tokens);
                self.phase = LoopPhase::Act {
                    tool_calls: calls.clone(),
                };
                LoopAction::ExecuteTools { calls }
            }

            LoopEvent::ToolResults { results } => {
                let has_fatal = results.iter().any(|r| r.is_fatal);
                if has_fatal {
                    // A fatal (transactional) error mutated shared state; roll
                    // back the turn so the LLM retries from a clean checkpoint.
                    let errors: Vec<String> = results
                        .iter()
                        .filter(|r| r.is_fatal)
                        .map(|r| match &r.output {
                            Content::Text(s) => s.clone(),
                            Content::Parts(parts) => {
                                serde_json::to_string(parts).unwrap_or_default()
                            }
                        })
                        .collect();
                    let err_msg = format!(
                        "[SYSTEM] A fatal tool error occurred; state was rolled back. Errors: {}",
                        errors.join("; ")
                    );
                    self.rollback();

                    let note = Message::user(err_msg);
                    self.ctx.partitions.working.push(note, 0);
                    self.phase = LoopPhase::Reason;
                    return self.emit_call_llm();
                }
                // Non-fatal errors are committed to history so the LLM can
                // see them and self-correct without losing turn state.

                for r in &results {
                    self.total_tokens += r.token_count.unwrap_or(0) as u64;
                    // Preserve Content::Parts (structured / multimodal tool output).
                    // Parts are serialised to JSON so the text can be restored faithfully.
                    let output = match &r.output {
                        Content::Text(s) => s.clone(),
                        Content::Parts(parts) => serde_json::to_string(parts).unwrap_or_default(),
                    };
                    let parts = vec![ContentPart::ToolResult {
                        call_id: r.call_id.clone(),
                        output,
                        is_error: r.is_error,
                    }];
                    let tool_msg = Message::tool(parts);
                    let tokens = r
                        .token_count
                        .unwrap_or_else(|| self.ctx.engine.count_message(&tool_msg));
                    self.ctx.push_history(tool_msg, tokens);
                }
                self.turn += 1;

                if let Some(reason) = self.policy.should_terminate(self.turn, self.total_tokens) {
                    let term = if reason == "max_turns" {
                        TerminationReason::MaxTurns
                    } else {
                        TerminationReason::TokenBudget
                    };
                    self.pending_termination = Some(term);
                    self.phase = LoopPhase::Reason;
                    return self.emit_call_llm();
                }

                let action = self.ctx.should_compress();
                self.phase = LoopPhase::Delta {
                    pressure: self.ctx.rho(),
                };
                if action != PressureAction::None {
                    let (_, summary, archived) = self.ctx.compress(action);
                    self.observations.push(LoopObservation::Compressed {
                        action,
                        rho_after: self.ctx.rho(),
                        summary,
                        archived,
                    });
                }

                // Renewal: when compression alone cannot recover enough headroom,
                // start a new sprint — carry forward system + memory + last N history turns.
                if self.ctx.should_renew() {
                    self.ctx.renew();
                    self.observations.push(LoopObservation::Renewed {
                        sprint: self.ctx.sprint,
                    });
                }

                self.phase = LoopPhase::Reason;
                self.emit_call_llm()
            }

            LoopEvent::Signal { signal } => {
                // Signals go into working (not history) — they are runtime events,
                // not part of the conversation transcript.
                match signal.urgency {
                    Urgency::Critical => {
                        let note = Message::user(format!("[INTERRUPT] {}", signal.summary));
                        self.ctx.partitions.working.push(note, 0);
                        self.phase = LoopPhase::Reason;
                        self.emit_call_llm()
                    }
                    Urgency::High => {
                        let note = Message::user(format!("[SIGNAL] {}", signal.summary));
                        self.ctx.partitions.working.push(note, 0);
                        self.emit_call_llm()
                    }
                    _ => self.emit_call_llm(),
                }
            }

            LoopEvent::MilestoneResult { result } => self.handle_milestone_result(result),

            LoopEvent::Timeout => self.terminate(TerminationReason::Timeout, None),
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self.phase, LoopPhase::Terminal { .. })
    }

    /// Drain observations emitted during the last `start`/`feed` call.
    pub fn take_observations(&mut self) -> Vec<LoopObservation> {
        std::mem::take(&mut self.observations)
    }

    fn terminate(
        &mut self,
        termination: TerminationReason,
        final_message: Option<Message>,
    ) -> LoopAction {
        // Commit the final response into history so subsequent session restores
        // include the complete transcript: user → [tool turns] → final assistant.
        if let Some(ref msg) = final_message {
            let tokens = self.message_tokens(msg);
            self.ctx.push_history(msg.clone(), tokens);
        }
        let result = LoopResult {
            termination,
            final_message,
            turns_used: self.turn,
            total_tokens_used: self.total_tokens,
        };
        self.phase = LoopPhase::Terminal {
            result: result.clone(),
        };
        LoopAction::Done { result }
    }

    /// Build the `CallLLM` action with a structured `RenderedContext`.
    /// Meta-tools (skill / memory / knowledge) are appended to the tool list
    /// when configured. When `pending_termination` is set, tools are stripped
    /// to force a plain-text response before the loop terminates.
    fn emit_call_llm(&mut self) -> LoopAction {
        self.checkpoint_history_len = self.ctx.partitions.history.messages.len();
        self.checkpoint_working_len = self.ctx.partitions.working.messages.len();
        self.checkpoint_task_state = Some(self.ctx.partitions.task_state.clone());

        let context = self.ctx.render();
        if self.pending_termination.is_some() {
            return LoopAction::CallLLM {
                context,
                tools: Vec::new(),
            };
        }
        let mut tools = self.tools.clone();
        tools.extend(self.ctx.meta_tool_schemas());
        LoopAction::CallLLM { context, tools }
    }

    pub fn rollback(&mut self) {
        self.ctx.partitions.history.messages.truncate(self.checkpoint_history_len);
        self.ctx.partitions.working.messages.truncate(self.checkpoint_working_len);
        if let Some(ref state) = self.checkpoint_task_state {
            self.ctx.partitions.task_state = state.clone();
        }
        self.observations.push(LoopObservation::Rollbacked {
            turn: self.turn,
            checkpoint_history_len: self.checkpoint_history_len as u32,
        });
    }

    pub fn mount_capability(&mut self, descriptor: crate::types::capability::CapabilityDescriptor) {
        let id = descriptor.id.to_string();
        let kind_str = format!("{:?}", descriptor.kind);
        self.ctx.capabilities.upsert(descriptor);
        self.observations.push(LoopObservation::CapabilityChanged {
            turn: self.turn,
            added: vec![format!("{}:{}", kind_str, id)],
            removed: vec![],
        });
    }

    pub fn unmount_capability(&mut self, kind: crate::types::capability::CapabilityKind, id: &str) {
        self.ctx.capabilities.remove(kind, id);
        let kind_str = format!("{:?}", kind);
        self.observations.push(LoopObservation::CapabilityChanged {
            turn: self.turn,
            added: vec![],
            removed: vec![format!("{}:{}", kind_str, id)],
        });
    }

    // ─── Milestone contract ────────────────────────────────────────────────

    /// Load a milestone contract.  Must be called before `start()`.
    pub fn load_milestone_contract(&mut self, contract: MilestoneContract) {
        self.milestone_contract = Some(contract);
        self.current_milestone_phase = 0;
    }

    /// Returns the ID of the current (not-yet-passed) phase, or `None` when
    /// no contract is loaded or all phases are complete.
    pub fn current_milestone_phase_id(&self) -> Option<&str> {
        self.milestone_contract
            .as_ref()
            .and_then(|c| c.phases.get(self.current_milestone_phase))
            .map(|p| p.id.as_str())
    }

    /// Returns the acceptance criteria of the current phase as a slice.
    pub fn current_milestone_criteria(&self) -> &[String] {
        self.milestone_contract
            .as_ref()
            .and_then(|c| c.phases.get(self.current_milestone_phase))
            .map(|p| p.criteria.as_slice())
            .unwrap_or(&[])
    }

    /// Returns `true` when there is no contract or all phases have passed.
    pub fn is_milestone_complete(&self) -> bool {
        match &self.milestone_contract {
            None => true,
            Some(c) => self.current_milestone_phase >= c.phases.len(),
        }
    }

    fn handle_milestone_result(&mut self, result: MilestoneCheckResult) -> LoopAction {
        self.observations.clear();

        if result.passed {
            // Mount unlocked capabilities and advance the phase.
            let mut unlocked: Vec<String> = Vec::new();
            if let Some(contract) = &self.milestone_contract {
                if let Some(phase) = contract.phases.get(self.current_milestone_phase) {
                    for cap in phase.unlocks.clone() {
                        let kind_str = format!("{:?}", cap.kind);
                        let id = cap.id.to_string();
                        self.ctx.capabilities.upsert(cap);
                        unlocked.push(format!("{}:{}", kind_str, id));
                    }
                    self.observations.push(LoopObservation::MilestoneAdvanced {
                        turn: self.turn,
                        phase_id: phase.id.clone(),
                        capabilities_unlocked: unlocked,
                    });
                }
            }
            self.current_milestone_phase += 1;

            if self.is_milestone_complete() {
                // All phases done — terminate normally.
                return self.terminate(TerminationReason::Completed, None);
            }

            // More phases remain: prompt the LLM with the next phase context.
            if let Some(criteria) = self
                .milestone_contract
                .as_ref()
                .and_then(|c| c.phases.get(self.current_milestone_phase))
                .map(|p| {
                    if p.criteria.is_empty() {
                        format!("[NEXT MILESTONE PHASE: {}]", p.id)
                    } else {
                        format!(
                            "[NEXT MILESTONE PHASE: {} — Criteria: {}]",
                            p.id,
                            p.criteria.join("; ")
                        )
                    }
                })
            {
                let note = Message::user(criteria);
                self.ctx.partitions.working.push(note, 0);
            }
            self.phase = LoopPhase::Reason;
            self.emit_call_llm()
        } else {
            // Assertion failed — inject blocked message and re-invite the LLM.
            let reason = result
                .reason
                .as_deref()
                .unwrap_or("milestone criteria not met");
            let msg = format!(
                "[MILESTONE BLOCKED: {} — {}. Address the criteria and try again.]",
                result.phase_id, reason
            );
            let note = Message::user(msg);
            self.ctx.partitions.working.push(note, 0);
            self.observations.push(LoopObservation::MilestoneBlocked {
                turn: self.turn,
                phase_id: result.phase_id,
                reason: reason.to_string(),
            });
            self.phase = LoopPhase::Reason;
            self.emit_call_llm()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::skill_catalog::SKILL_TOOL_NAME;
    use crate::types::message::Role;
    use crate::types::skill::SkillMetadata;

    fn sm() -> LoopStateMachine {
        LoopStateMachine::new(LoopPolicy {
            max_tokens: 128_000,
            ..LoopPolicy::default()
        })
    }

    #[test]
    fn start_emits_call_llm() {
        let mut sm = sm();
        let action = sm.start(RuntimeTask::new("Say hello"));
        assert!(matches!(action, LoopAction::CallLLM { .. }));
        assert!(matches!(sm.phase, LoopPhase::Reason));
    }

    #[test]
    fn resume_after_preload_runs_pending_tools_before_llm() {
        let mut sm = sm();
        sm.preload_history(vec![
            Message::user("goal"),
            Message {
                role: Role::Assistant,
                content: Content::Text("checking".into()),
                tool_calls: vec![ToolCall {
                    id: compact_str::CompactString::new("call_ping"),
                    name: compact_str::CompactString::new("ping"),
                    arguments: serde_json::json!({}),
                }],
                token_count: Some(5),
            },
        ]);
        match sm.resume_after_preload() {
            LoopAction::ExecuteTools { calls } => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].name.as_str(), "ping");
            }
            other => panic!("expected ExecuteTools, got {other:?}"),
        }
    }

    #[test]
    fn resume_after_preload_emits_call_llm_without_duplicate_user() {
        let mut sm = sm();
        sm.preload_history(vec![
            Message::user("prior goal"),
            Message::assistant("partial"),
        ]);
        let history_len = sm.ctx.partitions.history.messages.len();
        let action = sm.resume_after_preload();
        assert!(matches!(action, LoopAction::CallLLM { .. }));
        assert_eq!(sm.ctx.partitions.history.messages.len(), history_len);
    }

    #[test]
    fn start_places_user_message_in_history_not_working() {
        let mut sm = sm();
        sm.start(RuntimeTask::new("Say hello"));
        // User message goes to history so it appears in the correct chronological position
        assert!(
            !sm.ctx.partitions.history.is_empty(),
            "history should have user message"
        );
        assert!(
            sm.ctx.partitions.working.is_empty(),
            "working should stay empty — signals only"
        );
    }

    #[test]
    fn llm_response_without_tools_terminates_and_saves_to_history() {
        let mut sm = sm();
        sm.start(RuntimeTask::new("Say hello"));
        let action = sm.feed(LoopEvent::LLMResponse {
            message: Message::assistant("Hello!"),
        });
        assert!(matches!(action, LoopAction::Done { .. }));
        assert!(sm.is_terminal());
        // Final response is committed to history
        let history = &sm.ctx.partitions.history.messages;
        assert!(
            history
                .iter()
                .any(|m| m.content.as_text() == Some("Hello!"))
        );
    }

    #[test]
    fn timeout_terminates() {
        let mut sm = sm();
        sm.start(RuntimeTask::new("test"));
        match sm.feed(LoopEvent::Timeout) {
            LoopAction::Done { result } => {
                assert_eq!(result.termination, TerminationReason::Timeout)
            }
            _ => panic!("expected Done"),
        }
    }

    #[test]
    fn critical_signal_goes_to_working_not_history() {
        use crate::types::signal::{SignalSource, SignalType, Urgency};
        let mut sm = sm();
        sm.start(RuntimeTask::new("test"));
        let history_len_before = sm.ctx.partitions.history.messages.len();

        let sig = RuntimeSignal::new(
            SignalSource::Gateway,
            SignalType::Alert,
            Urgency::Critical,
            "fire",
        );
        let action = sm.feed(LoopEvent::Signal { signal: sig });
        assert!(matches!(action, LoopAction::CallLLM { .. }));
        assert!(matches!(sm.phase, LoopPhase::Reason));
        // Signal injected into working
        assert!(sm.ctx.partitions.working.messages.iter().any(|m| {
            m.content
                .as_text()
                .map(|t| t.contains("[INTERRUPT]"))
                .unwrap_or(false)
        }));
        // History did not grow from the signal
        assert_eq!(sm.ctx.partitions.history.messages.len(), history_len_before);
    }

    #[test]
    fn max_turns_emits_final_toolless_call_then_terminates() {
        let mut sm = LoopStateMachine::new(LoopPolicy {
            max_tokens: 128_000,
            max_turns: 1,
            ..LoopPolicy::default()
        });
        sm.start(RuntimeTask::new("test"));

        // After tool results hit maxTurns, kernel emits one final CallLLM with no tools
        let action = sm.feed(LoopEvent::ToolResults { results: vec![] });
        match action {
            LoopAction::CallLLM { tools, .. } => {
                assert!(tools.is_empty(), "final call must have no tools")
            }
            _ => panic!("expected CallLLM for final text-only call"),
        }

        // The LLM responds with text → terminates with MaxTurns
        let action = sm.feed(LoopEvent::LLMResponse {
            message: Message::assistant("final summary"),
        });
        match action {
            LoopAction::Done { result } => {
                assert_eq!(result.termination, TerminationReason::MaxTurns);
                assert!(
                    result.final_message.is_some(),
                    "final message must be preserved"
                );
            }
            _ => panic!("expected Done"),
        }
    }

    #[test]
    fn skill_tool_injected_in_call_llm_when_skills_registered() {
        let mut sm = sm();
        sm.ctx
            .set_available_skills(vec![SkillMetadata::new("debug", "Debug helper")]);
        let action = sm.start(RuntimeTask::new("Fix the bug"));
        match action {
            LoopAction::CallLLM { tools, .. } => {
                assert!(tools.iter().any(|t| t.name.as_str() == SKILL_TOOL_NAME));
            }
            _ => panic!("expected CallLLM"),
        }
    }

    #[test]
    fn skill_tool_not_injected_when_no_skills() {
        let mut sm = sm();
        let action = sm.start(RuntimeTask::new("Say hello"));
        match action {
            LoopAction::CallLLM { tools, .. } => {
                assert!(!tools.iter().any(|t| t.name.as_str() == SKILL_TOOL_NAME));
            }
            _ => panic!("expected CallLLM"),
        }
    }

    #[test]
    fn compression_emits_observation() {
        let mut sm = LoopStateMachine::new(LoopPolicy {
            max_tokens: 100,
            max_turns: 100,
            ..LoopPolicy::default()
        });
        sm.start(RuntimeTask::new("test"));
        for i in 0..10 {
            sm.ctx
                .push_history(Message::user(format!("filler {i}")), 50);
        }
        sm.feed(LoopEvent::ToolResults { results: vec![] });
        let obs = sm.take_observations();
        assert!(
            obs.iter()
                .any(|o| matches!(o, LoopObservation::Compressed { .. }))
        );
    }

    #[test]
    fn renewal_emits_observation_when_pressure_extreme() {
        // Renewal fires only when pressure stays > 0.98 even AFTER compression.
        // Compression only targets history + skill, so we saturate the system
        // partition (non-compressible) to keep rho above the threshold.
        let mut sm = LoopStateMachine::new(LoopPolicy {
            max_tokens: 100,
            max_turns: 100,
            ..LoopPolicy::default()
        });
        sm.start(RuntimeTask::new("test"));
        // 10 system messages × 10 tokens = 100 tokens in non-compressible partition.
        // rho = 100/100 = 1.0 > 0.98; compression on history saves nothing meaningful.
        for i in 0..10 {
            sm.ctx
                .partitions
                .system
                .push(Message::system(format!("constraint {i}")), 10);
        }
        sm.feed(LoopEvent::ToolResults { results: vec![] });
        let obs = sm.take_observations();
        assert!(
            obs.iter()
                .any(|o| matches!(o, LoopObservation::Renewed { .. }))
        );
    }

    #[test]
    fn preload_history_and_drain_new_messages() {
        let mut sm = sm();

        // Simulate restoring a prior session with one exchange
        let prior = vec![
            Message::user("Hello from last time"),
            Message::assistant("Hi! I remember."),
        ];
        sm.preload_history(prior.clone());
        assert_eq!(sm.ctx.partitions.history.messages.len(), 2);

        // Start a new turn
        sm.start(RuntimeTask::new("What did I say before?"));

        // New messages = user message from start() + (after termination) final assistant
        let new_msgs = sm.drain_new_messages();
        // At minimum the new user message must be present
        assert!(!new_msgs.is_empty());
        assert!(new_msgs.iter().any(|m| {
            m.content
                .as_text()
                .map(|t| t.contains("What did I say before"))
                .unwrap_or(false)
        }));
        // Prior session messages are NOT in drain_new_messages
        assert!(!new_msgs.iter().any(|m| {
            m.content
                .as_text()
                .map(|t| t.contains("Hello from last time"))
                .unwrap_or(false)
        }));
    }

    #[test]
    fn tool_result_content_parts_preserved_as_json() {
        use crate::types::message::Content;
        use compact_str::CompactString;

        let mut sm = sm();
        sm.start(RuntimeTask::new("test"));

        // Simulate an LLM tool call
        let mut msg = Message::assistant("");
        msg.tool_calls.push(crate::types::message::ToolCall {
            id: CompactString::new("c1"),
            name: CompactString::new("my_tool"),
            arguments: serde_json::json!({}),
        });
        sm.feed(LoopEvent::LLMResponse { message: msg });

        // Feed a structured (Parts) tool result
        let structured = Content::Parts(vec![ContentPart::Text {
            text: "structured output".to_string(),
        }]);
        sm.feed(LoopEvent::ToolResults {
            results: vec![ToolResult {
                call_id: CompactString::new("c1"),
                output: structured,
                is_error: false,
                is_fatal: false,
                token_count: None,
            }],
        });

        // The history should contain a tool message with JSON-serialised content
        let tool_msgs: Vec<_> = sm
            .ctx
            .partitions
            .history
            .messages
            .iter()
            .filter(|m| matches!(m.role, crate::types::message::Role::Tool))
            .collect();
        assert!(
            !tool_msgs.is_empty(),
            "tool result message must be in history"
        );
        // Content is Parts (ToolResult part), not empty
        if let Content::Parts(parts) = &tool_msgs[0].content {
            assert!(!parts.is_empty());
        }
    }

    // ─── Milestone contract tests ──────────────────────────────────────────

    fn make_tool_schema(name: &str) -> ToolSchema {
        ToolSchema {
            name: compact_str::CompactString::new(name),
            description: format!("tool {name}"),
            parameters: serde_json::json!({"type": "object"}),
        }
    }

    #[test]
    fn milestone_contract_loads_and_reports_current_phase() {
        let mut sm = sm();
        let contract = crate::types::milestone::MilestoneContract::new()
            .phase(crate::types::milestone::MilestonePhase::new("phase-a")
                .with_criterion("Output contains 'hello'"))
            .phase(crate::types::milestone::MilestonePhase::new("phase-b"));

        sm.load_milestone_contract(contract);
        assert_eq!(sm.current_milestone_phase_id(), Some("phase-a"));
        assert!(!sm.is_milestone_complete());
        assert_eq!(sm.current_milestone_criteria(), &["Output contains 'hello'"]);
    }

    #[test]
    fn milestone_pass_advances_phase_and_emits_observation() {
        let mut sm = sm();
        let contract = crate::types::milestone::MilestoneContract::new()
            .phase(crate::types::milestone::MilestonePhase::new("plan"))
            .phase(crate::types::milestone::MilestonePhase::new("implement"));
        sm.load_milestone_contract(contract);
        sm.start(RuntimeTask::new("do the thing"));

        // Simulate LLM returning text-only → EvaluateMilestone
        let action = sm.feed(LoopEvent::LLMResponse {
            message: Message::assistant("plan drafted"),
        });
        assert!(
            matches!(action, LoopAction::EvaluateMilestone { ref phase_id, .. } if phase_id == "plan"),
            "expected EvaluateMilestone for 'plan', got {action:?}",
        );

        // Feed a passing result
        let action2 = sm.feed(LoopEvent::MilestoneResult {
            result: crate::types::milestone::MilestoneCheckResult::pass("plan"),
        });
        assert!(
            matches!(action2, LoopAction::CallLLM { .. }),
            "expect CallLLM after milestone advance",
        );
        assert_eq!(sm.current_milestone_phase_id(), Some("implement"));

        let obs = sm.take_observations();
        assert!(obs.iter().any(|o| matches!(
            o,
            LoopObservation::MilestoneAdvanced { phase_id, .. } if phase_id == "plan"
        )));
    }

    #[test]
    fn milestone_fail_blocks_phase_and_emits_observation() {
        let mut sm = sm();
        let contract = crate::types::milestone::MilestoneContract::new()
            .phase(crate::types::milestone::MilestonePhase::new("plan"));
        sm.load_milestone_contract(contract);
        sm.start(RuntimeTask::new("do the thing"));

        sm.feed(LoopEvent::LLMResponse {
            message: Message::assistant("bad plan"),
        });

        let action = sm.feed(LoopEvent::MilestoneResult {
            result: crate::types::milestone::MilestoneCheckResult::fail("plan", "missing evidence"),
        });
        assert!(matches!(action, LoopAction::CallLLM { .. }), "blocked run must return CallLLM");
        // Phase index must NOT advance
        assert_eq!(sm.current_milestone_phase_id(), Some("plan"));

        let obs = sm.take_observations();
        assert!(obs.iter().any(|o| matches!(
            o,
            LoopObservation::MilestoneBlocked { phase_id, reason, .. }
            if phase_id == "plan" && reason.contains("missing evidence")
        )));
    }

    #[test]
    fn milestone_unlocks_capabilities_on_advance() {
        let mut sm = sm();
        let schema = make_tool_schema("deploy_tool");
        let cap = crate::types::capability::CapabilityDescriptor::tool(schema);

        let contract = crate::types::milestone::MilestoneContract::new()
            .phase(
                crate::types::milestone::MilestonePhase::new("phase-a")
                    .unlocking(cap),
            );
        sm.load_milestone_contract(contract);
        sm.start(RuntimeTask::new("build pipeline"));

        // Confirm tool not yet in manifest
        assert!(sm.ctx.capabilities.by_kind(
            crate::types::capability::CapabilityKind::Tool
        ).is_empty());

        sm.feed(LoopEvent::LLMResponse { message: Message::assistant("done") });
        sm.feed(LoopEvent::MilestoneResult {
            result: crate::types::milestone::MilestoneCheckResult::pass("phase-a"),
        });

        // Tool must now be in the capability manifest
        let tools = sm.ctx.capabilities.by_kind(crate::types::capability::CapabilityKind::Tool);
        assert!(
            tools.iter().any(|c| c.id.as_str() == "deploy_tool"),
            "deploy_tool should be unlocked after phase-a passes",
        );

        // And capability_unlocked list in observation
        let obs = sm.take_observations();
        let advanced = obs.iter().find_map(|o| {
            if let LoopObservation::MilestoneAdvanced { capabilities_unlocked, .. } = o {
                Some(capabilities_unlocked)
            } else {
                None
            }
        });
        assert!(advanced.is_some(), "MilestoneAdvanced observation expected");
        assert!(advanced.unwrap().iter().any(|s| s.contains("deploy_tool")));
    }

    #[test]
    fn all_phases_complete_terminates_run() {
        let mut sm = sm();
        let contract = crate::types::milestone::MilestoneContract::new()
            .phase(crate::types::milestone::MilestonePhase::new("only-phase"));
        sm.load_milestone_contract(contract);
        sm.start(RuntimeTask::new("single milestone run"));

        sm.feed(LoopEvent::LLMResponse { message: Message::assistant("ready") });
        let done = sm.feed(LoopEvent::MilestoneResult {
            result: crate::types::milestone::MilestoneCheckResult::pass("only-phase"),
        });

        assert!(sm.is_milestone_complete());
        assert!(matches!(done, LoopAction::Done { .. }), "all phases done must produce Done");
    }

    #[test]
    fn no_contract_terminates_normally() {
        let mut sm = sm();
        // No milestone contract loaded
        sm.start(RuntimeTask::new("simple task"));

        let action = sm.feed(LoopEvent::LLMResponse {
            message: Message::assistant("answer"),
        });
        assert!(
            matches!(action, LoopAction::Done { .. }),
            "without milestone contract, text-only response must terminate: {action:?}",
        );
    }

    #[test]
    fn mount_unmount_capability_emits_observation() {
        let mut sm = sm();
        let schema = ToolSchema {
            name: compact_str::CompactString::new("test_tool"),
            description: "test description".to_string(),
            parameters: serde_json::json!({ "type": "object" }),
        };
        let desc = crate::types::capability::CapabilityDescriptor::tool(schema);

        sm.mount_capability(desc);

        let obs = sm.take_observations();
        assert_eq!(obs.len(), 1);
        if let LoopObservation::CapabilityChanged { turn, added, removed } = &obs[0] {
            assert_eq!(*turn, 0);
            assert_eq!(added, &vec!["Tool:test_tool".to_string()]);
            assert!(removed.is_empty());
        } else {
            panic!("Expected CapabilityChanged observation");
        }

        sm.unmount_capability(crate::types::capability::CapabilityKind::Tool, "test_tool");
        let obs2 = sm.take_observations();
        assert_eq!(obs2.len(), 1);
        if let LoopObservation::CapabilityChanged { turn, added, removed } = &obs2[0] {
            assert_eq!(*turn, 0);
            assert!(added.is_empty());
            assert_eq!(removed, &vec!["Tool:test_tool".to_string()]);
        } else {
            panic!("Expected CapabilityChanged observation");
        }
    }
}
