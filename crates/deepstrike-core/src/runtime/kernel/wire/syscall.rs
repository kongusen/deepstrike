//! P1 syscall requests and their causation (spec §7.6).
//!
//! A syscall is **not** a sixth wire input class. It reaches the kernel one of exactly two ways,
//! and in both the caller is derived from kernel-owned state rather than declared by the host:
//!
//! * the current operation's provider tool call — recognised while resolving the provider effect;
//! * a child's request, attached to its `ChildCompleted` fact — the parent kernel derives the
//!   caller from the attempt.
//!
//! There is no `AgentRequest { actor_id }`, and no SDK-side path that fills an actor in.

use serde::{Deserialize, Serialize};

use super::command::TaskUpdate;
use super::root::{WorkflowNode, WorkflowSpec};
use super::scalar::{AttemptId, CallId, EffectId, HandleId, TaskId};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SyscallRequest {
    SubmitWorkflow(SubmitWorkflowRequest),
    AppendWorkflowNodes(AppendWorkflowNodesRequest),
    ActivateSkill(ActivateSkillRequest),
    /// The model's task update. Distinct from `HostCommand::UpdateTask` precisely because the
    /// two have different authority.
    UpdateTask(UpdateTaskRequest),
    RequestMemoryWrite(RequestMemoryWriteRequest),
    RequestMemoryQuery(RequestMemoryQueryRequest),
    SendMessage(SendMessageRequest),
    PublishChannel(PublishChannelRequest),
    ReceiveMailbox(ReceiveMailboxRequest),
    ReceiveChannel(ReceiveChannelRequest),
    ReadObject(ReadObjectRequest),
    /// P3 handle page-in. Opposite in meaning to the host's `SeedKnowledge` command (DEC-9).
    PageIn(PageInRequest),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubmitWorkflowRequest {
    pub spec: WorkflowSpec,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppendWorkflowNodesRequest {
    pub nodes: Vec<WorkflowNode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivateSkillRequest {
    pub name: String,
    /// Auto-deactivate after this many turns. `None` ⇒ until explicitly deactivated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_turns: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateTaskRequest {
    pub update: TaskUpdate,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestMemoryWriteRequest {
    pub proposal: MemoryWriteProposal,
}

/// A **proposal**, not a record. It carries no tenant/namespace, no record id, no author or trust
/// level, no timestamp and no session provenance: the kernel derives all of those from the
/// operation's memory binding, the envelope's accepted time and the syscall causation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryWriteProposal {
    pub name: String,
    pub kind: MemoryKind,
    pub content: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    User,
    Feedback,
    Project,
    Reference,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestMemoryQueryRequest {
    pub query: MemoryQueryProposal,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryQueryProposal {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<MemoryKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageInRequest {
    /// Must already be reachable in the current P3 handle table.
    pub handle_id: HandleId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendMessageRequest {
    pub message_id: String,
    pub to: TaskId,
    pub message_kind: String,
    pub payload_handle: HandleId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_turns: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishChannelRequest {
    pub channel_id: String,
    pub message_id: String,
    pub subscribers: Vec<TaskId>,
    pub message_kind: String,
    pub payload_handle: HandleId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_turns: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiveMailboxRequest {
    #[serde(default = "default_receive_limit")]
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiveChannelRequest {
    pub channel_id: String,
}

fn default_receive_limit() -> u32 {
    16
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadObjectRequest {
    pub object_id: u32,
}

/// How the kernel derived the caller of a syscall. Never host-supplied.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SyscallCausation {
    /// A tool call inside the provider result the kernel is currently resolving.
    ProviderTool(ProviderToolCausation),
    /// A request a child attached to its completion fact.
    ChildAttempt(ChildAttemptCausation),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderToolCausation {
    pub provider_effect_id: EffectId,
    pub call_id: CallId,
    pub task_id: TaskId,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildAttemptCausation {
    pub task_id: TaskId,
    pub attempt_id: AttemptId,
    /// Position in the child's `parent_requests` list. Derived from that stable order — a host
    /// never picks it.
    pub request_seq: u32,
}
