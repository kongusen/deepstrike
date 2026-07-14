//! Stable host/kernel ABI.
//!
//! The public protocol, runtime dispatcher, and contract tests live in focused
//! submodules so the wire contract can evolve independently from step execution.

use serde::{Deserialize, Serialize};

use crate::context::pressure::PressureAction;
use crate::context::renderer::RenderedContext;
use crate::context::task_state::TaskUpdate;
use crate::context::token_engine::ContextTokenEngine;
use crate::runtime::session::RollbackReason;
use crate::scheduler::policy::SchedulerBudget;
use crate::scheduler::state_machine::{LoopAction, LoopEvent, LoopStateMachine};
use crate::types::agent::AgentRunSpec;
use crate::types::capability::{CapabilityCommand, CapabilityDescriptor, CapabilityKind};
use crate::types::message::{Message, ToolCall, ToolResult, ToolSchema};
use crate::types::milestone::{MilestoneCheckResult, MilestoneContract};
use crate::types::result::{LoopResult, SubAgentResult};
use crate::types::signal::RuntimeSignal;
use crate::types::skill::SkillMetadata;
use crate::types::task::RuntimeTask;

mod protocol;
mod runtime;

pub use protocol::*;
pub use runtime::KernelRuntime;

#[cfg(test)]
mod tests;
