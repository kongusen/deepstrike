//! Stable host/kernel ABI types.
//!
//! This module is the narrow contract SDKs should bind to over time. It wraps
//! the existing loop state machine without changing behavior, giving FFI layers
//! a versioned input/action/observation vocabulary before the larger runner
//! refactor lands.

use serde::{Deserialize, Serialize};

use crate::context::pressure::PressureAction;
use crate::context::renderer::RenderedContext;
use crate::scheduler::policy::LoopPolicy;
use crate::scheduler::state_machine::{LoopAction, LoopEvent, LoopObservation, LoopStateMachine};
use crate::types::message::{Message, ToolCall, ToolResult, ToolSchema};
use crate::types::milestone::MilestoneCheckResult;
use crate::types::result::LoopResult;
use crate::types::signal::RuntimeSignal;
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
    StartRun { task: RuntimeTask },
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
        added: Vec<String>,
        removed: Vec<String>,
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
            } => Self::CapabilityChanged {
                turn,
                added,
                removed,
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
            KernelInputEvent::StartRun { task } => self.sm.start(task),
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
        }));
        let step = runtime.step(KernelInput::new(KernelInputEvent::ProviderResult {
            message: Message::assistant("done"),
        }));

        assert!(matches!(
            step.actions.as_slice(),
            [KernelAction::Done { .. }]
        ));
    }
}
