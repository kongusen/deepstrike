#![allow(deprecated)]

//! Stable host/kernel ABI types.
//!
//! This module is the narrow contract SDKs should bind to over time. It wraps
//! the existing loop state machine without changing behavior, giving FFI layers
//! a versioned input/action/observation vocabulary before the larger runner
//! refactor lands.

use serde::{Deserialize, Serialize};

use crate::context::pressure::PressureAction;
use crate::context::renderer::RenderedContext;
use crate::context::task_state::TaskUpdate;
use crate::context::token_engine::ContextTokenEngine;
use crate::scheduler::policy::LoopPolicy;
use crate::scheduler::state_machine::{LoopAction, LoopEvent, LoopObservation, LoopStateMachine};
use crate::types::capability::{CapabilityCommand, CapabilityDescriptor, CapabilityKind};
use crate::types::agent::AgentRunSpec;
use crate::types::message::{Message, ToolCall, ToolResult, ToolSchema};
use crate::types::milestone::{MilestoneCheckResult, MilestoneContract};
use crate::types::result::LoopResult;
use crate::types::signal::RuntimeSignal;
use crate::types::skill::SkillMetadata;
use crate::types::task::RuntimeTask;

pub const KERNEL_ABI_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KernelInput {
    pub version: u32,
    pub event: KernelInputEvent,
}

impl KernelInput {
    pub fn new(event: KernelInputEvent) -> Self {
        Self {
            version: KERNEL_ABI_VERSION,
            event,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KernelInputEvent {
    SetTools {
        tools: Vec<ToolSchema>,
    },
    SetAvailableSkills {
        skills: Vec<SkillMetadata>,
    },
    SetMemoryEnabled {
        enabled: bool,
    },
    SetKnowledgeEnabled {
        enabled: bool,
    },
    SetPlanToolEnabled {
        enabled: bool,
    },
    SetTokenizer {
        name: String,
    },
    AddSystemMessage {
        content: String,
        tokens: u32,
    },
    AddMemoryMessage {
        content: String,
        tokens: u32,
    },
    AddHistoryMessage {
        message: Message,
        tokens: Option<u32>,
    },
    PreloadHistory {
        messages: Vec<Message>,
    },
    MountCapability {
        capability: CapabilityDescriptor,
    },
    UnmountCapability {
        capability_kind: CapabilityKind,
        id: String,
    },
    LoadMilestoneContract {
        contract: MilestoneContract,
    },
    PushArtifact {
        message: Message,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tokens: Option<u32>,
    },
    ForceCompact,
    UpdateTask {
        update: TaskUpdate,
    },
    StartRun {
        task: RuntimeTask,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_spec: Option<AgentRunSpec>,
    },
    CapabilityCommand {
        command: CapabilityCommand,
    },
    Resume,
    ProviderResult { message: Message },
    ToolResults { results: Vec<ToolResult> },
    Signal { signal: RuntimeSignal },
    MilestoneResult { result: MilestoneCheckResult },
    Timeout,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KernelStep {
    pub version: u32,
    pub actions: Vec<KernelAction>,
    pub observations: Vec<KernelObservation>,
}

impl KernelStep {
    fn empty(observations: Vec<LoopObservation>) -> Self {
        Self {
            version: KERNEL_ABI_VERSION,
            actions: Vec::new(),
            observations: observations.into_iter().map(Into::into).collect(),
        }
    }

    fn single(action: LoopAction, observations: Vec<LoopObservation>) -> Self {
        Self {
            version: KERNEL_ABI_VERSION,
            actions: vec![action.into()],
            observations: observations.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KernelAction {
    CallProvider {
        context: RenderedContext,
        tools: Vec<ToolSchema>,
    },
    ExecuteTool {
        calls: Vec<ToolCall>,
    },
    EvaluateMilestone {
        phase_id: String,
        criteria: Vec<String>,
    },
    Done {
        result: LoopResult,
    },
}

impl From<LoopAction> for KernelAction {
    fn from(action: LoopAction) -> Self {
        match action {
            LoopAction::CallLLM { context, tools } => Self::CallProvider { context, tools },
            LoopAction::ExecuteTools { calls } => Self::ExecuteTool { calls },
            LoopAction::EvaluateMilestone { phase_id, criteria } => {
                Self::EvaluateMilestone { phase_id, criteria }
            }
            LoopAction::Done { result } => Self::Done { result },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KernelObservation {
    Compressed {
        action: KernelPressureAction,
        rho_after: f64,
        summary: Option<String>,
        archived: Vec<Message>,
    },
    Renewed {
        sprint: u32,
    },
    Rollbacked {
        turn: u32,
        checkpoint_history_len: u32,
    },
    CapabilityChanged {
        turn: u32,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        added: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        removed: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        change_kind: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        capability_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mounted_by: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mount_reason: Option<String>,
    },
    MilestoneAdvanced {
        turn: u32,
        phase_id: String,
        capabilities_unlocked: Vec<String>,
    },
    MilestoneBlocked {
        turn: u32,
        phase_id: String,
        reason: String,
    },
}

impl From<LoopObservation> for KernelObservation {
    fn from(observation: LoopObservation) -> Self {
        match observation {
            LoopObservation::Compressed {
                action,
                rho_after,
                summary,
                archived,
            } => Self::Compressed {
                action: action.into(),
                rho_after,
                summary,
                archived,
            },
            LoopObservation::Renewed { sprint } => Self::Renewed { sprint },
            LoopObservation::Rollbacked {
                turn,
                checkpoint_history_len,
            } => Self::Rollbacked {
                turn,
                checkpoint_history_len,
            },
            LoopObservation::CapabilityChanged {
                turn,
                added,
                removed,
                change_kind,
                capability_id,
                version,
                mounted_by,
                mount_reason,
            } => Self::CapabilityChanged {
                turn,
                added,
                removed,
                change_kind,
                capability_id,
                version,
                mounted_by,
                mount_reason,
            },
            LoopObservation::MilestoneAdvanced {
                turn,
                phase_id,
                capabilities_unlocked,
            } => Self::MilestoneAdvanced {
                turn,
                phase_id,
                capabilities_unlocked,
            },
            LoopObservation::MilestoneBlocked {
                turn,
                phase_id,
                reason,
            } => Self::MilestoneBlocked {
                turn,
                phase_id,
                reason,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KernelPressureAction {
    None,
    SnipCompact,
    MicroCompact,
    ContextCollapse,
    AutoCompact,
}

impl From<PressureAction> for KernelPressureAction {
    fn from(action: PressureAction) -> Self {
        match action {
            PressureAction::None => Self::None,
            PressureAction::SnipCompact => Self::SnipCompact,
            PressureAction::MicroCompact => Self::MicroCompact,
            PressureAction::ContextCollapse => Self::ContextCollapse,
            PressureAction::AutoCompact => Self::AutoCompact,
        }
    }
}

/// Pure kernel runtime wrapper. SDKs should migrate toward feeding
/// `KernelInput` values here instead of directly driving `LoopStateMachine`.
pub struct KernelRuntime {
    sm: LoopStateMachine,
}

impl KernelRuntime {
    pub fn new(policy: LoopPolicy) -> Self {
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

    pub fn step(&mut self, input: KernelInput) -> KernelStep {
        let action = match input.event {
            KernelInputEvent::SetTools { tools } => {
                self.sm.tools = tools;
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::SetAvailableSkills { skills } => {
                self.sm.ctx.set_available_skills(skills);
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
            KernelInputEvent::SetTokenizer { name } => {
                self.sm.ctx.engine = match name.as_str() {
                    "tiktoken_cl100k" | "cl100k" => ContextTokenEngine::cl100k(),
                    "tiktoken_o200k" | "o200k" => ContextTokenEngine::o200k(),
                    _ => ContextTokenEngine::char_approx(),
                };
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
            KernelInputEvent::AddMemoryMessage { content, tokens } => {
                self.sm
                    .ctx
                    .partitions
                    .memory
                    .push(Message::user(content), tokens.max(1));
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
            KernelInputEvent::PushArtifact { message, tokens } => {
                let token_count = tokens.unwrap_or_else(|| self.sm.ctx.engine.count_message(&message));
                self.sm.ctx.push_artifact(message, token_count.max(1));
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
            KernelInputEvent::Resume => self.sm.resume_after_preload(),
            KernelInputEvent::ProviderResult { message } => {
                self.sm.feed(LoopEvent::LLMResponse { message })
            }
            KernelInputEvent::ToolResults { results } => {
                self.sm.feed(LoopEvent::ToolResults { results })
            }
            KernelInputEvent::Signal { signal } => self.sm.feed(LoopEvent::Signal { signal }),
            KernelInputEvent::MilestoneResult { result } => {
                self.sm.feed(LoopEvent::MilestoneResult { result })
            }
            KernelInputEvent::Timeout => self.sm.feed(LoopEvent::Timeout),
        };
        KernelStep::single(action, self.sm.take_observations())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_run_returns_versioned_provider_action() {
        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        let step = runtime.step(KernelInput::new(KernelInputEvent::StartRun {
            task: RuntimeTask::new("ship it"),
            run_spec: None,
        }));

        assert_eq!(step.version, KERNEL_ABI_VERSION);
        assert!(matches!(
            step.actions.as_slice(),
            [KernelAction::CallProvider { .. }]
        ));
    }

    #[test]
    fn provider_text_response_returns_done() {
        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        runtime.step(KernelInput::new(KernelInputEvent::StartRun {
            task: RuntimeTask::new("ship it"),
            run_spec: None,
        }));
        let step = runtime.step(KernelInput::new(KernelInputEvent::ProviderResult {
            message: Message::assistant("done"),
        }));

        assert!(matches!(
            step.actions.as_slice(),
            [KernelAction::Done { .. }]
        ));
    }

    #[test]
    fn config_inputs_mutate_runtime_without_actions() {
        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        let step = runtime.step(KernelInput::new(KernelInputEvent::SetTools {
            tools: vec![ToolSchema {
                name: "echo".into(),
                description: "Echo input".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }],
        }));

        assert!(step.actions.is_empty());
        assert_eq!(runtime.state_machine().tools.len(), 1);
    }

    #[test]
    fn update_task_input_mutates_task_state() {
        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        let step = runtime.step(KernelInput::new(KernelInputEvent::UpdateTask {
            update: TaskUpdate {
                progress: Some("tools executed".to_string()),
                ..Default::default()
            },
        }));

        assert!(step.actions.is_empty());
        assert_eq!(
            runtime.state_machine().ctx.partitions.task_state.progress,
            "tools executed"
        );
    }

    #[test]
    fn push_artifact_enters_artifacts_partition() {
        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        let step = runtime.step(KernelInput::new(KernelInputEvent::PushArtifact {
            message: Message::assistant("artifact content"),
            tokens: Some(10),
        }));

        assert!(step.actions.is_empty());
        assert_eq!(
            runtime.state_machine().ctx.partitions.artifacts.messages.len(),
            1
        );
    }

    #[test]
    fn capability_mount_emits_observation() {
        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        let step = runtime.step(KernelInput::new(KernelInputEvent::MountCapability {
            capability: CapabilityDescriptor::marker(
                CapabilityKind::McpServer,
                "docs",
                "Documentation server",
            ),
        }));

        assert!(step.actions.is_empty());
        assert!(matches!(
            step.observations.as_slice(),
            [KernelObservation::CapabilityChanged { .. }]
        ));
    }
}
