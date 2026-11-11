//! Rust-runner projection of canonical effects.
//!
//! These types are host-side control-flow DTOs, not a second kernel wire contract. The canonical
//! envelope and planned step remain the only ABI; this projection only gives the Rust runner an
//! ergonomic exhaustive match over work it must execute.

use deepstrike_core::context::renderer::InternalRenderedContext;
use deepstrike_core::mm::memory::{MemoryQuery, MemoryRecord};
use deepstrike_core::orchestration::workflow::{WorkflowBudget, WorkflowSpawnInfo};
use deepstrike_core::runtime::kernel::KernelPressureAction;
use deepstrike_core::scheduler::state_machine::ApprovalRequest;
use deepstrike_core::types::message::{CoreMessage, ToolCall, ToolSchema};
use deepstrike_core::types::milestone::MilestoneVerifier;
use deepstrike_core::types::result::LoopResult;

/// Plan task 1.1 · one parent knowledge item a child run starts with, chosen by the kernel from
/// the item's scope and the node's context inheritance (a blackboard entry, for one).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InheritedKnowledge {
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    /// Plan task 7.4 · the parent run's item this is a copy of; reported as an `inherited_from` edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inherited_from: Option<serde_json::Value>,
}

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
        context: InternalRenderedContext,
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
        /// Plan task 3.1 · per node, the dependency outputs its child run holds, by agent id.
        #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
        dependency_outputs: std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
        /// Plan task 1.1 · per node, the parent knowledge its child run starts with, by agent id.
        #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
        inherited_knowledge: std::collections::BTreeMap<String, Vec<InheritedKnowledge>>,
    },
    PreemptSubAgents {
        agent_ids: Vec<String>,
        reason: String,
    },
    PersistMemory {
        memory: MemoryRecord,
        /// The write came from the model's `propose_memory` tool rather than a child request.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        model_proposal: bool,
    },
    QueryMemory {
        query: MemoryQuery,
        requested_k: usize,
        /// `memory(record_id)`: a point read of this record, answered with it or with nothing.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        record_id: Option<String>,
        /// The kernel's own recall (plan task 2.1), with the sprint it serves: 0 at run start.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        recall_sprint: Option<u32>,
    },
    ArchivePageOut {
        turn: u32,
        action: KernelPressureAction,
        summary: Option<String>,
        archived: Vec<CoreMessage>,
        tier: String,
        /// The kernel's opaque archive body and its digest. The host must store exactly these
        /// bytes under the ref it reports, because the kernel later `load_payload`s that ref.
        payload_content: String,
        payload_digest: String,
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
