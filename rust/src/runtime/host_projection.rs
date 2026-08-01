//! Rust-runner projection of canonical effects.
//!
//! These types are host-side control-flow DTOs, not a second kernel wire contract. The canonical
//! envelope and planned step remain the only ABI; this projection only gives the Rust runner an
//! ergonomic exhaustive match over work it must execute.

use deepstrike_core::context::renderer::RenderedContext;
use deepstrike_core::mm::memory::{MemoryQuery, MemoryRecord};
use deepstrike_core::orchestration::workflow::{WorkflowBudget, WorkflowSpawnInfo};
use deepstrike_core::runtime::kernel::KernelPressureAction;
use deepstrike_core::scheduler::state_machine::ApprovalRequest;
use deepstrike_core::types::message::{Message, ToolCall, ToolSchema};
use deepstrike_core::types::milestone::MilestoneVerifier;
use deepstrike_core::types::result::LoopResult;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct HostAction {
    pub effect_id: String,
    pub causation_id: String,
    #[serde(flatten)]
    pub effect: HostEffect,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum HostEffect {
    CallProvider {
        context: RenderedContext,
        tools: Vec<ToolSchema>,
    },
    ExecuteTool {
        calls: Vec<ToolCall>,
    },
    RequestApproval {
        requests: Vec<ApprovalRequest>,
    },
    SpawnWorkflow {
        nodes: Vec<WorkflowSpawnInfo>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        budget: Option<WorkflowBudget>,
    },
    PreemptSubAgents {
        agent_ids: Vec<String>,
        reason: String,
    },
    PersistMemory {
        memory: MemoryRecord,
    },
    QueryMemory {
        query: MemoryQuery,
        requested_k: usize,
    },
    ArchivePageOut {
        turn: u32,
        action: KernelPressureAction,
        summary: Option<String>,
        archived: Vec<Message>,
        tier: String,
    },
    LoadPayload {
        handle_id: String,
        payload_ref: String,
    },
    EvaluateMilestone {
        phase_id: String,
        criteria: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        verifier: Option<MilestoneVerifier>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        required_evidence: Vec<String>,
    },
    Done {
        result: LoopResult,
    },
}
