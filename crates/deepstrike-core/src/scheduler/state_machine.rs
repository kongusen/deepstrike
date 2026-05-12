use crate::context::manager::ContextManager;
use crate::context::pressure::PressureAction;
use crate::types::message::{ContentPart, Message, ToolCall, ToolResult, ToolSchema};
use crate::types::result::{LoopResult, TerminationReason};
use crate::types::signal::{RuntimeSignal, Urgency};
use crate::types::task::RuntimeTask;
use super::policy::LoopPolicy;

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
    Start { task: RuntimeTask },
    LLMResponse { message: Message },
    ToolResults { results: Vec<ToolResult> },
    /// Inbound signal from SignalRouter — Critical/High urgency may interrupt.
    Signal { signal: RuntimeSignal },
    Timeout,
}

/// Actions the state machine outputs — SDK layer executes the I/O.
#[derive(Debug)]
pub enum LoopAction {
    /// `tools` always includes the `skill` meta-tool when skills are registered.
    CallLLM { messages: Vec<Message>, tools: Vec<ToolSchema> },
    ExecuteTools { calls: Vec<ToolCall> },
    Done { result: LoopResult },
}

/// One-shot observation emitted by the kernel during `feed`.
/// SDK drains this between calls for telemetry/UI updates.
#[derive(Debug, Clone)]
pub enum LoopObservation {
    Compressed { action: PressureAction, rho_after: f64 },
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
}

impl LoopStateMachine {
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
        }
    }

    pub fn start(&mut self, task: RuntimeTask) -> LoopAction {
        self.observations.clear();
        self.ctx.current_goal = task.goal.clone();

        let user_msg = if task.criteria.is_empty() {
            task.goal
        } else {
            let criteria_text = task.criteria.iter().enumerate()
                .map(|(i, c)| format!("{}. {}", i + 1, c))
                .collect::<Vec<_>>()
                .join("\n");
            format!("{}\n\nCriteria:\n{}", task.goal, criteria_text)
        };

        self.ctx.partitions.working.push(Message::user(user_msg), 0);
        self.phase = LoopPhase::Reason;
        self.emit_call_llm()
    }

    pub fn feed(&mut self, event: LoopEvent) -> LoopAction {
        self.observations.clear();
        match event {
            LoopEvent::Start { task } => self.start(task),

            LoopEvent::LLMResponse { message } => {
                let tokens = message.token_count.unwrap_or(0);
                self.total_tokens += tokens as u64;

                if let Some(reason) = self.pending_termination.take() {
                    return self.terminate(reason, Some(message));
                }

                if message.tool_calls.is_empty() {
                    return self.terminate(TerminationReason::Completed, Some(message));
                }

                let calls = message.tool_calls.clone();
                self.ctx.push_history(message, tokens);
                self.phase = LoopPhase::Act { tool_calls: calls.clone() };
                LoopAction::ExecuteTools { calls }
            }

            LoopEvent::ToolResults { results } => {
                for r in &results {
                    self.total_tokens += r.token_count.unwrap_or(0) as u64;
                    let parts = vec![ContentPart::ToolResult {
                        call_id: r.call_id.clone(),
                        output: r.output.as_text().unwrap_or("").to_string(),
                        is_error: r.is_error,
                    }];
                    let tokens = r.token_count.unwrap_or(0);
                    self.ctx.push_history(Message::tool(parts), tokens);
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
                self.phase = LoopPhase::Delta { pressure: self.ctx.rho() };
                if action != PressureAction::None {
                    self.ctx.compress(action);
                    self.observations.push(LoopObservation::Compressed {
                        action,
                        rho_after: self.ctx.rho(),
                    });
                }

                self.phase = LoopPhase::Reason;
                self.emit_call_llm()
            }

            LoopEvent::Signal { signal } => {
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

    fn terminate(&mut self, termination: TerminationReason, final_message: Option<Message>) -> LoopAction {
        let result = LoopResult {
            termination,
            final_message,
            turns_used: self.turn,
            total_tokens_used: self.total_tokens,
        };
        self.phase = LoopPhase::Terminal { result: result.clone() };
        LoopAction::Done { result }
    }

    /// Build the `CallLLM` action, automatically appending the `skill` and `memory`
    /// meta-tools when they are configured — the LLM can invoke them on demand.
    /// When `pending_termination` is set, tools are stripped to force a text-only response.
    fn emit_call_llm(&self) -> LoopAction {
        let messages = self.ctx.render();
        if self.pending_termination.is_some() {
            return LoopAction::CallLLM { messages, tools: Vec::new() };
        }
        let mut tools = self.tools.clone();
        if let Some(skill_tool) = self.ctx.skill_tool_schema() { tools.push(skill_tool); }
        if let Some(memory_tool) = self.ctx.memory_tool_schema() { tools.push(memory_tool); }
        if let Some(knowledge_tool) = self.ctx.knowledge_tool_schema() { tools.push(knowledge_tool); }
        LoopAction::CallLLM { messages, tools }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::skill::SkillMetadata;
    use crate::context::skill_catalog::SKILL_TOOL_NAME;

    fn sm() -> LoopStateMachine {
        LoopStateMachine::new(LoopPolicy { max_tokens: 128_000, ..LoopPolicy::default() })
    }

    #[test]
    fn start_emits_call_llm() {
        let mut sm = sm();
        let action = sm.start(RuntimeTask::new("Say hello"));
        assert!(matches!(action, LoopAction::CallLLM { .. }));
        assert!(matches!(sm.phase, LoopPhase::Reason));
    }

    #[test]
    fn llm_response_without_tools_terminates() {
        let mut sm = sm();
        sm.start(RuntimeTask::new("Say hello"));
        let action = sm.feed(LoopEvent::LLMResponse { message: Message::assistant("Hello!") });
        assert!(matches!(action, LoopAction::Done { .. }));
        assert!(sm.is_terminal());
    }

    #[test]
    fn timeout_terminates() {
        let mut sm = sm();
        sm.start(RuntimeTask::new("test"));
        match sm.feed(LoopEvent::Timeout) {
            LoopAction::Done { result } => assert_eq!(result.termination, TerminationReason::Timeout),
            _ => panic!("expected Done"),
        }
    }

    #[test]
    fn critical_signal_interrupts_and_rereason() {
        use crate::types::signal::{SignalSource, SignalType, Urgency};
        let mut sm = sm();
        sm.start(RuntimeTask::new("test"));
        let sig = RuntimeSignal::new(SignalSource::Gateway, SignalType::Alert, Urgency::Critical, "fire");
        let action = sm.feed(LoopEvent::Signal { signal: sig });
        assert!(matches!(action, LoopAction::CallLLM { .. }));
        assert!(matches!(sm.phase, LoopPhase::Reason));
        assert!(sm.ctx.partitions.working.messages.iter().any(|m| {
            m.content.as_text().map(|t| t.contains("[INTERRUPT]")).unwrap_or(false)
        }));
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
            LoopAction::CallLLM { tools, .. } => assert!(tools.is_empty(), "final call must have no tools"),
            _ => panic!("expected CallLLM for final text-only call"),
        }

        // The LLM responds with text → terminates with MaxTurns
        let action = sm.feed(LoopEvent::LLMResponse {
            message: Message::assistant("final summary"),
        });
        match action {
            LoopAction::Done { result } => {
                assert_eq!(result.termination, TerminationReason::MaxTurns);
                assert!(result.final_message.is_some(), "final message must be preserved");
            }
            _ => panic!("expected Done"),
        }
    }

    #[test]
    fn skill_tool_injected_in_call_llm_when_skills_registered() {
        let mut sm = sm();
        sm.ctx.set_available_skills(vec![
            SkillMetadata::new("debug", "Debug helper"),
        ]);
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
            sm.ctx.push_history(Message::user(format!("filler {i}")), 50);
        }
        sm.feed(LoopEvent::ToolResults { results: vec![] });
        let obs = sm.take_observations();
        assert!(obs.iter().any(|o| matches!(o, LoopObservation::Compressed { .. })));
    }
}
