//! The canonical operation driver — the plan function of [`KernelTransaction`] (spec §5.4, §6, §7.4,
//! §10.1, §10.2).
//!
//! This is the layer the migration's semantic half turns on. Everything above it is contract
//! (envelope → record → transaction); everything below it is the kernel's existing semantic
//! machinery (the scheduler's [`LoopStateMachine`], the P1 syscall gate, the P2 task table, the P3
//! context VM). The driver is the **only** place the two meet, and §5.4 fixes exactly how: every
//! wire input reduces to a P1/P2/P3 primitive, and none of them is allowed to grow a parallel
//! business state machine here.
//!
//! Three properties define it:
//!
//! 1. **No protocol adapter.** The driver reads [`NormalizedPayload`] directly and reduces it to
//!    scheduler/context primitives such as `LoopStateMachine::start`, `load_workflow`,
//!    `resolve_workflow_spawn`, and `feed`. The canonical envelope is the only wire contract.
//! 2. **`RootKind` is immutable and `ExecutionFocus` moves only on a committed transition**
//!    (§6.1.5/6.1.6, §7.4). Both live in [`CanonicalOperationDriver`], and `plan` never writes
//!    them — it *stages* the next value, and [`CanonicalOperationDriver::note_committed`] is what
//!    installs it. There is no input, host command or otherwise, that sets a focus directly.
//! 3. **A root start is one atomic input.** `ConfigureOperation` builds the engine;
//!    `StartOperation` seeds the initial context, enters the root, and publishes the first effect
//!    — a provider call for an agent root, a task spawn for a workflow root — in the *same*
//!    planned step. The historical 12+ separate accepted inputs before a first provider call
//!    (§3.3 item 20) have no equivalent path here.
//! 4. **One resolution entry, one decision per failure** (§7.9). Every pending effect — provider,
//!    tools, approval, spawn, preempt, memory, page-out, milestone — is answered through
//!    `ResolveEffect` and nothing else, and a `Failed` outcome buys exactly one policy decision:
//!    abandon, switch recovery ladder, or commit a terminal. The kernel never re-emits the same
//!    intent (DEC-5), so the historical unbounded `retry_approval` / `retry_workflow_spawn` /
//!    `retry_preempt` round trips are not expressible on this path. `ContextOverflow` is not a
//!    failure at all: it is the one *semantic* provider outcome, and it feeds the compaction ladder.
//!
//! ### What `plan` may mutate
//!
//! [`KernelTransaction::prepare`] guarantees that a non-`Prepared` outcome leaves the *transaction*
//! byte-for-byte unchanged, and it can reject after the planner has already run (an unsupported
//! effect kind, a duplicate effect identity, the tail hard limit). The driver answers that in two
//! layers:
//!
//! * every refusal the driver itself owns — root authority, focus depth, an unreducible input — is
//!   decided **before** the semantic engine is touched, so it is a genuine zero-mutation rejection;
//! * the engine advance that a successful plan performs is guarded by a staging slot. A second
//!   `plan` without an intervening `note_committed` means the previous plan was discarded while the
//!   engine had already moved, and the driver fails closed with a poison fault that names the only
//!   legal recovery — rebuild from the journal (§8.3).
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::checkpoint::{
    AuthoredMemoryQueryState, AuthoredMemoryWriteState, ChildProcessState, ContextVmState,
    EntropyState, EntropyTurnState, HandleState, InlineMessageBody, KnowledgeSlotState,
    LocalChannelState, LogicalCompressionEntry, LogicalKernelState, LogicalPlanStep,
    LogicalStateProjection, LogicalTaskState, LogicalToolCall, MessagePartition, MilestoneState,
    PartitionTokenState, PendingPayloadLoadState, PendingProviderCallState, QueuedSignalState,
    ReferencedMessageBody, SchedulerState, SkillLeaseState, StoredMessageBody, StoredMessageState,
    StructuredMessageBody, SyscallState, TaskAttemptState, TaskControlState,
    TaskWaitConditionState, TaskWaitSetState, WorkflowGraphState, WorkflowNodeState,
};
use super::command::{
    AdmitMemoryWriteCommand, AppendWorkflowNodesCommand, ApplyCapabilityPatchCommand,
    ApplyKnowledgeMutationCommand, ApplyPolicyPatchCommand, ApplySkillActivationCommand,
    CancelCommand, CancellationReason, DynamicWorkflowReplayCommand, HostCommand, LivePolicyState,
    SeedKnowledgeCommand, TaskUpdate as WireTaskUpdate, UpdateDeadlineCommand, UpdateTaskCommand,
};
use super::config::ResolvedOperationConfig;
use super::effect::{
    ApprovalRequest as WireApprovalRequest, ArchivePageOutEffect, CallProviderEffect,
    CanonicalMemoryQuery, CanonicalMemoryWrite, EffectKind, EffectKindTag, EffectOutcome,
    EffectSuccess, EvaluateMilestoneEffect, ExecuteToolsEffect, HostEffectFailure, KernelEffect,
    LaunchToken, LoadPayloadEffect, PageOutPayload, PayloadRef, PersistMemoryEffect,
    PreemptTasksEffect, ProviderCompleted, ProviderMessage, ProviderOutcome, QueryMemoryEffect,
    RenderedContext as WireRenderedContext, RequestApprovalEffect, SpawnTasksEffect,
    TaskAttemptRef, TaskLaunch, ToolCall as WireToolCall, ToolResultDisposition,
    ToolResultPayload as WireToolResultPayload, ToolSchema as WireToolSchema,
    WorkflowBudget as WireWorkflowBudget,
};
use super::envelope::{OperationLifecycle, ResolveEffect};
use super::event::{
    ChildCompleted, ChildStatus, DeliverSignal, ExternalEvent, LogicalSignal, SignalSourceKind,
    SignalTarget, SignalUrgency,
};
use super::fault::{KernelFault, KernelFaultCode};
use super::record::NormalizedPayload;
use super::root::{
    AgentIsolation as WireIsolation, AgentRole as WireRole, ExecutionFocus, InitialContext,
    LogicalAgentSpec, LogicalContextInheritance as WireContextInheritance, LogicalTask,
    MessageRole, RootEntry, RootKind, WorkflowNode as WireNode, WorkflowSpec as WireSpec,
};
use super::scalar::{
    AttemptId, EffectId, MemoryBindingId, NodeId, OperationId, TaskId, WireU64, WorkflowId,
};
use super::syscall::{
    ChildAttemptCausation, MemoryKind as WireMemoryKind, ProviderToolCausation, SyscallCausation,
    SyscallRequest,
};
use super::terminal::{
    AgentTerminal, CancelledTerminal, EffectsDisposition, FailedTerminal, KernelFailure,
    KernelFailureCode, KernelTerminal, LoopResult as WireLoopResult, StepDisposition,
    TerminalDisposition, TerminationReason as WireTermination, UsageReport, WorkflowOutcome,
    WorkflowStatus, WorkflowTerminal,
};
use super::transaction::{PlanContext, TransitionStep};

use crate::context::manager::READ_RESULT_TOOL_NAME;
use crate::context::task_state::{CompressionEntry, PlanStep, TaskState};
use crate::mm::handle::{Handle, HandleKind, Residency};
use crate::orchestration::task_graph::TaskStatus;
use crate::orchestration::workflow::run::{WorkflowNodeStatus, WorkflowRuntimeNodeState};
use crate::orchestration::workflow::{
    WorkflowNode as CoreWorkflowNode, WorkflowSpec as CoreWorkflowSpec,
};
use crate::runtime::kernel::{KernelObservation, WorkflowSpawnFailure};
use crate::scheduler::policy::SchedulerBudget;
use crate::scheduler::state_machine::{
    AdjudicatedTurn, AnsweredCall, IdleContinuation, LoopAction, LoopEvent, LoopStateMachine,
};
use crate::scheduler::tcb::{
    ApprovalId, BudgetLedger, ChannelId, DurableWaitSet, LogicalDeadline, ProcInfo, ResourceKey,
    SignalFilter, SubscriptionId, TaskLifecycle, Tcb, WaitCondition, WaitMode,
};
use crate::scheduler::wait_index::WaitKey;
use crate::signals::queue::QueuedSignalRuntimeState;
use crate::signals::router::SignalRouterRuntimeState;
use crate::syscall::{Disposition, Syscall as CoreSyscall};
use crate::types::agent::{
    AgentCapabilityFilter, AgentIdentity, AgentIsolation, AgentRole, AgentRunSpec,
    ContextInheritance, LoopRoundSpec,
};
use crate::types::durable_content::{
    DurableContent, DurableContentBlock, DurableSource, DurableToolResult,
};
use crate::types::message::{Content, ContentPart, CoreMessage, Role, ToolErrorKind, ToolResult};
use crate::types::result::{
    LoopResult, PaceAction as CorePaceAction, SubAgentResult, TerminationReason,
};
use crate::types::signal::{RuntimeSignal, SignalSource, SignalType, Urgency};
use crate::types::task::{RuntimeTask, TaskLane};

// ---------------------------------------------------------------------------------------------
// the planned step
// ---------------------------------------------------------------------------------------------

/// One planned transition, as the canonical driver produces it.
///
/// The record freezes only this value's **digest** (§22.12), so its shape is what a rebuild has to
/// reproduce bit-for-bit. Three fields, each load-bearing:
///
/// * `root_kind` — the operation's immutable root class *after* this step. Present from the root
///   start onward and never different from the value the start committed;
/// * `focus` — the execution focus after this step. Because it is inside the digest, a focus that
///   moved differently on a replay is a `RecordCorrupted` rebuild failure rather than a silent
///   divergence;
/// * `observations` — facts produced by this exact transition, published only after commit;
/// * `disposition` — effects **or** a terminal, never both (§7.12).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedStep {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_kind: Option<RootKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus: Option<ExecutionFocus>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub observations: Vec<KernelObservation>,
    pub disposition: StepDisposition,
}

impl PartialEq for PlannedStep {
    fn eq(&self, other: &Self) -> bool {
        self.root_kind == other.root_kind
            && self.focus == other.focus
            && self.disposition == other.disposition
            && serde_json::to_vec(&self.observations).ok()
                == serde_json::to_vec(&other.observations).ok()
    }
}

impl PlannedStep {
    fn quiet(root_kind: Option<RootKind>, focus: Option<ExecutionFocus>) -> Self {
        Self {
            root_kind,
            focus,
            observations: Vec::new(),
            disposition: StepDisposition::Effects(EffectsDisposition::default()),
        }
    }
}

impl TransitionStep for PlannedStep {
    fn disposition(&self) -> &StepDisposition {
        &self.disposition
    }
}

// ---------------------------------------------------------------------------------------------
// the driver
// ---------------------------------------------------------------------------------------------

/// The internal id of the root agent task. The kernel's task table has used it since M1d; the
/// canonical `TaskId` is the same string so an `AgentTurn` focus names a row that exists.
pub const ROOT_TASK_ID: &str = "root";

/// The empty session slot used when projecting the logical kernel spec into `AgentRunSpec`.
/// Host session identity is not a kernel fact and the canonical wire has no field for one.
const NO_HOST_SESSION: &str = "";

#[derive(Debug, Clone, PartialEq)]
struct StagedFocus {
    step_seq: WireU64,
    root_kind: Option<RootKind>,
    focus: Option<ExecutionFocus>,
}

/// What the kernel published on one provider call, kept so a `ProviderTool` causation can be
/// *derived* rather than believed (§7.6).
///
/// Two facts, both kernel-owned:
///
/// * `task_id` — the task whose turn issued the call. This is the caller a syscall inherits; no
///   host names it;
/// * `exposed_tools` — exactly the surface that turn advertised. A tool call naming anything else
///   has no causation to derive from, so it is refused rather than adjudicated.
#[derive(Debug, Clone, PartialEq)]
struct PendingProviderCall {
    task_id: TaskId,
    exposed_tools: BTreeSet<String>,
}

/// A syscall the driver refused *after* its caller was established — a malformed request, a gate
/// denial, a quarantined caller, a memory binding the operation does not hold.
///
/// Deliberately not a [`KernelFault`]. §7.7's GAP-4 and the §7.6 fixture both require that a
/// refused request leaves an **audit fact** and no derived action, while the transition it arrived
/// on still commits: a child's execution is not undone because the parent denied one of the
/// requests it attached, and a model's bad argument is not a host protocol violation.
#[derive(Debug, Clone, PartialEq)]
struct SyscallRejection {
    operation: &'static str,
    /// The derived caller. Present on every rejection raised after causation succeeded, which is
    /// all of them — an audit fact that cannot name who asked is not an audit fact.
    subject: Option<String>,
    reason: String,
}

impl SyscallRejection {
    fn new(operation: &'static str, reason: impl Into<String>) -> Self {
        Self {
            operation,
            subject: None,
            reason: reason.into(),
        }
    }

    fn by(mut self, caller: &TaskId) -> Self {
        self.subject = Some(caller.as_str().to_string());
        self
    }
}

/// The two ways a syscall can fail, kept apart on purpose (§7.13 vs §7.7 GAP-4).
#[derive(Debug, Clone)]
enum SyscallRefusal {
    /// **Who** could not be established, or the transition itself is inadmissible. Zero mutation,
    /// and the whole input is refused.
    Fault(KernelFault),
    /// **What** was asked is refused. The caller was established, so the answer is an audit fact
    /// the model reads on its next turn; the transition still commits.
    Rejected(SyscallRejection),
}

/// Stable wire label of a handle kind, for the §12.1 context-VM projection.
///
/// Written out rather than derived from the internal enum's serde rename: the checkpoint's
/// vocabulary is a contract, and a rename inside `mm::handle` must break this match arm instead of
/// silently changing what a stored checkpoint means.
fn handle_kind_label(kind: &HandleKind) -> &'static str {
    match kind {
        HandleKind::ToolResult => "tool_result",
        HandleKind::MemoryPage => "memory_page",
        HandleKind::KnowledgeEntry => "knowledge_entry",
        HandleKind::SubAgentJoin => "sub_agent_join",
    }
}

fn role_label(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn role_from_label(label: &str) -> Option<Role> {
    match label {
        "system" => Some(Role::System),
        "user" => Some(Role::User),
        "assistant" => Some(Role::Assistant),
        "tool" => Some(Role::Tool),
        _ => None,
    }
}

/// Reduce a stored message to `(text, tool_call_id, is_error)`, or report that it does not reduce.
///
/// `None` means "this body is multimodal": it carries an image or audio part, and flattening it to
/// the text parts beside it would drop content a restore could never get back. Those travel as
/// [`StoredMessageBody::Structured`] instead.
#[allow(clippy::type_complexity)]
fn message_body_parts(message: &CoreMessage) -> Option<(String, Option<String>, bool)> {
    match &message.content {
        Content::Text(text) => Some((text.clone(), None, false)),
        Content::Parts(parts) => {
            let mut text = String::new();
            let mut tool_call_id = None;
            let mut is_error = false;
            for part in parts {
                match part {
                    ContentPart::Text { text: chunk } => text.push_str(chunk),
                    ContentPart::ToolResult {
                        call_id,
                        output,
                        is_error: failed,
                        durable_content,
                    } => {
                        if tool_call_id.is_some() {
                            // Two results in one message have two call ids; the pair projection
                            // holds one. Carry the whole content instead of picking a winner.
                            return None;
                        }
                        tool_call_id = Some(call_id.to_string());
                        if durable_content.is_some() {
                            // The correlated durable envelope must survive intact, never be
                            // reduced to its text projection.
                            return None;
                        }
                        text.push_str(output);
                        is_error = *failed;
                    }
                    ContentPart::Image { .. } | ContentPart::Audio { .. } => return None,
                }
            }
            Some((text, tool_call_id, is_error))
        }
    }
}

/// Rebuild the stored content a [`StoredMessageBody`] describes.
///
/// The exact inverse of [`message_body_parts`]: a body with a `tool_call_id` was a single-part tool
/// result and goes back as one, everything else was flat text.
fn message_content(text: String, tool_call_id: Option<&str>, is_error: bool) -> Content {
    match tool_call_id {
        Some(call_id) => Content::Parts(vec![ContentPart::ToolResult {
            call_id: call_id.into(),
            output: text,
            is_error,
            durable_content: None,
        }]),
        None => Content::Text(text),
    }
}

fn content_to_durable(content: &Content) -> Result<DurableContent, String> {
    let blocks = match content {
        Content::Text(text) => vec![DurableContentBlock::Text { text: text.clone() }],
        Content::Parts(parts) => parts
            .iter()
            .map(content_part_to_durable)
            .collect::<Result<Vec<_>, _>>()?,
    };
    let content = DurableContent { blocks };
    content.validate().map_err(|error| error.to_string())?;
    Ok(content)
}

fn content_part_to_durable(part: &ContentPart) -> Result<DurableContentBlock, String> {
    match part {
        ContentPart::Text { text } => Ok(DurableContentBlock::Text { text: text.clone() }),
        ContentPart::ToolResult { .. } => Err(
            "a structured message cannot embed a tool result; durable tool results use their separate envelope".into(),
        ),
        ContentPart::Image { source, media_type, detail } => {
            let provider_options = detail
                .as_ref()
                .map(|detail| serde_json::json!({ "detail": detail }));
            Ok(DurableContentBlock::Image {
                source: source.clone(),
                media_type: media_type.clone(),
                provider_options,
            })
        }
        ContentPart::Audio { source, media_type } => Ok(DurableContentBlock::Audio {
            source: source.clone(),
            media_type: Some(media_type.clone()),
            provider_options: None,
        }),
    }
}

fn durable_tool_result_from_content(content: &Content) -> Option<DurableToolResult> {
    let Content::Parts(parts) = content else {
        return None;
    };
    let [
        ContentPart::ToolResult {
            call_id,
            is_error,
            output,
            durable_content,
        },
    ] = parts.as_slice()
    else {
        return None;
    };
    Some(durable_tool_result_from_part(
        call_id,
        output,
        *is_error,
        durable_content.as_ref(),
    ))
}

fn durable_tool_results_from_content(content: &Content) -> Option<Vec<DurableToolResult>> {
    let Content::Parts(parts) = content else {
        return None;
    };
    if parts.len() < 2 {
        return None;
    }
    let results = parts
        .iter()
        .map(|part| match part {
            ContentPart::ToolResult {
                call_id,
                output,
                is_error,
                durable_content,
            } => Some(durable_tool_result_from_part(
                call_id,
                output,
                *is_error,
                durable_content.as_ref(),
            )),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    Some(results)
}

fn durable_tool_result_from_part(
    call_id: &str,
    output: &str,
    is_error: bool,
    durable_content: Option<&DurableContent>,
) -> DurableToolResult {
    match durable_content {
        Some(content) => DurableToolResult {
            call_id: call_id.to_owned(),
            is_error,
            blocks: content.blocks.clone(),
        },
        None => DurableToolResult::text(call_id.to_owned(), output.to_owned(), is_error),
    }
}

fn content_from_durable_tool_result(result: &DurableToolResult) -> Result<Content, String> {
    result.validate().map_err(|error| error.to_string())?;
    let output = result
        .blocks
        .iter()
        .filter_map(|block| match block {
            DurableContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    Ok(Content::Parts(vec![ContentPart::ToolResult {
        call_id: result.call_id.clone().into(),
        output,
        is_error: result.is_error,
        durable_content: Some(DurableContent {
            blocks: result.blocks.clone(),
        }),
    }]))
}

fn content_from_durable_tool_results(results: &[DurableToolResult]) -> Result<Content, String> {
    let mut parts = Vec::with_capacity(results.len());
    for result in results {
        let Content::Parts(mut result_parts) = content_from_durable_tool_result(result)? else {
            return Err("durable tool result did not restore to tool content".into());
        };
        parts.append(&mut result_parts);
    }
    Ok(Content::Parts(parts))
}

fn content_from_durable(content: &DurableContent) -> Result<Content, String> {
    let parts = content
        .blocks
        .iter()
        .map(durable_block_to_content_part)
        .collect::<Result<Vec<_>, _>>()?;
    if parts.len() == 1 {
        if let ContentPart::Text { text } = &parts[0] {
            return Ok(Content::Text(text.clone()));
        }
    }
    Ok(Content::Parts(parts))
}

fn durable_block_to_content_part(block: &DurableContentBlock) -> Result<ContentPart, String> {
    match block {
        DurableContentBlock::Text { text } => Ok(ContentPart::Text { text: text.clone() }),
        DurableContentBlock::Image {
            source,
            media_type,
            provider_options,
        } => match source {
            DurableSource::Url { url } => Ok(ContentPart::Image {
                source: DurableSource::Url { url: url.clone() },
                media_type: media_type.clone(),
                detail: provider_options
                    .as_ref()
                    .and_then(|value| value.get("detail"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
            }),
            DurableSource::Base64 { data } => Ok(ContentPart::Image {
                source: DurableSource::Base64 { data: data.clone() },
                media_type: media_type.clone(),
                detail: provider_options
                    .as_ref()
                    .and_then(|value| value.get("detail"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
            }),
            _ => Err("this kernel only restores image url/base64 sources".into()),
        },
        DurableContentBlock::Audio {
            source: DurableSource::Base64 { data },
            media_type,
            ..
        } => Ok(ContentPart::Audio {
            source: DurableSource::Base64 { data: data.clone() },
            media_type: media_type
                .clone()
                .ok_or_else(|| "audio durable block requires media_type".to_string())?,
        }),
        DurableContentBlock::Audio { .. }
        | DurableContentBlock::File { .. }
        | DurableContentBlock::Video { .. } => {
            Err("this kernel content vocabulary cannot restore the durable media source".into())
        }
    }
}

fn workflow_kind_label(state: &WorkflowRuntimeNodeState) -> &'static str {
    match state.node.kind {
        crate::orchestration::workflow::NodeKind::Spawn => "spawn",
        crate::orchestration::workflow::NodeKind::Loop { .. } => "loop",
        crate::orchestration::workflow::NodeKind::Classify { .. } => "classify",
        crate::orchestration::workflow::NodeKind::Tournament { .. } => "tournament",
        crate::orchestration::workflow::NodeKind::Reduce { .. } => "reduce",
    }
}

fn workflow_status_label(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "pending",
        TaskStatus::Ready => "ready",
        TaskStatus::Running => "running",
        TaskStatus::Completed => "completed",
        TaskStatus::CompletedPartial => "completed_partial",
        TaskStatus::Failed => "failed",
        TaskStatus::SkippedUpstreamFailed => "skipped_upstream_failed",
    }
}

fn restore_workflow_status(label: &str) -> Result<TaskStatus, KernelFault> {
    match label {
        "pending" => Ok(TaskStatus::Pending),
        "ready" => Ok(TaskStatus::Ready),
        "running" => Ok(TaskStatus::Running),
        "completed" => Ok(TaskStatus::Completed),
        "completed_partial" => Ok(TaskStatus::CompletedPartial),
        "failed" => Ok(TaskStatus::Failed),
        "skipped_upstream_failed" => Ok(TaskStatus::SkippedUpstreamFailed),
        other => Err(KernelFault::new(
            KernelFaultCode::CheckpointIncompatible,
            format!("workflow checkpoint carries unknown node status {other:?}"),
        )),
    }
}

fn agent_role_label(role: AgentRole) -> &'static str {
    match role {
        AgentRole::Explore => "explore",
        AgentRole::Plan => "plan",
        AgentRole::Implement => "implement",
        AgentRole::Verify => "verify",
        AgentRole::Custom => "custom",
    }
}

fn restore_agent_role(label: &str) -> Result<AgentRole, KernelFault> {
    match label {
        "explore" => Ok(AgentRole::Explore),
        "plan" => Ok(AgentRole::Plan),
        "implement" => Ok(AgentRole::Implement),
        "verify" => Ok(AgentRole::Verify),
        "custom" => Ok(AgentRole::Custom),
        other => Err(KernelFault::new(
            KernelFaultCode::CheckpointIncompatible,
            format!("child process carries unknown role {other:?}"),
        )),
    }
}

fn agent_isolation_label(isolation: AgentIsolation) -> &'static str {
    match isolation {
        AgentIsolation::Shared => "shared",
        AgentIsolation::ReadOnly => "read_only",
        AgentIsolation::Worktree => "worktree",
        AgentIsolation::Remote => "remote",
    }
}

fn restore_agent_isolation(label: &str) -> Result<AgentIsolation, KernelFault> {
    match label {
        "shared" => Ok(AgentIsolation::Shared),
        "read_only" => Ok(AgentIsolation::ReadOnly),
        "worktree" => Ok(AgentIsolation::Worktree),
        "remote" => Ok(AgentIsolation::Remote),
        other => Err(KernelFault::new(
            KernelFaultCode::CheckpointIncompatible,
            format!("child process carries unknown isolation {other:?}"),
        )),
    }
}

fn context_inheritance_label(inheritance: ContextInheritance) -> &'static str {
    match inheritance {
        ContextInheritance::None => "none",
        ContextInheritance::SystemOnly => "system_only",
        ContextInheritance::Full => "full",
    }
}

fn restore_context_inheritance(label: &str) -> Result<ContextInheritance, KernelFault> {
    match label {
        "none" => Ok(ContextInheritance::None),
        "system_only" => Ok(ContextInheritance::SystemOnly),
        "full" => Ok(ContextInheritance::Full),
        other => Err(KernelFault::new(
            KernelFaultCode::CheckpointIncompatible,
            format!("child process carries unknown context inheritance {other:?}"),
        )),
    }
}

fn queued_signal_state(queued: &QueuedSignalRuntimeState) -> QueuedSignalState {
    let signal = &queued.signal;
    QueuedSignalState {
        signal_id: super::scalar::SignalId::new(signal.id.as_str())
            .expect("a canonical runtime signal keeps its branded id"),
        source: signal_source_label(signal.source).to_string(),
        signal_type: signal_type_label(signal.signal_type).to_string(),
        urgency: urgency_label(signal.urgency).to_string(),
        summary: signal.summary.to_string(),
        payload: super::scalar::BoundedJson::new(signal.payload.clone())
            .expect("a canonical signal payload remains bounded"),
        dedupe_key: signal.dedupe_key.as_ref().map(ToString::to_string),
        deadline_ms: signal.deadline_ms.map(WireU64::new),
        coalesce_key: signal.coalesce_key.as_ref().map(ToString::to_string),
        coalesced_count: signal.coalesced_count,
        recipient: signal.recipient.as_ref().map(ToString::to_string),
        timestamp_ms: WireU64::new(signal.timestamp_ms),
        deadline_escalated: queued.deadline_escalated,
        dedupe_keys: queued.dedupe_keys.iter().map(ToString::to_string).collect(),
    }
}

fn restore_queued_signal(
    queued: &QueuedSignalState,
) -> Result<QueuedSignalRuntimeState, KernelFault> {
    if queued.coalesced_count == 0 {
        return Err(KernelFault::new(
            KernelFaultCode::CheckpointIncompatible,
            format!(
                "queued signal {} carries a zero coalesced count",
                queued.signal_id
            ),
        ));
    }
    Ok(QueuedSignalRuntimeState {
        signal: RuntimeSignal {
            id: queued.signal_id.as_str().into(),
            source: restore_signal_source(&queued.source)?,
            signal_type: restore_signal_type(&queued.signal_type)?,
            urgency: restore_urgency(&queued.urgency)?,
            summary: queued.summary.as_str().into(),
            payload: queued.payload.get().clone(),
            dedupe_key: queued.dedupe_key.as_deref().map(Into::into),
            deadline_ms: queued.deadline_ms.map(WireU64::get),
            coalesce_key: queued.coalesce_key.as_deref().map(Into::into),
            coalesced_count: queued.coalesced_count,
            recipient: queued.recipient.as_deref().map(Into::into),
            timestamp_ms: queued.timestamp_ms.get(),
        },
        deadline_escalated: queued.deadline_escalated,
        dedupe_keys: queued
            .dedupe_keys
            .iter()
            .map(|key| key.as_str().into())
            .collect(),
    })
}

fn signal_source_label(source: SignalSource) -> &'static str {
    match source {
        SignalSource::Cron => "cron",
        SignalSource::Gateway => "gateway",
        SignalSource::Heartbeat => "heartbeat",
        SignalSource::Custom => "custom",
    }
}

fn restore_signal_source(label: &str) -> Result<SignalSource, KernelFault> {
    match label {
        "cron" => Ok(SignalSource::Cron),
        "gateway" => Ok(SignalSource::Gateway),
        "heartbeat" => Ok(SignalSource::Heartbeat),
        "custom" => Ok(SignalSource::Custom),
        other => Err(KernelFault::new(
            KernelFaultCode::CheckpointIncompatible,
            format!("queued signal carries unknown source {other:?}"),
        )),
    }
}

fn signal_type_label(signal_type: SignalType) -> &'static str {
    match signal_type {
        SignalType::Event => "event",
        SignalType::Job => "job",
        SignalType::Alert => "alert",
    }
}

fn restore_signal_type(label: &str) -> Result<SignalType, KernelFault> {
    match label {
        "event" => Ok(SignalType::Event),
        "job" => Ok(SignalType::Job),
        "alert" => Ok(SignalType::Alert),
        other => Err(KernelFault::new(
            KernelFaultCode::CheckpointIncompatible,
            format!("queued signal carries unknown type {other:?}"),
        )),
    }
}

fn urgency_label(urgency: Urgency) -> &'static str {
    match urgency {
        Urgency::Low => "low",
        Urgency::Normal => "normal",
        Urgency::High => "high",
        Urgency::Critical => "critical",
    }
}

fn restore_urgency(label: &str) -> Result<Urgency, KernelFault> {
    match label {
        "low" => Ok(Urgency::Low),
        "normal" => Ok(Urgency::Normal),
        "high" => Ok(Urgency::High),
        "critical" => Ok(Urgency::Critical),
        other => Err(KernelFault::new(
            KernelFaultCode::CheckpointIncompatible,
            format!("queued signal carries unknown urgency {other:?}"),
        )),
    }
}

/// §12.2 · put the scheduler partition back on the engine.
fn restore_scheduler(
    engine: &mut LoopStateMachine,
    config: &ResolvedOperationConfig,
    state: &SchedulerState,
) -> Result<(), KernelFault> {
    engine.run_spec = state.run_spec.as_ref().map(agent_run_spec);
    if let Some(names) = &state.advertised_tool_ids {
        let unique: std::collections::BTreeSet<&str> = names.iter().map(String::as_str).collect();
        if unique.len() != names.len() {
            return Err(KernelFault::new(
                KernelFaultCode::CheckpointIncompatible,
                "advertised_tool_ids contains a duplicate tool id",
            ));
        }
    }
    engine.restore_advertised_tool_ids(state.advertised_tool_ids.clone());
    engine.turn = state.turn;
    engine.restore_budget_usage(state.total_tokens.get(), state.rounds_completed);
    engine.restore_started_at_ms(state.started_at_ms.map(WireU64::get));
    engine.set_wall_budget(state.wall_budget_ms.map(WireU64::get));
    engine
        .restore_entropy_checkpoint_state(crate::scheduler::entropy::EntropyTrackerRuntimeState {
            window: state
                .entropy
                .window
                .iter()
                .map(|entry| crate::scheduler::entropy::EntropyTurnRuntimeState {
                    errored_results: entry.errored_results,
                    total_results: entry.total_results,
                    rollbacks: entry.rollbacks,
                })
                .collect(),
            rollbacks_pending: state.entropy.rollbacks_pending,
            disarmed: state.entropy.disarmed,
            last_alert_turn: state.entropy.last_alert_turn,
        })
        .map_err(|error| {
            KernelFault::new(
                KernelFaultCode::CheckpointIncompatible,
                format!("entropy checkpoint could not be rebuilt: {error}"),
            )
        })?;

    let limits = SchedulerBudget {
        max_tokens: config.execution_policy.max_context_tokens,
        max_turns: config.execution_policy.max_turns,
        max_total_tokens: config.execution_policy.max_total_tokens.get(),
        max_wall_ms: state.wall_budget_ms.map(WireU64::get),
    };
    let table = engine.task_table_mut();
    for task in &state.tasks {
        let mut tcb = Tcb::root(task.task_id.as_str(), limits.clone());
        tcb.parent = task
            .parent_task_id
            .as_ref()
            .map(|parent| parent.as_str().into());
        tcb.state = restore_task_lifecycle(task)?;
        tcb.runnable_cause = task.runnable_cause;
        tcb.wait_set = task
            .wait_set
            .as_ref()
            .map(|wait_set| restore_wait_set(&task.task_id, wait_set))
            .transpose()?;
        tcb.caps = task.capability_ids.iter().map(|cap| cap.into()).collect();
        tcb.capabilities = task.capabilities.clone();
        tcb.supervision = task.supervision.clone();
        tcb.supervision_events = task.supervision_events.clone();
        // spc_009-06: restore this task's own checkpointed pool verbatim — never re-derive it from
        // `state.budget_grant` (the `set_budget_grant` call above only restores the whole-operation
        // admission grant for reporting; re-seeding from it here would silently undo every debit a
        // spawn made before this checkpoint was taken).
        tcb.child_budget_remaining = task.child_budget_remaining;
        tcb.budget_grant = task.budget_grant.clone();
        tcb.mailbox = task.mailbox.clone();
        if let Some(grant) = tcb.budget_grant.as_ref()
            && (grant.child.as_str() != task.task_id.as_str()
                || tcb.parent.as_deref() != Some(grant.parent.as_str()))
        {
            return Err(KernelFault::new(
                KernelFaultCode::CheckpointIncompatible,
                format!(
                    "task {} carries a hierarchical budget grant for parent {} and child {}",
                    task.task_id, grant.parent, grant.child
                ),
            ));
        }
        tcb.proc = task
            .process
            .as_ref()
            .map(|process| {
                let result = process
                    .join_result
                    .as_ref()
                    .map(|value| {
                        serde_json::from_value(value.get().clone()).map_err(|error| {
                            KernelFault::new(
                                KernelFaultCode::CheckpointIncompatible,
                                format!(
                                    "task {} carries an invalid child join result: {error}",
                                    task.task_id
                                ),
                            )
                        })
                    })
                    .transpose()?;
                if result.as_ref().is_some_and(|result: &SubAgentResult| {
                    result.agent_id.as_str() != task.task_id.as_str()
                }) {
                    return Err(KernelFault::new(
                        KernelFaultCode::CheckpointIncompatible,
                        format!(
                            "task {} carries a join result for another child",
                            task.task_id
                        ),
                    ));
                }
                Ok(ProcInfo {
                    role: restore_agent_role(&process.role)?,
                    isolation: restore_agent_isolation(&process.isolation)?,
                    context_inheritance: restore_context_inheritance(&process.context_inheritance)?,
                    result,
                })
            })
            .transpose()?;
        tcb.budget = BudgetLedger {
            limits: limits.clone(),
            turns: task.turns_used,
            total_tokens: task.tokens_used.get(),
            started_at_ms: state.started_at_ms.map(WireU64::get),
        };
        table.insert(tcb);
    }
    let mut restored_channels = BTreeMap::new();
    for channel in &state.channels {
        let id = ChannelId(channel.channel_id.as_str().into());
        if restored_channels
            .insert(id, channel.channel.clone())
            .is_some()
        {
            return Err(KernelFault::new(
                KernelFaultCode::CheckpointIncompatible,
                format!("duplicate local channel {:?}", channel.channel_id),
            ));
        }
    }
    table.restore_channels(restored_channels);
    let mut restored_objects = BTreeMap::new();
    for object in &state.objects {
        if table.get(object.owner.as_str()).is_none() {
            return Err(KernelFault::new(
                KernelFaultCode::CheckpointIncompatible,
                format!("object {} names unknown owner {}", object.id, object.owner),
            ));
        }
        if restored_objects.insert(object.id, object.clone()).is_some() {
            return Err(KernelFault::new(
                KernelFaultCode::CheckpointIncompatible,
                format!("duplicate local object {}", object.id),
            ));
        }
    }
    table.restore_objects(restored_objects);
    // spc_002-09: `children` is not on the wire (derivable from `parent`); `insert` above only
    // registers a child when its parent row already exists, which the wire's task order does not
    // guarantee. Recompute from the now-complete `parent` links rather than trust insertion order.
    table.rebuild_children();
    // WaitIndex is derived state. Recompute it from every task's restored durable wait set.
    table.rebuild_wait_index();

    let queued = state
        .queued_signals
        .iter()
        .map(restore_queued_signal)
        .collect::<Result<Vec<_>, _>>()?;
    engine
        .restore_signal_checkpoint_state(SignalRouterRuntimeState {
            queued,
            seen_order: state
                .signal_dedupe_keys
                .iter()
                .map(|key| key.as_str().into())
                .collect(),
        })
        .map_err(|error| {
            KernelFault::new(
                KernelFaultCode::CheckpointIncompatible,
                format!("signal checkpoint could not be rebuilt: {error}"),
            )
        })?;

    if let Some(workflow) = &state.workflow {
        let wire_spec = WireSpec {
            name: String::new(),
            nodes: workflow
                .nodes
                .iter()
                .map(|node| WireNode {
                    node_id: node.node_id.clone(),
                    task: node.task.clone(),
                    depends_on: node.depends_on.clone(),
                    run_spec: node.run_spec.clone(),
                })
                .collect(),
        };
        let core_spec = build_core_spec(&wire_spec).map_err(|fault| {
            KernelFault::new(KernelFaultCode::CheckpointIncompatible, fault.message)
        })?;
        let runtime_states: Result<Vec<_>, KernelFault> = workflow
            .nodes
            .iter()
            .enumerate()
            .zip(core_spec.nodes.iter())
            .map(|((index, node), core)| {
                if node.kind != "spawn" {
                    return Err(KernelFault::new(
                        KernelFaultCode::CheckpointIncompatible,
                        format!(
                            "workflow node {} carries unsupported checkpoint kind {:?}",
                            node.node_id, node.kind
                        ),
                    ));
                }
                let result = engine
                    .task_table()
                    .get(&crate::orchestration::workflow::node_agent_id(index))
                    .and_then(|task| task.proc.as_ref())
                    .and_then(|process| process.result.as_ref())
                    .map(|result| result.result.clone());
                Ok(WorkflowRuntimeNodeState {
                    node: core.clone(),
                    status: restore_workflow_status(&node.status)?,
                    result,
                    active_agent_id: node.active_agent_id.clone(),
                    iterations_completed: node.iterations_completed as usize,
                })
            })
            .collect();
        let run = crate::orchestration::workflow::WorkflowRun::restore_from_checkpoint(
            &core_spec,
            &runtime_states?,
        )
        .map_err(|error| {
            KernelFault::new(
                KernelFaultCode::CheckpointIncompatible,
                format!("workflow checkpoint could not be rebuilt: {error}"),
            )
        })?;
        engine.restore_checkpoint_workflow(run);
    }
    Ok(())
}

fn restore_task_lifecycle(task: &TaskControlState) -> Result<TaskLifecycle, KernelFault> {
    let lifecycle = match task.lifecycle.as_str() {
        "pending_launch" => TaskLifecycle::PendingLaunch,
        "starting" => TaskLifecycle::Starting,
        "ready" => TaskLifecycle::Ready,
        "running" => TaskLifecycle::Running,
        "suspended" => TaskLifecycle::Suspended,
        "done" => {
            let label = task.termination.as_deref().ok_or_else(|| {
                incompatible(format!(
                    "task {} is done but the checkpoint does not say why; a finished task without \
                     its termination reason is not restorable",
                    task.task_id
                ))
            })?;
            TaskLifecycle::Done(termination_from_label(label).ok_or_else(|| {
                incompatible(format!(
                    "task {} names termination reason {label:?}, which this kernel does not know",
                    task.task_id
                ))
            })?)
        }
        other => {
            return Err(incompatible(format!(
                "task {} names lifecycle {other:?}, which this kernel does not know",
                task.task_id
            )));
        }
    };
    Ok(lifecycle)
}

fn project_wait_set(wait_set: &DurableWaitSet) -> TaskWaitSetState {
    TaskWaitSetState {
        mode: match wait_set.mode {
            WaitMode::Any => "any",
            WaitMode::All => "all",
        }
        .to_string(),
        conditions: wait_set
            .conditions
            .iter()
            .map(|condition| match condition {
                WaitCondition::Effect(effect_id) => TaskWaitConditionState::Effect {
                    effect_id: effect_id.clone(),
                },
                WaitCondition::Child(task_id) => TaskWaitConditionState::Child {
                    task_id: TaskId::new(task_id.as_str())
                        .expect("an internal task id is a legal branded ref"),
                },
                WaitCondition::Children(task_ids) => TaskWaitConditionState::Children {
                    task_ids: task_ids
                        .iter()
                        .map(|task_id| {
                            TaskId::new(task_id.as_str())
                                .expect("an internal task id is a legal branded ref")
                        })
                        .collect(),
                },
                WaitCondition::Approval(ApprovalId(id)) => TaskWaitConditionState::Approval {
                    approval_id: id.to_string(),
                },
                WaitCondition::Signal(SignalFilter(filter)) => TaskWaitConditionState::Signal {
                    filter: filter.to_string(),
                },
                WaitCondition::Timer(LogicalDeadline(deadline_ms)) => {
                    TaskWaitConditionState::Timer {
                        deadline_ms: WireU64::new(*deadline_ms),
                    }
                }
                WaitCondition::Channel(ChannelId(id)) => TaskWaitConditionState::Channel {
                    channel_id: id.to_string(),
                },
                WaitCondition::Resource(ResourceKey(key)) => TaskWaitConditionState::Resource {
                    resource_key: key.to_string(),
                },
                WaitCondition::External(SubscriptionId(id)) => TaskWaitConditionState::External {
                    subscription_id: id.to_string(),
                },
            })
            .collect(),
        satisfied: wait_set
            .satisfied
            .iter()
            .map(|index| *index as u32)
            .collect(),
    }
}

fn restore_wait_set(
    task_id: &TaskId,
    state: &TaskWaitSetState,
) -> Result<DurableWaitSet, KernelFault> {
    let mode = match state.mode.as_str() {
        "any" => WaitMode::Any,
        "all" => WaitMode::All,
        other => {
            return Err(incompatible(format!(
                "task {task_id} wait set names mode {other:?}, which this kernel does not know"
            )));
        }
    };
    if state.conditions.is_empty() {
        return Err(incompatible(format!(
            "task {task_id} carries an empty durable WaitSet"
        )));
    }
    let conditions = state
        .conditions
        .iter()
        .map(|condition| match condition {
            TaskWaitConditionState::Effect { effect_id } => {
                WaitCondition::Effect(effect_id.clone())
            }
            TaskWaitConditionState::Child { task_id } => {
                WaitCondition::Child(task_id.as_str().into())
            }
            TaskWaitConditionState::Children { task_ids } => WaitCondition::Children(
                task_ids
                    .iter()
                    .map(|task_id| task_id.as_str().into())
                    .collect(),
            ),
            TaskWaitConditionState::Approval { approval_id } => {
                WaitCondition::Approval(ApprovalId(approval_id.as_str().into()))
            }
            TaskWaitConditionState::Signal { filter } => {
                WaitCondition::Signal(SignalFilter(filter.as_str().into()))
            }
            TaskWaitConditionState::Timer { deadline_ms } => {
                WaitCondition::Timer(LogicalDeadline(deadline_ms.get()))
            }
            TaskWaitConditionState::Channel { channel_id } => {
                WaitCondition::Channel(ChannelId(channel_id.as_str().into()))
            }
            TaskWaitConditionState::Resource { resource_key } => {
                WaitCondition::Resource(ResourceKey(resource_key.as_str().into()))
            }
            TaskWaitConditionState::External { subscription_id } => {
                WaitCondition::External(SubscriptionId(subscription_id.as_str().into()))
            }
        })
        .collect::<Vec<_>>();
    let mut satisfied = BTreeSet::new();
    for index in &state.satisfied {
        let index = *index as usize;
        if index >= conditions.len() || !satisfied.insert(index) {
            return Err(incompatible(format!(
                "task {task_id} carries invalid satisfied WaitSet index {index}"
            )));
        }
    }
    Ok(DurableWaitSet {
        mode,
        conditions,
        satisfied,
    })
}

fn termination_from_label(label: &str) -> Option<TerminationReason> {
    Some(match label {
        "completed" => TerminationReason::Completed,
        "max_turns" => TerminationReason::MaxTurns,
        "token_budget" => TerminationReason::TokenBudget,
        "timeout" => TerminationReason::Timeout,
        "user_abort" => TerminationReason::UserAbort,
        "error" => TerminationReason::Error,
        "milestone_exceeded" => TerminationReason::MilestoneExceeded,
        "context_overflow" => TerminationReason::ContextOverflow,
        "no_progress" => TerminationReason::NoProgress,
        _ => return None,
    })
}

/// §12.2 · put the context-VM partition back on the engine.
///
/// Order matters: the messages are pushed first so the partition token counters land on the
/// checkpoint's own numbers, then the handle table is repopulated **by id** so a handle addresses
/// the same body it addressed before, then the allocator is moved past all of them.
fn restore_context_vm(
    engine: &mut LoopStateMachine,
    state: &ContextVmState,
) -> Result<(), KernelFault> {
    let ctx = &mut engine.ctx;
    for entry in &state.messages {
        let message = restore_message(&entry.role, &entry.body, &entry.tool_calls)?;
        match entry.partition {
            MessagePartition::System => ctx.partitions.system.push(message, entry.tokens),
            MessagePartition::History => ctx.partitions.history.push(message, entry.tokens),
        }
    }
    for slot in &state.knowledge {
        let message = restore_message(&slot.role, &slot.body, &slot.tool_calls)?;
        ctx.partitions.knowledge.push_entry(
            slot.key.as_deref().map(Into::into),
            message,
            slot.tokens,
            slot.pinned,
        );
        if let Some(entry) = ctx.partitions.knowledge.entries.last_mut() {
            entry.evict_at_boundary = slot.evict_at_boundary;
            entry.use_count = slot.use_count;
            entry.last_used_step = slot.last_used_step;
            entry.pending = slot
                .pending
                .as_ref()
                .map(|pending| {
                    restore_message(&pending.role, &pending.body, &pending.tool_calls)
                        .map(|message| Box::new((message, pending.tokens)))
                })
                .transpose()?;
        }
    }
    ctx.partitions.system.measurements = state.system_measurements.clone();
    ctx.partitions.history.measurements = state.history_measurements.clone();
    ctx.restore_knowledge_checkpoint_state(
        state.knowledge_reference_step,
        state.knowledge_budget_warned,
    );
    ctx.partitions.signals = state.signals.clone();
    ctx.partitions.task_state = restore_task_state(&state.task_state);
    ctx.restore_state_generation(state.state_generation);
    ctx.last_activity_ms = state.last_activity_ms.get();
    ctx.last_compact_ms = state.last_compact_ms.map(WireU64::get);
    ctx.active_skills = state
        .active_skills
        .iter()
        .map(|lease| (lease.skill.as_str().into(), lease.lease_until_turn))
        .collect();

    for handle in &state.handles {
        ctx.handles.insert(Handle {
            id: handle.handle_id,
            kind: restore_handle_kind(&handle.kind)?,
            residency: restore_residency(handle)?,
            tokens: handle.tokens,
            source: handle.source.as_deref().map(Into::into),
        });
    }
    ctx.restore_next_handle_id(state.next_handle_id);
    if !ctx.restore_frozen_history_len(state.frozen_history_len as usize) {
        return Err(incompatible(format!(
            "the checkpoint freezes {} history messages but restores only {}",
            state.frozen_history_len,
            ctx.partitions.history.messages.len()
        )));
    }
    Ok(())
}

fn restore_message(
    role: &str,
    body: &StoredMessageBody,
    tool_calls: &[LogicalToolCall],
) -> Result<CoreMessage, KernelFault> {
    let role = role_from_label(role)
        .ok_or_else(|| incompatible(format!("the checkpoint carries message role {role:?}")))?;
    let content = match body {
        StoredMessageBody::Inline(inline) => message_content(
            inline.text.clone(),
            inline.tool_call_id.as_deref(),
            inline.is_error,
        ),
        StoredMessageBody::Reference(reference) => message_content(
            reference.preview.clone(),
            reference.tool_call_id.as_deref(),
            reference.is_error,
        ),
        StoredMessageBody::Structured(structured) => {
            if !structured.durable_tool_results.is_empty() {
                if structured.durable_content.is_some() {
                    return Err(incompatible(
                        "the checkpoint durable tool results must not carry another body form"
                            .to_string(),
                    ));
                }
                content_from_durable_tool_results(&structured.durable_tool_results).map_err(|error| incompatible(format!(
                    "the checkpoint carries durable tool results this runtime cannot restore: {error}"
                )))?
            } else if let Some(content) = &structured.durable_content {
                content.validate().map_err(|error| {
                    incompatible(format!(
                        "the checkpoint carries invalid durable content: {error}"
                    ))
                })?;
                content_from_durable(content).map_err(|error| incompatible(format!(
                    "the checkpoint carries durable content this runtime cannot restore: {error}"
                )))?
            } else {
                return Err(incompatible(
                    "the checkpoint structured message body has no content".to_string(),
                ));
            }
        }
    };
    Ok(CoreMessage {
        role,
        content,
        tool_calls: tool_calls
            .iter()
            .map(|call| {
                Ok(crate::types::message::ToolCall {
                    id: call.call_id.as_str().into(),
                    name: call.name.as_str().into(),
                    arguments: serde_json::from_str(&call.arguments).map_err(|error| {
                        incompatible(format!(
                            "tool call {} carries arguments that do not decode: {error}",
                            call.call_id
                        ))
                    })?,
                })
            })
            .collect::<Result<Vec<_>, KernelFault>>()?,
    })
}

fn restore_handle_kind(label: &str) -> Result<HandleKind, KernelFault> {
    Ok(match label {
        "tool_result" => HandleKind::ToolResult,
        "memory_page" => HandleKind::MemoryPage,
        "knowledge_entry" => HandleKind::KnowledgeEntry,
        "sub_agent_join" => HandleKind::SubAgentJoin,
        other => {
            return Err(incompatible(format!(
                "the checkpoint carries handle kind {other:?}, which this kernel does not know"
            )));
        }
    })
}

fn restore_residency(handle: &HandleState) -> Result<Residency, KernelFault> {
    let missing = |what: &str| {
        incompatible(format!(
            "handle {} is {} but carries no {what}",
            handle.handle_id, handle.residency
        ))
    };
    Ok(match handle.residency.as_str() {
        "resident" => Residency::Resident,
        "collapsed" => Residency::Collapsed,
        "external" => Residency::External {
            payload_ref: handle
                .payload_ref
                .clone()
                .ok_or_else(|| missing("locator"))?,
            digest: handle.digest.clone().ok_or_else(|| missing("digest"))?,
            original_size: handle
                .original_size
                .ok_or_else(|| missing("original size"))?
                .get(),
        },
        "paged_out" => Residency::PagedOut {
            payload_ref: handle
                .payload_ref
                .clone()
                .ok_or_else(|| missing("locator"))?,
            digest: handle.digest.clone().ok_or_else(|| missing("digest"))?,
        },
        other => {
            return Err(incompatible(format!(
                "the checkpoint carries residency {other:?}, which this kernel does not know"
            )));
        }
    })
}

fn incompatible(message: String) -> KernelFault {
    KernelFault::new(KernelFaultCode::CheckpointIncompatible, message)
}

fn project_task_state(state: &TaskState) -> LogicalTaskState {
    LogicalTaskState {
        goal: state.goal.clone(),
        criteria: state.criteria.clone(),
        plan: state
            .plan
            .iter()
            .map(|step| LogicalPlanStep {
                label: step.label.clone(),
                done: step.done,
            })
            .collect(),
        current_step: state.current_step.map(|index| index as u32),
        progress: state.progress.clone(),
        scratchpad: state.scratchpad.clone(),
        blocked_on: state.blocked_on.clone(),
        directives: state.directives.clone(),
        preserved_refs: state.preserved_refs.clone(),
        recent_actions: state.recent_actions.clone(),
        compression_log: state
            .compression_log
            .iter()
            .map(|entry| LogicalCompressionEntry {
                action: entry.action.clone(),
                summary: entry.summary.clone(),
            })
            .collect(),
        compression_log_dropped: WireU64::new(state.compression_log_dropped),
    }
}

fn restore_task_state(state: &LogicalTaskState) -> TaskState {
    TaskState {
        goal: state.goal.clone(),
        criteria: state.criteria.clone(),
        plan: state
            .plan
            .iter()
            .map(|step| PlanStep {
                label: step.label.clone(),
                done: step.done,
            })
            .collect(),
        current_step: state.current_step.map(|index| index as usize),
        progress: state.progress.clone(),
        scratchpad: state.scratchpad.clone(),
        blocked_on: state.blocked_on.clone(),
        directives: state.directives.clone(),
        preserved_refs: state.preserved_refs.clone(),
        recent_actions: state.recent_actions.clone(),
        compression_log: state
            .compression_log
            .iter()
            .map(|entry| CompressionEntry {
                action: entry.action.clone(),
                summary: entry.summary.clone(),
            })
            .collect(),
        compression_log_dropped: state.compression_log_dropped.get(),
    }
}

fn authority(message: &str) -> SyscallRefusal {
    SyscallRefusal::Fault(KernelFault::new(
        KernelFaultCode::InvalidAuthority,
        message.to_string(),
    ))
}

fn denial_reason(disposition: &Disposition, fallback: &str) -> String {
    match disposition {
        Disposition::Deny { stage, reason } => format!("{stage}: {reason}"),
        Disposition::RateLimited { retry_after_ms } => {
            format!("rate limited; retry after {retry_after_ms}ms")
        }
        Disposition::Gate { reason, .. } => format!("awaiting approval: {reason}"),
        Disposition::Defer { slot } => format!("deferred at slot {slot}"),
        Disposition::Allow => fallback.to_string(),
    }
}

/// What the kernel authored for a pending `PersistMemory` effect (§22.13).
#[derive(Debug, Clone, PartialEq)]
struct AuthoredMemoryWrite {
    binding_id: MemoryBindingId,
    name: String,
    kind: WireMemoryKind,
    size_bytes: u32,
}

/// What the kernel authored for a pending `QueryMemory` effect.
#[derive(Debug, Clone, PartialEq)]
struct AuthoredMemoryQuery {
    binding_id: MemoryBindingId,
    text: String,
    requested_k: u32,
}

/// What the kernel published for a pending `LoadPayload` effect (§7.10 rule 4).
#[derive(Debug, Clone, PartialEq)]
struct PendingPayloadLoad {
    /// The wire address of the handle — the tool `call_id` for an external result, the kernel-minted
    /// archive id for a paged-out one.
    handle_id: String,
    /// The digest the body has to reproduce. The kernel never saw the body, so this is the *only*
    /// thing that makes a restored payload the one that left.
    digest: String,
    /// Present only for [`Residency::External`], whose declared size the kernel admitted and can
    /// therefore hold the host to. A page-out archive is checked by digest alone.
    original_size: Option<u64>,
}

/// What one admitted syscall produced.
#[derive(Debug, Default)]
struct SyscallOutcome {
    effects: Vec<KernelEffect>,
    /// Set only by `SubmitWorkflow`'s bootstrap arm — the one syscall that moves the focus.
    focus: Option<ExecutionFocus>,
    /// The DAG grew and still owes a spawn round. Honoured on the provider-tool path; on the
    /// child-completion path the completion's own drive produces the batch (§7.7 ordering).
    needs_workflow_round: bool,
    /// Optional structured response for read-like local syscalls.
    ack: Option<String>,
}

/// Reduces the five canonical input classes onto the kernel's existing semantic mechanisms.
///
/// Use it as the plan function of [`KernelTransaction::prepare`](super::transaction::KernelTransaction::prepare):
///
/// ```ignore
/// let preparation = tx.prepare(&envelope, |ctx| driver.plan(ctx));
/// // ... host CAS-appends the record ...
/// let committed = tx.commit(&token, &head)?;
/// driver.note_committed(committed.step_seq)?;
/// ```
pub struct CanonicalOperationDriver {
    engine: Option<LoopStateMachine>,
    root_kind: Option<RootKind>,
    focus: Option<ExecutionFocus>,
    workflow_id: Option<WorkflowId>,
    /// Wire node identity by internal DAG index — the DAG the engine runs is index-addressed, the
    /// wire is not, and the mapping is what keeps a `SpawnTasks` effect nameable by the host.
    node_ids: Vec<NodeId>,
    /// Canonical source DAG in the same index order as `node_ids`. Checkpoint projection pairs it
    /// with the semantic node statuses without serializing the graph's private indexes.
    workflow_nodes: Vec<WireNode>,
    /// The attempt the kernel minted for each **live** task. §10.4: a host may not create or
    /// rewrite child identity through a resolution or a completion, so a completion that names an
    /// attempt this kernel never issued is refused rather than folded in. A completed attempt is
    /// removed here, which is what makes a second completion — and the `parent_requests` riding on
    /// it — a stale causation rather than a second helping of authority.
    attempts: BTreeMap<String, AttemptId>,
    /// §7.6 · the provider calls this operation is waiting on, by effect id. At most one is live
    /// (DEC-3); the map keys it so a resolution names its own call rather than "the current one".
    provider_calls: BTreeMap<EffectId, PendingProviderCall>,
    /// §22.13 · the memory records **this kernel authored** for its pending `PersistMemory`
    /// effects. The resolution reports these, never what the host echoes back: a receipt may carry
    /// its store's own locator and digest and nothing else, so no host reply can restate a name,
    /// kind, size, trust or provenance the kernel derived.
    pending_memory_writes: BTreeMap<EffectId, AuthoredMemoryWrite>,
    /// The same for `QueryMemory`: the query the kernel authored and the width it clamped.
    pending_memory_queries: BTreeMap<EffectId, AuthoredMemoryQuery>,
    /// §7.10 · the handle each pending `LoadPayload` addresses, with the digest the loaded body has
    /// to reproduce. Kept here rather than re-read from the handle table at resolution time, so a
    /// residency that moved in between cannot silently change what a page-in is verified against.
    pending_payload_loads: BTreeMap<EffectId, PendingPayloadLoad>,
    /// Tool call ids that already produced a syscall. A causation is spent once — replaying the
    /// same provider result with a fresh input id must not buy a second workflow append.
    consumed_calls: BTreeSet<String>,
    /// §13.2 / DEC-6 · the live-mutable half of the configuration, plus the revision two concurrent
    /// writers race on. Seeded from the genesis record's resolved configuration; only
    /// `HostCommand::ApplyPolicyPatch` moves it. Boot-only axes are read from `context.config`
    /// instead, which is why the transaction never needs a second copy of this.
    policy: Option<LivePolicyState>,
    /// §7.3 · the verification contract whose cascade is installed on the engine, if any.
    ///
    /// The engine's own `LoopAction::EvaluateMilestone` names only a phase, because internally
    /// there is one cascade and a phase id is enough. The wire needs the pair: `phase_id` is
    /// unique only within its contract, so `(contract_id, phase_id)` is the host's complete lookup
    /// key. The driver retains that contract id beside the semantic engine so the wire projection
    /// is total without duplicating host-owned verifier state inside the engine.
    loaded_contract_id: Option<String>,
    staged: Option<StagedFocus>,
    poison: Option<KernelFault>,
}

impl Default for CanonicalOperationDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl CanonicalOperationDriver {
    pub fn new() -> Self {
        Self {
            engine: None,
            root_kind: None,
            focus: None,
            workflow_id: None,
            node_ids: Vec::new(),
            workflow_nodes: Vec::new(),
            attempts: BTreeMap::new(),
            provider_calls: BTreeMap::new(),
            pending_memory_writes: BTreeMap::new(),
            pending_memory_queries: BTreeMap::new(),
            pending_payload_loads: BTreeMap::new(),
            consumed_calls: BTreeSet::new(),
            policy: None,
            loaded_contract_id: None,
            staged: None,
            poison: None,
        }
    }

    // ----- observers -----

    /// The operation's root class. `None` until the root start commits; immutable afterwards.
    pub fn root_kind(&self) -> Option<RootKind> {
        self.root_kind
    }

    /// Where control currently is. Moves only on a committed transition (§7.4).
    pub fn focus(&self) -> Option<&ExecutionFocus> {
        self.focus.as_ref()
    }

    pub fn workflow_id(&self) -> Option<&WorkflowId> {
        self.workflow_id.as_ref()
    }

    /// Return the kernel-issued live attempt for `task_id`.
    ///
    /// Bindings use this read-only projection to correlate a host completion with the live task
    /// attempt. The value comes from checkpointed kernel state; hosts must never synthesize it.
    pub fn attempt_id(&self, task_id: &str) -> Option<&AttemptId> {
        self.attempts.get(task_id)
    }

    pub fn poison(&self) -> Option<&KernelFault> {
        self.poison.as_ref()
    }

    /// Read-only access to the semantic engine, for tests and host projections.
    pub fn engine(&self) -> Option<&LoopStateMachine> {
        self.engine.as_ref()
    }

    /// Where the driver's own fold says the operation is. The transaction stays the authority on
    /// lifecycle; this exists so a host projection never needs a second copy of the rule.
    pub fn lifecycle(&self) -> OperationLifecycle {
        match (self.engine.is_some(), self.root_kind) {
            (false, _) => OperationLifecycle::Created,
            (true, None) => OperationLifecycle::Configured,
            (true, Some(_)) => OperationLifecycle::Running,
        }
    }
}

mod continuation;
mod effects;
mod events;
mod planning;
mod projection;
mod provider;
mod syscall;

// ---------------------------------------------------------------------------------------------
// wire ⇄ semantic projections
// ---------------------------------------------------------------------------------------------

fn root_task_id() -> TaskId {
    TaskId::new(ROOT_TASK_ID).expect("the root task id is a legal branded ref")
}

// ---------------------------------------------------------------------------------------------
// §7.6 · the model-facing syscall surface
// ---------------------------------------------------------------------------------------------

/// Tool names that reduce to a P1 syscall instead of to a host tool execution.
///
/// This list is what deletes §22.10's bypasses 4 and 5. Historically the SDK *removed*
/// `submit_workflow_nodes` / `start_workflow` from the tool loop, faked a tool result for the
/// model, and re-submitted the request as a separate kernel input with a submitter of its own
/// choosing — so the kernel never saw a `ProviderTool` causation at all. Here the names are the
/// kernel's, the arguments are decoded by the kernel, and the caller comes from the pending call.
///
/// F15 ruling (0.2.66, user-adjudicated — resolves the §7.6 SPEC-ISSUE): `RequestMemoryWrite`
/// has no entry here **by design** — the kernel gains no model-facing memory *write* surface.
/// `memory` is a search tool, long-term writes are extracted host-side (§22.13's 现状定位), and
/// the syscall's only caller channel is a child's `parent_requests` (child→parent only). If a
/// spec revision ever proposes a write surface, it must overturn this ruling explicitly.
pub const SYSCALL_TOOL_NAMES: &[&str] = &[
    "start_workflow",
    "submit_workflow_nodes",
    "skill",
    "update_plan",
    crate::context::manager::MEMORY_TOOL_NAME,
    crate::context::manager::READ_RESULT_TOOL_NAME,
    "send_message",
    "publish_channel",
    "receive_mailbox",
    "receive_channel",
    "read_object",
];

fn is_syscall_tool(name: &str) -> bool {
    SYSCALL_TOOL_NAMES.contains(&name)
}

/// Decode a recognised meta-tool call into its typed request.
///
/// A decode failure is a *rejection*, never a fault: the model wrote bad arguments, which is a
/// thing to answer with an audit fact rather than a host protocol violation.
fn decode_syscall(call: &WireToolCall) -> Result<SyscallRequest, SyscallRejection> {
    let arguments = call.arguments.get().clone();
    let name: &'static str = SYSCALL_TOOL_NAMES
        .iter()
        .copied()
        .find(|known| *known == call.name.as_str())
        .expect("only recognised syscall tools reach the decoder");

    fn decode<T: serde::de::DeserializeOwned>(
        name: &'static str,
        arguments: serde_json::Value,
    ) -> Result<T, SyscallRejection> {
        serde_json::from_value(arguments)
            .map_err(|error| SyscallRejection::new(name, format!("malformed arguments: {error}")))
    }

    match name {
        "start_workflow" => Ok(SyscallRequest::SubmitWorkflow(
            super::syscall::SubmitWorkflowRequest {
                spec: decode(name, arguments)?,
            },
        )),
        "submit_workflow_nodes" => {
            #[derive(serde::Deserialize)]
            struct Args {
                nodes: Vec<WireNode>,
            }
            let args: Args = decode(name, arguments)?;
            Ok(SyscallRequest::AppendWorkflowNodes(
                super::syscall::AppendWorkflowNodesRequest { nodes: args.nodes },
            ))
        }
        "skill" => {
            #[derive(serde::Deserialize)]
            struct Args {
                name: String,
                #[serde(default)]
                lease_turns: Option<u32>,
            }
            let args: Args = decode(name, arguments)?;
            Ok(SyscallRequest::ActivateSkill(
                super::syscall::ActivateSkillRequest {
                    name: args.name,
                    lease_turns: args.lease_turns,
                },
            ))
        }
        "update_plan" => Ok(SyscallRequest::UpdateTask(
            super::syscall::UpdateTaskRequest {
                update: decode(name, arguments)?,
            },
        )),
        crate::context::manager::MEMORY_TOOL_NAME => {
            #[derive(serde::Deserialize)]
            struct Args {
                #[serde(default)]
                query: String,
                #[serde(default)]
                kinds: Vec<WireMemoryKind>,
                #[serde(default)]
                top_k: Option<u32>,
            }
            let args: Args = decode(name, arguments)?;
            Ok(SyscallRequest::RequestMemoryQuery(
                super::syscall::RequestMemoryQueryRequest {
                    query: super::syscall::MemoryQueryProposal {
                        text: args.query,
                        kinds: args.kinds,
                        limit: args.top_k,
                    },
                },
            ))
        }
        crate::context::manager::READ_RESULT_TOOL_NAME => {
            #[derive(serde::Deserialize)]
            struct Args {
                call_id: String,
            }
            let args: Args = decode(name, arguments)?;
            let handle_id = super::scalar::HandleId::new(args.call_id).map_err(|error| {
                SyscallRejection::new(name, format!("malformed handle: {}", error.message))
            })?;
            Ok(SyscallRequest::PageIn(super::syscall::PageInRequest {
                handle_id,
            }))
        }
        "send_message" => Ok(SyscallRequest::SendMessage(decode(name, arguments)?)),
        "publish_channel" => Ok(SyscallRequest::PublishChannel(decode(name, arguments)?)),
        "receive_mailbox" => Ok(SyscallRequest::ReceiveMailbox(decode(name, arguments)?)),
        "receive_channel" => Ok(SyscallRequest::ReceiveChannel(decode(name, arguments)?)),
        "read_object" => Ok(SyscallRequest::ReadObject(decode(name, arguments)?)),
        other => unreachable!("unrecognised syscall tool {other}"),
    }
}

/// The task a causation names. Both variants carry one, and neither lets a host choose it.
fn causation_task(causation: &SyscallCausation) -> TaskId {
    match causation {
        SyscallCausation::ProviderTool(provider) => provider.task_id.clone(),
        SyscallCausation::ChildAttempt(child) => child.task_id.clone(),
    }
}

/// §7.6 · the authority families a quarantined caller may not touch. `None` ⇒ the request widens
/// nothing (a plan edit, a page-in of an address the caller already holds).
///
/// SPEC-ISSUE: §7.6 requires that "a quarantined task must not escalate through workflow append,
/// memory scope or capability mutation", but the canonical [`WorkflowNode`](super::root::WorkflowNode)
/// carries no trust level — the internal DAG has `NodeTrust::{Trusted,Quarantined}` and the wire
/// has no field for it. The refusal below is therefore complete but currently unreachable through
/// the contract: no canonical input can declare a node quarantined. Either §7.4's workflow node
/// grows a trust field, or §7.6 has to say where quarantine comes from.
fn privileged_family(request: &SyscallRequest) -> Option<&'static str> {
    match request {
        SyscallRequest::SubmitWorkflow(_) | SyscallRequest::AppendWorkflowNodes(_) => {
            Some("workflow")
        }
        SyscallRequest::RequestMemoryWrite(_) | SyscallRequest::RequestMemoryQuery(_) => {
            Some("memory")
        }
        SyscallRequest::ActivateSkill(_) => Some("capability"),
        SyscallRequest::SendMessage(_) | SyscallRequest::PublishChannel(_) => Some("ipc"),
        SyscallRequest::UpdateTask(_)
        | SyscallRequest::PageIn(_)
        | SyscallRequest::ReceiveMailbox(_)
        | SyscallRequest::ReceiveChannel(_)
        | SyscallRequest::ReadObject(_) => None,
    }
}

fn core_task_update(update: &WireTaskUpdate) -> crate::context::task_state::TaskUpdate {
    crate::context::task_state::TaskUpdate {
        plan: update.plan.clone(),
        current_step: update.current_step.map(|step| step as usize),
        progress: update.progress.clone(),
        scratchpad: update.scratchpad.clone(),
        blocked_on: update.blocked_on.clone(),
        preserved_refs: update.preserved_refs.clone(),
        directives: update.directives.clone(),
    }
}

fn mint_effect_id(operation_id: &OperationId, step_seq: WireU64, index: u32) -> EffectId {
    EffectId::new(format!("{operation_id}:step:{step_seq}:effect:{index}"))
        .expect("an operation-scoped effect id is always a legal branded ref")
}

fn mint_workflow_id(operation_id: &OperationId, step_seq: WireU64) -> WorkflowId {
    WorkflowId::new(format!("{operation_id}:workflow:{step_seq}"))
        .expect("an operation-scoped workflow id is always a legal branded ref")
}

/// `wf-node{N}` / `wf-node{N}-i{k}` → `N`. The internal DAG is index-addressed; the wire is not.
fn parse_node_index(agent_id: &str) -> Option<usize> {
    let rest = agent_id.strip_prefix("wf-node")?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

fn wire_node_ids(spec: &WireSpec) -> Vec<NodeId> {
    spec.nodes.iter().map(|node| node.node_id.clone()).collect()
}

/// Node identity is unique across the whole DAG, not only within one batch. `build_core_spec`
/// checks a batch against itself; an append must also be checked against every node the DAG
/// already holds, or two nodes share one wire id and every later spawn effect and terminal
/// outcome names the wrong node.
fn ensure_fresh_node_ids(existing: &[NodeId], nodes: &[WireNode]) -> Result<(), KernelFault> {
    for node in nodes {
        if existing.iter().any(|known| known == &node.node_id) {
            return Err(KernelFault::new(
                KernelFaultCode::InvalidConfig,
                format!(
                    "workflow node id {:?} is already declared in this DAG; node identity is \
                     unique across every appended batch",
                    node.node_id
                ),
            ));
        }
    }
    Ok(())
}

/// Wire DAG → the kernel's index-addressed DAG. Node identity is checked here: a duplicate id or a
/// dependency on a node the spec does not declare is refused before the engine sees the spec.
fn build_core_spec(spec: &WireSpec) -> Result<CoreWorkflowSpec, KernelFault> {
    let mut index_of: BTreeMap<&str, usize> = BTreeMap::new();
    for (index, node) in spec.nodes.iter().enumerate() {
        if index_of.insert(node.node_id.as_str(), index).is_some() {
            return Err(KernelFault::new(
                KernelFaultCode::InvalidConfig,
                format!(
                    "workflow node id {:?} appears twice; node identity is unique within a DAG",
                    node.node_id
                ),
            ));
        }
    }
    let mut nodes = Vec::with_capacity(spec.nodes.len());
    for node in &spec.nodes {
        let role = node
            .run_spec
            .as_ref()
            .and_then(|spec| spec.role)
            .map_or(AgentRole::Custom, core_role);
        let mut core = CoreWorkflowNode::new(runtime_task(&node.task), role);
        if let Some(isolation) = node.run_spec.as_ref().and_then(|spec| spec.isolation) {
            core = core.with_isolation(core_isolation(isolation));
        }
        if let Some(inheritance) = node
            .run_spec
            .as_ref()
            .and_then(|spec| spec.context_inheritance)
        {
            core.context_inheritance = core_context_inheritance(inheritance);
        }
        if let Some(metadata) = node
            .run_spec
            .as_ref()
            .map(|spec| spec.metadata.get())
            .and_then(serde_json::Value::as_object)
        {
            if let Some(model_hint) = metadata
                .get("model_hint")
                .and_then(serde_json::Value::as_str)
            {
                core = core.with_model_hint(model_hint);
            }
            if let Some(output_schema) = metadata.get("output_schema") {
                core = core.with_output_schema(output_schema.clone());
            }
            if let Some(token_budget) = metadata.get("token_budget") {
                let tokens = token_budget.as_u64().ok_or_else(|| {
                    KernelFault::new(
                        KernelFaultCode::InvalidConfig,
                        format!(
                            "workflow node {:?} metadata.token_budget must be a non-negative integer",
                            node.node_id
                        ),
                    )
                })?;
                core = core.with_token_budget(tokens);
            }
            if let Some(max_turns) = metadata.get("max_turns") {
                let turns = max_turns
                    .as_u64()
                    .and_then(|value| u32::try_from(value).ok())
                    .ok_or_else(|| {
                        KernelFault::new(
                            KernelFaultCode::InvalidConfig,
                            format!(
                                "workflow node {:?} metadata.max_turns must be a 32-bit non-negative integer",
                                node.node_id
                            ),
                        )
                    })?;
                core = core.with_max_turns(turns);
            }
            if let Some(max_wall_ms) = metadata.get("max_wall_ms") {
                let millis = max_wall_ms.as_u64().ok_or_else(|| {
                    KernelFault::new(
                        KernelFaultCode::InvalidConfig,
                        format!(
                            "workflow node {:?} metadata.max_wall_ms must be a non-negative integer",
                            node.node_id
                        ),
                    )
                })?;
                core = core.with_max_wall_ms(millis);
            }
            // spc_008-01: fine-grained capability requests, smuggled through the same generic
            // `metadata` escape hatch `model_hint`/`output_schema` already use rather than a new
            // dedicated wire field. Fails closed on malformed input rather than silently treating
            // it as "no capability requested" — a security-relevant declaration must not downgrade
            // itself into a no-op check on a parse error.
            if let Some(requested) = metadata.get("requested_capabilities") {
                let capabilities: Vec<crate::types::capability::Capability> =
                    serde_json::from_value(requested.clone()).map_err(|error| {
                        KernelFault::new(
                            KernelFaultCode::InvalidConfig,
                            format!(
                                "workflow node {:?} metadata.requested_capabilities is malformed: {error}",
                                node.node_id
                            ),
                        )
                    })?;
                core = core.with_requested_capabilities(capabilities);
            }
            // spc_008-02: same escape-hatch pattern, same fail-closed convention, for the
            // hierarchical budget grant a node's spawn requests.
            if let Some(requested) = metadata.get("requested_budget") {
                let budget: crate::scheduler::budget_grant::ResourceBudget =
                    serde_json::from_value(requested.clone()).map_err(|error| {
                        KernelFault::new(
                            KernelFaultCode::InvalidConfig,
                            format!(
                                "workflow node {:?} metadata.requested_budget is malformed: {error}",
                                node.node_id
                            ),
                        )
                    })?;
                core = core.with_requested_budget(budget);
            }
            if let Some(factors) = metadata.get("scheduling_factors") {
                let factors: crate::orchestration::task_graph::SchedulingFactors =
                    serde_json::from_value(factors.clone()).map_err(|error| {
                        KernelFault::new(
                            KernelFaultCode::InvalidConfig,
                            format!(
                                "workflow node {:?} metadata.scheduling_factors is malformed: {error}",
                                node.node_id
                            ),
                        )
                    })?;
                core = core.with_scheduling_factors(factors);
            }
        }
        let mut depends_on = Vec::with_capacity(node.depends_on.len());
        for dependency in &node.depends_on {
            let Some(&index) = index_of.get(dependency.as_str()) else {
                return Err(KernelFault::new(
                    KernelFaultCode::InvalidConfig,
                    format!(
                        "workflow node {:?} depends on {:?}, which this DAG does not declare",
                        node.node_id, dependency
                    ),
                ));
            };
            depends_on.push(index);
        }
        nodes.push(core.with_depends_on(depends_on));
    }
    let core = CoreWorkflowSpec::new(nodes);
    core.validate()
        .map_err(|error| KernelFault::new(KernelFaultCode::InvalidConfig, error.to_string()))?;
    Ok(core)
}

fn runtime_task(task: &LogicalTask) -> RuntimeTask {
    RuntimeTask {
        goal: task.goal.clone(),
        criteria: task.criteria.clone(),
        metadata: task.metadata.get().clone(),
        lane: task.lane.as_ref().map(TaskLane::new).unwrap_or_default(),
    }
}

/// The logical spec carries no host session identity, so its internal identity carries none.
fn agent_run_spec(spec: &LogicalAgentSpec) -> AgentRunSpec {
    AgentRunSpec {
        identity: AgentIdentity::new(ROOT_TASK_ID, NO_HOST_SESSION),
        role: spec.role.map_or(AgentRole::Custom, core_role),
        isolation: spec
            .isolation
            .map_or(AgentIsolation::Shared, core_isolation),
        goal: spec.goal.clone(),
        verification_contract_id: spec.verification_contract_id.as_deref().map(Into::into),
        capability_filter: AgentCapabilityFilter {
            allowed_kinds: spec
                .capability_filter
                .allowed_kinds
                .iter()
                .copied()
                .map(core_capability_kind)
                .collect(),
            allowed_ids: spec
                .capability_filter
                .allowed_ids
                .iter()
                .map(|id| id.as_str().into())
                .collect(),
        },
        milestones: None,
        metadata: spec.metadata.get().clone(),
        loop_round: spec.loop_round.as_ref().map(|round| LoopRoundSpec {
            max_rounds: round.max_rounds,
            min_sleep_ms: round.min_sleep_ms.map(WireU64::get),
            max_sleep_ms: round.max_sleep_ms.map(WireU64::get),
            default_action: round.default_action.clone(),
        }),
        exposure_baseline: spec
            .exposure_baseline
            .as_ref()
            .map(|ids| ids.iter().map(|id| id.as_str().into()).collect()),
        requested_capabilities: Vec::new(),
        requested_budget: None,
    }
}

fn logical_agent_run_spec(spec: &AgentRunSpec) -> LogicalAgentSpec {
    LogicalAgentSpec {
        goal: spec.goal.clone(),
        role: match spec.role {
            AgentRole::Custom => None,
            AgentRole::Explore => Some(WireRole::Explore),
            AgentRole::Plan => Some(WireRole::Plan),
            AgentRole::Implement => Some(WireRole::Implement),
            AgentRole::Verify => Some(WireRole::Verify),
        },
        isolation: match spec.isolation {
            AgentIsolation::Shared => None,
            AgentIsolation::ReadOnly => Some(WireIsolation::ReadOnly),
            AgentIsolation::Worktree => Some(WireIsolation::Worktree),
            AgentIsolation::Remote => Some(WireIsolation::Remote),
        },
        context_inheritance: None,
        verification_contract_id: spec
            .verification_contract_id
            .as_ref()
            .map(ToString::to_string),
        capability_filter: super::root::CapabilityFilter {
            allowed_kinds: spec
                .capability_filter
                .allowed_kinds
                .iter()
                .copied()
                .map(wire_capability_kind)
                .collect(),
            allowed_ids: spec
                .capability_filter
                .allowed_ids
                .iter()
                .map(ToString::to_string)
                .collect(),
        },
        exposure_baseline: spec
            .exposure_baseline
            .as_ref()
            .map(|ids| ids.iter().map(ToString::to_string).collect()),
        loop_round: spec
            .loop_round
            .as_ref()
            .map(|round| super::root::LogicalLoopRoundSpec {
                max_rounds: round.max_rounds,
                min_sleep_ms: round.min_sleep_ms.map(WireU64::new),
                max_sleep_ms: round.max_sleep_ms.map(WireU64::new),
                default_action: round.default_action.clone(),
            }),
        metadata: super::scalar::BoundedJson::new(spec.metadata.clone())
            .expect("canonical run metadata remains bounded"),
    }
}

fn core_capability_kind(
    kind: super::root::CapabilityKind,
) -> crate::types::capability::CapabilityKind {
    use super::root::CapabilityKind as Wire;
    use crate::types::capability::CapabilityKind as Core;
    match kind {
        Wire::Tool => Core::Tool,
        Wire::Skill => Core::Skill,
        Wire::Memory => Core::Memory,
        Wire::Knowledge => Core::Knowledge,
        Wire::McpServer => Core::McpServer,
        Wire::Command => Core::Command,
        Wire::Agent => Core::Agent,
    }
}

fn wire_capability_kind(
    kind: crate::types::capability::CapabilityKind,
) -> super::root::CapabilityKind {
    use super::root::CapabilityKind as Wire;
    use crate::types::capability::CapabilityKind as Core;
    match kind {
        Core::Tool => Wire::Tool,
        Core::Skill => Wire::Skill,
        Core::Memory => Wire::Memory,
        Core::Knowledge => Wire::Knowledge,
        Core::McpServer => Wire::McpServer,
        Core::Command => Wire::Command,
        Core::Agent => Wire::Agent,
    }
}

fn core_role(role: WireRole) -> AgentRole {
    match role {
        WireRole::Explore => AgentRole::Explore,
        WireRole::Plan => AgentRole::Plan,
        WireRole::Implement => AgentRole::Implement,
        WireRole::Verify => AgentRole::Verify,
        WireRole::Custom => AgentRole::Custom,
    }
}

fn core_isolation(isolation: WireIsolation) -> AgentIsolation {
    match isolation {
        WireIsolation::Shared => AgentIsolation::Shared,
        WireIsolation::ReadOnly => AgentIsolation::ReadOnly,
        WireIsolation::Worktree => AgentIsolation::Worktree,
        WireIsolation::Remote => AgentIsolation::Remote,
    }
}

fn core_context_inheritance(inheritance: WireContextInheritance) -> ContextInheritance {
    match inheritance {
        WireContextInheritance::None => ContextInheritance::None,
        WireContextInheritance::SystemOnly => ContextInheritance::SystemOnly,
        WireContextInheritance::Full => ContextInheritance::Full,
    }
}

/// Internal role/isolation labels back onto the wire vocabulary. `None` is the *absent* field, not
/// a parse failure: `custom`/`shared` are the wire defaults, so omitting them keeps a launch spec
/// minimal instead of restating what the contract already implies.
fn parse_wire_role(label: &str) -> Option<WireRole> {
    match label {
        "explore" => Some(WireRole::Explore),
        "plan" => Some(WireRole::Plan),
        "implement" => Some(WireRole::Implement),
        "verify" => Some(WireRole::Verify),
        _ => None,
    }
}

fn parse_wire_isolation(label: &str) -> Option<WireIsolation> {
    match label {
        "read_only" => Some(WireIsolation::ReadOnly),
        "worktree" => Some(WireIsolation::Worktree),
        "remote" => Some(WireIsolation::Remote),
        _ => None,
    }
}

fn parse_wire_context_inheritance(label: &str) -> Option<WireContextInheritance> {
    match label {
        "none" => Some(WireContextInheritance::None),
        "system_only" => Some(WireContextInheritance::SystemOnly),
        "full" => Some(WireContextInheritance::Full),
        _ => None,
    }
}

/// §7.4 · seed the P3 context partitions from the one initial context the start carried. This is
/// the whole of what used to be a dozen separate accepted inputs.
fn seed_initial_context(engine: &mut LoopStateMachine, initial: &InitialContext) {
    if !initial.messages.is_empty() {
        engine.preload_history(initial.messages.iter().map(logical_message).collect());
    }
    seed_knowledge(engine, &initial.knowledge);
    if !initial.requested_capabilities.is_empty() {
        engine.set_requested_capabilities(initial.requested_capabilities.clone());
    }
}

/// The one place wire knowledge entries enter the P3 knowledge partition.
///
/// Shared by the initial context (§7.4), `HostCommand::SeedKnowledge` (DEC-9) and the upsert half
/// of `HostCommand::ApplyKnowledgeMutation` (§13.2), so the three cannot drift in how a keyed,
/// pinned or token-counted entry is stored.
fn seed_knowledge(engine: &mut LoopStateMachine, entries: &[super::root::KnowledgeEntry]) {
    if entries.is_empty() {
        return;
    }
    let entries: Vec<crate::mm::PageInEntry> = entries
        .iter()
        .map(|entry| crate::mm::PageInEntry {
            content: entry.content.clone(),
            tokens: entry.tokens,
            source: None,
            key: entry.key.clone(),
            pinned: entry.pinned,
        })
        .collect();
    engine.apply_page_in(&entries);
}

/// §7.7 · project one logical signal onto the runtime signal the in-kernel router works with.
///
/// Three rules are load-bearing:
///
/// * the **business** signal id travels verbatim. It is what the disposition, expiry and
///   displacement audit facts name, so a derived id would report an identity no caller ever wrote;
/// * `timestamp_ms` is the **envelope's accepted time**, not the signal's `source_timestamp_ms`.
///   The source timestamp is audit metadata and stays out of every admission decision (§11.2);
/// * absent optional fields mean "the author did not say", not a default urgency or source that
///   would change how the signal is scheduled;
/// * `escalate_after_ms` is a **duration** the kernel anchors to that same accepted time. That is
///   what closes the old gap where §13.2 admitted `SignalPolicy.deadline_escalation` while §7.7
///   carried nothing that could ever come due, leaving the whole escalation axis unreachable
///   (Task 14 · adjudication §5n item 1). The kernel still invents no deadline from
///   `source_timestamp_ms`, which is not a clock (§11.2).
///
/// SPEC-ISSUE (task-targeted routing): §7.7 defines the address space (operation or logical task)
/// but no per-task attention semantics, and core holds **one** router per operation. A validated
/// task target therefore lands in the operation's queue rather than a queue of its own. Either
/// §7.7 states that the target is audit-only addressing, or per-task queues need a contract.
fn runtime_signal(
    signal: &LogicalSignal,
    accepted_at_ms: WireU64,
) -> crate::types::signal::RuntimeSignal {
    use crate::types::signal::{RuntimeSignal, SignalSource, SignalType, Urgency};

    let source = match signal.source {
        Some(SignalSourceKind::Cron) => SignalSource::Cron,
        Some(SignalSourceKind::Gateway) => SignalSource::Gateway,
        Some(SignalSourceKind::Heartbeat) => SignalSource::Heartbeat,
        Some(SignalSourceKind::Custom) | None => SignalSource::Custom,
    };
    let urgency = match signal.urgency {
        Some(SignalUrgency::Low) => Urgency::Low,
        Some(SignalUrgency::High) => Urgency::High,
        Some(SignalUrgency::Critical) => Urgency::Critical,
        Some(SignalUrgency::Normal) | None => Urgency::Normal,
    };
    let mut runtime = RuntimeSignal::new(
        source,
        // `signal_type` is deliberately not on the canonical wire (adjudication §5n item 3):
        // urgency already expresses priority, nothing branches on the router's event/job/alert
        // distinction, and a second axis that changes no decision is one more thing four hosts
        // would have to agree about. Every canonical signal enters as an event.
        SignalType::Event,
        urgency,
        signal_summary(signal),
    )
    .with_id(signal.signal_id.as_str())
    .with_payload(signal.payload.get().clone())
    .with_timestamp(accepted_at_ms.get());
    if let Some(key) = &signal.dedupe_key {
        runtime = runtime.with_dedupe(key.as_str());
    }
    // §7.7 · `escalate_after_ms` is a duration; the router works in instants. Anchoring it to the
    // envelope's accepted time here is the whole point of carrying a duration on the wire: the
    // same bytes redelivered produce the same deadline relative to *this* admission, and no host
    // clock ever enters the payload (DEC-2).
    if let Some(after) = signal.escalate_after_ms {
        runtime = runtime.with_deadline(accepted_at_ms.get().saturating_add(after.get()));
    }
    runtime
}

/// The model-facing one-liner a queued or interrupting signal becomes.
///
/// §7.7 carries a payload and no summary, so the summary is derived — deterministically, because a
/// replay must produce the same context bytes. A JSON string payload is its own summary; anything
/// else is its canonical serialization, bounded.
fn signal_summary(signal: &LogicalSignal) -> String {
    const SIGNAL_SUMMARY_MAX_BYTES: usize = 512;
    match signal.payload.get() {
        serde_json::Value::Null => signal.signal_id.as_str().to_string(),
        serde_json::Value::String(text) => {
            truncate_on_char_boundary(text, SIGNAL_SUMMARY_MAX_BYTES)
        }
        other => truncate_on_char_boundary(&other.to_string(), SIGNAL_SUMMARY_MAX_BYTES),
    }
}

fn live_policy_label(patch: &super::command::LivePolicyPatch) -> &'static str {
    use super::command::LivePolicyPatch;
    match patch {
        LivePolicyPatch::ReplaceSignalPolicy(_) => "signal",
        LivePolicyPatch::ReplaceGovernancePolicy(_) => "governance",
        LivePolicyPatch::TightenResourceQuota(_) => "resource_quota",
        LivePolicyPatch::ReplaceRecoveryPolicy(_) => "recovery",
    }
}

fn logical_message(message: &super::root::LogicalMessage) -> CoreMessage {
    // A tool message's `tool_call_id` becomes a structural tool-result part, the same shape a
    // resolved tool effect leaves in history, so the pairing survives rendering and compaction.
    let content = match (&message.tool_call_id, message.role) {
        (Some(call_id), MessageRole::Tool) => Content::Parts(vec![ContentPart::ToolResult {
            call_id: call_id.as_str().into(),
            output: message.content.clone(),
            is_error: message.is_error,
            durable_content: None,
        }]),
        _ => Content::Text(message.content.clone()),
    };
    CoreMessage {
        role: core_role_of(message.role),
        content,
        tool_calls: message.tool_calls.iter().map(core_tool_call).collect(),
    }
}

fn core_role_of(role: MessageRole) -> Role {
    match role {
        MessageRole::System => Role::System,
        MessageRole::User => Role::User,
        MessageRole::Assistant => Role::Assistant,
        MessageRole::Tool => Role::Tool,
    }
}

fn wire_role_of(role: Role) -> MessageRole {
    match role {
        Role::System => MessageRole::System,
        Role::User => MessageRole::User,
        Role::Assistant => MessageRole::Assistant,
        Role::Tool => MessageRole::Tool,
    }
}

fn rendered_context(
    context: &crate::context::renderer::InternalRenderedContext,
) -> WireRenderedContext {
    WireRenderedContext {
        system_stable: context.system_stable.clone(),
        system_knowledge: context.system_knowledge.clone(),
        turns: context.turns.iter().map(provider_message).collect(),
        state_turn: context.state_turn.as_ref().map(provider_message),
        frozen_prefix_len: context.frozen_prefix_len.map(|len| len as u32),
    }
}

fn provider_message(message: &CoreMessage) -> ProviderMessage {
    let (content, tool_call_id, is_error) = match &message.content {
        Content::Parts(parts) => match parts.as_slice() {
            [
                ContentPart::ToolResult {
                    call_id,
                    output,
                    is_error,
                    ..
                },
            ] => (output.clone(), Some(call_id.to_string()), *is_error),
            _ => message_body_parts(message).unwrap_or_default(),
        },
        Content::Text(_) => message_body_parts(message).unwrap_or_default(),
    };
    ProviderMessage {
        role: wire_role_of(message.role),
        content,
        tool_calls: message
            .tool_calls
            .iter()
            .filter_map(|call| wire_tool_call(call).ok())
            .collect(),
        tool_call_id: tool_call_id.and_then(|call_id| super::scalar::CallId::new(call_id).ok()),
        is_error,
        tokens: None,
    }
}

fn tool_schema(schema: &crate::types::message::ToolSchema) -> WireToolSchema {
    WireToolSchema {
        name: schema.name.to_string(),
        description: schema.description.clone(),
        parameters: super::scalar::BoundedJson::new(schema.parameters.clone())
            .unwrap_or_else(|_| Default::default()),
    }
}

fn workflow_budget(budget: &crate::orchestration::workflow::WorkflowBudget) -> WireWorkflowBudget {
    WireWorkflowBudget {
        max_total_tokens: budget.tokens_max.map(WireU64::new),
        max_turns: None,
        max_concurrency: budget.max_concurrent_subagents.map(|max| max as u32),
    }
}

fn sub_agent_result(completed: &ChildCompleted) -> SubAgentResult {
    let termination = match completed.result.status {
        ChildStatus::Completed => TerminationReason::Completed,
        ChildStatus::Failed => TerminationReason::Error,
        ChildStatus::Cancelled => TerminationReason::UserAbort,
    };
    SubAgentResult {
        agent_id: completed.task_id.as_str().into(),
        result: LoopResult {
            termination,
            final_message: completed
                .result
                .output
                .as_ref()
                .map(|text| CoreMessage::assistant(text.clone())),
            turns_used: completed
                .result
                .usage
                .as_ref()
                .and_then(|usage| usage.turns)
                .unwrap_or(0),
            total_tokens_used: completed
                .result
                .usage
                .as_ref()
                .map_or(0, child_total_tokens),
            loop_continue: None,
            classify_branch: None,
            pace_decision: None,
            tournament_winner: None,
        },
    }
}

/// The attempt's total token spend: the host's observed total, else the sum of the split.
fn child_total_tokens(usage: &super::event::UsageFacts) -> u64 {
    usage.total_tokens.map(WireU64::get).unwrap_or_else(|| {
        usage
            .input_tokens
            .map_or(0, WireU64::get)
            .saturating_add(usage.output_tokens.map_or(0, WireU64::get))
    })
}

fn attempt_ordinal(attempt_id: &AttemptId) -> Option<u32> {
    attempt_id.as_str().rsplit(':').next()?.parse().ok()
}

fn supervision_label(policy: crate::scheduler::tcb::ChildFailurePolicy) -> &'static str {
    match policy {
        crate::scheduler::tcb::ChildFailurePolicy::Propagate => "propagate",
        crate::scheduler::tcb::ChildFailurePolicy::Isolate => "isolate",
        crate::scheduler::tcb::ChildFailurePolicy::Restart => "restart",
        crate::scheduler::tcb::ChildFailurePolicy::Retry => "retry",
        crate::scheduler::tcb::ChildFailurePolicy::Ignore => "ignore",
    }
}

/// §7.12 · how an agent loop's own termination reason becomes an operation terminal.
///
/// The internal vocabulary has two reasons the wire's `TerminationReason` deliberately does not
/// carry: `user_abort` **is** a `Cancelled` terminal and `error` **is** a `Failed` one. Folding
/// either back into `Completed` would give the same event two representations, which is exactly
/// what the canonical union removed.
fn agent_terminal(result: &LoopResult) -> KernelTerminal {
    let usage = UsageReport {
        input_tokens: WireU64::new(result.total_tokens_used),
        output_tokens: WireU64::ZERO,
        turns: result.turns_used,
        cached_input_tokens: None,
    };
    let termination = match result.termination {
        TerminationReason::Completed => WireTermination::Completed,
        TerminationReason::MaxTurns => WireTermination::MaxTurns,
        TerminationReason::TokenBudget => WireTermination::TokenBudget,
        TerminationReason::Timeout => WireTermination::Deadline,
        TerminationReason::ContextOverflow => WireTermination::ContextOverflow,
        TerminationReason::NoProgress => WireTermination::NoProgress,
        TerminationReason::MilestoneExceeded => WireTermination::MilestoneExceeded,
        TerminationReason::UserAbort => {
            return KernelTerminal::Cancelled(CancelledTerminal {
                reason: CancellationReason::User,
                usage,
            });
        }
        TerminationReason::Error => {
            return KernelTerminal::Failed(FailedTerminal {
                failure: KernelFailure {
                    code: KernelFailureCode::InvariantViolated,
                    message: "the agent loop ended in an error state".to_string(),
                },
                usage,
            });
        }
    };
    KernelTerminal::Agent(AgentTerminal {
        result: WireLoopResult {
            termination,
            final_message: result.final_message.as_ref().map(provider_message),
            turns_used: result.turns_used,
            pace_decision: result.pace_decision.as_ref().map(|decision| {
                super::terminal::PaceDecision {
                    action: match decision.action {
                        CorePaceAction::Continue => super::terminal::PaceAction::Continue,
                        CorePaceAction::Sleep => super::terminal::PaceAction::Sleep,
                        CorePaceAction::Stop => super::terminal::PaceAction::Stop,
                    },
                    delay_ms: decision.delay_ms.map(WireU64::new),
                    reason: decision.reason.clone(),
                    coerced_from: decision.coerced_from.clone(),
                }
            }),
        },
        usage,
    })
}

fn publishes(disposition: &StepDisposition, tag: EffectKindTag) -> bool {
    disposition
        .effects()
        .iter()
        .any(|effect| effect.tag() == tag)
}

fn loop_action_label(action: &LoopAction) -> &'static str {
    match action {
        LoopAction::CallLLM { .. } => "call_provider",
        LoopAction::ExecuteTools { .. } => "execute_tools",
        LoopAction::RequestApproval { .. } => "request_approval",
        LoopAction::SpawnWorkflow { .. } => "spawn_tasks",
        LoopAction::PreemptSubAgents { .. } => "preempt_tasks",
        LoopAction::PersistMemory { .. } => "persist_memory",
        LoopAction::QueryMemory { .. } => "query_memory",
        LoopAction::ArchivePageOut { .. } => "archive_page_out",
        LoopAction::EvaluateMilestone { .. } => "evaluate_milestone",
        LoopAction::Done { .. } => "terminal",
        LoopAction::AwaitingResume => "awaiting_resume",
    }
}

/// The model-facing answer to a P1 syscall the kernel executed.
///
/// 下一请求信息最大化: each says what happened *and* where the consequence will show up, so the
/// model's next turn does not have to guess whether a control-plane call took effect.
fn syscall_ack(name: &str) -> &'static str {
    match name {
        "start_workflow" => {
            "workflow accepted: its ready nodes are scheduled; each result arrives as that node \
             completes"
        }
        "submit_workflow_nodes" => {
            "nodes appended to the running workflow; each result arrives as that node completes"
        }
        "skill" => "skill activated: its guidance and tools are in this turn's context",
        "update_plan" => "plan updated: the new state renders in [TASK STATE] from here on",
        crate::context::manager::MEMORY_TOOL_NAME => {
            "memory search issued: matching records are added to this conversation before your \
             next turn"
        }
        crate::context::manager::READ_RESULT_TOOL_NAME => "page-in requested",
        "send_message" | "publish_channel" => "local handle routed",
        "receive_mailbox" | "receive_channel" | "read_object" => "local state returned",
        _ => "accepted",
    }
}

fn validate_ipc_labels(message_id: &str, kind: &str) -> Result<(), SyscallRefusal> {
    if message_id.is_empty() || kind.is_empty() || message_id.len() > 256 || kind.len() > 256 {
        return Err(SyscallRefusal::Rejected(SyscallRejection::new(
            "local_ipc",
            "message_id and message_kind must contain 1..=256 bytes",
        )));
    }
    Ok(())
}

fn resolve_ipc_handle(
    engine: &LoopStateMachine,
    handle_id: &super::scalar::HandleId,
) -> Result<crate::mm::handle::Handle, SyscallRefusal> {
    engine
        .ctx
        .handles
        .all()
        .iter()
        .find(|handle| {
            handle.source.as_deref() == Some(handle_id.as_str())
                || handle.id.to_string() == handle_id.as_str()
        })
        .cloned()
        .ok_or_else(|| {
            SyscallRefusal::Rejected(SyscallRejection::new(
                "local_ipc",
                format!("payload handle {handle_id} is not reachable by this operation"),
            ))
        })
}

fn local_ipc_refusal(error: crate::scheduler::tcb::LocalIpcError) -> SyscallRefusal {
    let reason = match error {
        crate::scheduler::tcb::LocalIpcError::UnknownCaller => "unknown caller",
        crate::scheduler::tcb::LocalIpcError::CallerTerminal => "caller is terminal",
        crate::scheduler::tcb::LocalIpcError::UnknownRecipient => "unknown recipient",
        crate::scheduler::tcb::LocalIpcError::ChannelSubscribersMismatch => {
            "channel subscriber set is immutable"
        }
        crate::scheduler::tcb::LocalIpcError::NotSubscriber => "caller is not a channel subscriber",
        crate::scheduler::tcb::LocalIpcError::Full => "IPC capacity is full",
        crate::scheduler::tcb::LocalIpcError::Expired => "message TTL already expired",
        crate::scheduler::tcb::LocalIpcError::ObjectConflict => {
            "object id already names a different descriptor"
        }
    };
    SyscallRefusal::Rejected(SyscallRejection::new("local_ipc", reason))
}

fn local_ipc_outcome(accepted: bool) -> SyscallOutcome {
    SyscallOutcome {
        ack: Some(
            serde_json::json!({
                "status": if accepted { "accepted" } else { "duplicate" },
            })
            .to_string(),
        ),
        ..SyscallOutcome::default()
    }
}

fn ipc_messages_outcome(messages: &[crate::scheduler::mailbox::MailboxMessage]) -> SyscallOutcome {
    SyscallOutcome {
        ack: Some(
            serde_json::to_string(messages)
                .expect("canonical mailbox messages are always serializable"),
        ),
        ..SyscallOutcome::default()
    }
}

/// Wire → semantic projections for the resolution half.
fn core_provider_message(message: &ProviderMessage) -> Result<CoreMessage, KernelFault> {
    Ok(CoreMessage {
        role: core_role_of(message.role),
        content: Content::Text(message.content.clone()),
        tool_calls: message.tool_calls.iter().map(core_tool_call).collect(),
    })
}

fn core_tool_call(call: &WireToolCall) -> crate::types::message::ToolCall {
    crate::types::message::ToolCall {
        id: call.call_id.as_str().into(),
        name: call.name.as_str().into(),
        arguments: call.arguments.get().clone(),
    }
}

fn wire_tool_call(call: &crate::types::message::ToolCall) -> Result<WireToolCall, KernelFault> {
    Ok(WireToolCall {
        call_id: super::scalar::CallId::new(call.id.as_str()).map_err(malformed)?,
        name: call.name.to_string(),
        arguments: super::scalar::BoundedJson::new(call.arguments.clone())
            .unwrap_or_else(|_| Default::default()),
    })
}

fn wire_approval_request(
    request: &crate::scheduler::state_machine::ApprovalRequest,
) -> Result<WireApprovalRequest, KernelFault> {
    Ok(WireApprovalRequest {
        call_id: super::scalar::CallId::new(request.call_id.as_str()).map_err(malformed)?,
        tool_name: request.tool.clone(),
        arguments: super::scalar::BoundedJson::new(request.arguments.clone())
            .unwrap_or_else(|_| Default::default()),
        reason: (!request.reason.is_empty()).then(|| request.reason.clone()),
    })
}

/// §7.10 · one returned tool result.
///
/// Both arms produce the same thing: the text that enters working context. For `Inline` that is the
/// body; for `External` it is the preview, and the body never crosses core at all — the host
/// persisted it before submitting, and the kernel holds only the reference the
/// [`ToolsSuccess`](super::effect::ToolsSuccess) carried. The residency transfer that records
/// *where* the body went happens after the engine has accepted the batch (see
/// `record_external_payloads`), because the handle it moves does not exist until the result is in
/// history.
///
/// The canonical [`ToolResultDisposition`] is binary, so the projection onto core's historical
/// `is_fatal` + six-way `ToolErrorKind` is total and lossless in the direction that matters: only
/// `Recoverable` and `Fatal` are reachable, and `UserInterrupt` — the one kind that still rolls a
/// turn back — has no canonical spelling at all. Cancellation travels on `HostControl::Cancel`
/// (§7.9), so that retired retry rung is not re-expressible here.
///
/// §7.10 rule 9 · failure is orthogonal to residency, so the two failure facts are read through
/// [`WireToolResultPayload::disposition`] / [`WireToolResultPayload::is_error`] and land in core
/// identically for both arms. A tool that failed *and* produced a body over the inline threshold —
/// the common shape, not a rare one — is now expressible, and its fatality reaches the batch
/// close-out on the same path an inline one does.
fn core_tool_result(payload: &WireToolResultPayload) -> ToolResult {
    let disposition = payload.disposition();
    let is_error = payload.is_error();
    let error_kind = match disposition {
        ToolResultDisposition::Fatal => Some(ToolErrorKind::Fatal),
        ToolResultDisposition::Recoverable => is_error.then_some(ToolErrorKind::Recoverable),
    };
    match payload {
        WireToolResultPayload::Inline(inline) => ToolResult {
            call_id: inline.call_id.as_str().into(),
            output: Content::Text(inline.result.output.clone()),
            durable_content: inline.result.durable_content.clone(),
            is_error,
            is_fatal: disposition.is_fatal(),
            error_kind,
        },
        WireToolResultPayload::External(external) => ToolResult {
            call_id: external.call_id.as_str().into(),
            output: Content::Text(external.preview.clone()),
            durable_content: None,
            is_error,
            is_fatal: disposition.is_fatal(),
            error_kind,
        },
    }
}

/// §7.10 rules 1, 2 and 5 · the configured threshold is the **arbiter** of which arm a result may
/// take, checked before the engine sees anything.
///
/// `PayloadPolicy::inline_threshold_bytes` documents a total partition — "results at or above this
/// size are committed as `External` rather than inline" — so both directions are enforced here:
///
/// - an oversized `Inline` is refused rather than externalised by the kernel. The host must persist
///   before submission, so "reject" is the only answer that keeps rule 5 true.
/// - an undersized `External` is refused too, because it costs a `LoadPayload` round trip to read
///   something that would have fitted in the turn that produced it, and it makes the partition —
///   the one thing a host has to agree with the kernel about — untotal.
///
/// The digest must be one this kernel can *verify*: a page-in is checked by recomputing the digest
/// over the returned body, so a foreign algorithm would admit a payload whose restoration could
/// never be proved. The preview is bounded because it is the part that actually occupies context.
fn check_payload_policy(
    payload: &WireToolResultPayload,
    policy: &super::config::ResolvedPayloadPolicy,
) -> Result<(), KernelFault> {
    let threshold = policy.inline_threshold_bytes as u64;
    match payload {
        WireToolResultPayload::Inline(inline) => {
            let durable_size = inline
                .result
                .durable_content
                .as_ref()
                .map(|content| {
                    content.validate().map_err(|error| {
                        KernelFault::new(
                            KernelFaultCode::MalformedEnvelope,
                            format!(
                                "inline tool result {} carries invalid durable content: {error}",
                                inline.call_id
                            ),
                        )
                    })?;
                    serde_json::to_vec(content).map(|bytes| bytes.len() as u64).map_err(|error| {
                        KernelFault::new(
                            KernelFaultCode::MalformedEnvelope,
                            format!(
                                "inline tool result {} durable content cannot be encoded: {error}",
                                inline.call_id
                            ),
                        )
                    })
                })
                .transpose()?
                .unwrap_or(0);
            let size = (inline.result.output.len() as u64).max(durable_size);
            if size >= threshold {
                return Err(KernelFault::new(
                    KernelFaultCode::ResourceLimitExceeded,
                    format!(
                        "tool result {} is {size} bytes and this operation's payload policy \
                         externalises at {threshold}; the host persists the body and submits an \
                         external result — the kernel does not spool on its behalf (§7.10)",
                        inline.call_id
                    ),
                ));
            }
            Ok(())
        }
        WireToolResultPayload::External(external) => {
            if !is_verifiable_digest(external.digest.as_str()) {
                return Err(KernelFault::new(
                    KernelFaultCode::MalformedEnvelope,
                    format!(
                        "external tool result {} carries digest {}, which this kernel cannot \
                         verify; a paged-in body is checked by recomputing {}:<64 hex> over it",
                        external.call_id,
                        external.digest,
                        super::record::DIGEST_ALGORITHM
                    ),
                ));
            }
            let size = external.original_size.get();
            if size < threshold {
                return Err(KernelFault::new(
                    KernelFaultCode::MalformedEnvelope,
                    format!(
                        "external tool result {} declares {size} bytes but this operation's \
                         payload policy inlines below {threshold}; the threshold is the single \
                         arbiter of which arm a result takes (§7.10)",
                        external.call_id
                    ),
                ));
            }
            let preview = external.preview.len() as u64;
            if preview > policy.preview_bytes as u64 {
                return Err(KernelFault::new(
                    KernelFaultCode::ResourceLimitExceeded,
                    format!(
                        "external tool result {} carries a {preview}-byte preview and this \
                         operation keeps {} bytes resident",
                        external.call_id, policy.preview_bytes
                    ),
                ));
            }
            Ok(())
        }
    }
}

/// Whether `digest` is a digest this kernel can recompute — `sha256:` plus 64 lowercase hex.
fn is_verifiable_digest(digest: &str) -> bool {
    let Some(hex) = digest.strip_prefix(super::record::DIGEST_ALGORITHM) else {
        return false;
    };
    let Some(hex) = hex.strip_prefix(':') else {
        return false;
    };
    hex.len() == 64
        && hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn core_milestone_result(
    result: &super::effect::MilestoneCheckResult,
) -> crate::types::milestone::MilestoneCheckResult {
    crate::types::milestone::MilestoneCheckResult {
        phase_id: result.phase_id.clone(),
        passed: result.passed,
        reason: (!result.passed).then(|| {
            if result.failed_criteria.is_empty() {
                result.notes.clone()
            } else {
                format!("unmet criteria: {}", result.failed_criteria.join("; "))
            }
        }),
    }
}

/// Project a wire contract skeleton onto the engine's phase cascade.
///
/// The skeleton carries the two things core decides — phase order and unlocks — and nothing else,
/// so the projection fills the rest from the engine's own defaults: no criteria (the host owns
/// them, §5.2), the default `HarnessEval` verifier, unlimited retries, terminate-on-exhaustion.
/// The one lookup here is `unlocks` → capability descriptor, and it cannot fail: `resolve` already
/// proved every id names a declared tool or skill, so the fallback marker is unreachable and
/// exists only to keep the projection total.
fn core_milestone_contract(
    contract: &super::config::VerificationContract,
    config: &ResolvedOperationConfig,
) -> crate::types::milestone::MilestoneContract {
    use crate::types::capability::{CapabilityDescriptor, CapabilityKind as CoreCapabilityKind};
    use crate::types::milestone::{MilestoneContract, MilestonePhase};

    let mut cascade = MilestoneContract::new();
    for phase in &contract.phases {
        let unlocks = phase
            .unlocks
            .iter()
            .map(|id| {
                if let Some(tool) = config.tool_catalog.iter().find(|tool| &tool.name == id) {
                    CapabilityDescriptor::tool(core_tool_schema(tool))
                } else if let Some(skill) = config.skill_catalog.iter().find(|s| &s.name == id) {
                    CapabilityDescriptor::skill(core_skill(skill))
                } else {
                    CapabilityDescriptor::marker(
                        CoreCapabilityKind::Tool,
                        id.as_str(),
                        String::new(),
                    )
                }
            })
            .collect();
        cascade = cascade.phase(MilestonePhase {
            unlocks,
            ..MilestonePhase::new(phase.phase_id.clone())
        });
    }
    cascade
}

fn core_memory_kind(kind: WireMemoryKind) -> crate::mm::memory::MemoryKind {
    match kind {
        WireMemoryKind::User => crate::mm::memory::MemoryKind::User,
        WireMemoryKind::Feedback => crate::mm::memory::MemoryKind::Feedback,
        WireMemoryKind::Project => crate::mm::memory::MemoryKind::Project,
        WireMemoryKind::Reference => crate::mm::memory::MemoryKind::Reference,
    }
}

fn wire_memory_kind_label(kind: WireMemoryKind) -> &'static str {
    core_memory_kind(kind).label()
}

/// The canonical memory binding is **opaque** (§7.8): it is not a tenant, not a namespace and not a
/// path. Host-facing observations still need to say which binding a fact belongs to, so the binding
/// id rides in the namespace slot and the tenant stays empty — the kernel derives no tenant because
/// the contract gives it none.
fn binding_scope(binding_id: &MemoryBindingId) -> crate::mm::memory::MemoryScope {
    crate::mm::memory::MemoryScope::new(String::new(), binding_id.as_str().to_string())
}

/// The audit text of a host executor failure. Classification first, host prose second — a kernel
/// decision was already taken on the kind alone (§7.9), and this is only what the operator reads.
fn host_failure_text(failure: &HostEffectFailure) -> String {
    if failure.message.is_empty() {
        failure.kind.as_str().to_string()
    } else {
        format!("{}: {}", failure.kind.as_str(), failure.message)
    }
}

/// A resolution for an effect this driver has no record of authoring. The transaction already
/// refuses one for an effect that is not pending, so reaching this means the driver's own ledger
/// and the journal disagree — a rebuild-from-records failure, not a host protocol error.
fn unowned_resolution(effect_id: &EffectId, what: &str) -> KernelFault {
    KernelFault::new(
        KernelFaultCode::RecordCorrupted,
        format!(
            "effect {effect_id} resolves a {what} this runtime never authored; the driver's ledger \
             no longer describes the journal — rebuild from the records"
        ),
    )
}

fn truncate_on_char_boundary(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

// ---------------------------------------------------------------------------------------------
// engine construction from the resolved configuration
// ---------------------------------------------------------------------------------------------

/// Build the semantic kernel this operation runs on, from the configuration its genesis record
/// froze. Nothing here reads a compile-time default: every value comes off the record, which is
/// what makes a rebuild on a newer binary reproduce the same steps (§15.2).
fn build_engine(config: &ResolvedOperationConfig) -> LoopStateMachine {
    let execution = &config.execution_policy;
    let mut engine = LoopStateMachine::new(SchedulerBudget {
        max_tokens: execution.max_context_tokens,
        max_turns: execution.max_turns,
        max_total_tokens: execution.max_total_tokens.get(),
        max_wall_ms: execution.max_wall_ms.map(WireU64::get),
    });
    if let Some(grant) = config.budget_grant.clone() {
        engine.set_budget_grant(grant);
    }
    let scheduler_policy = config.scheduler_policy;
    engine.set_scheduler_policy(crate::scheduler::policy::SchedulerPolicyConfig {
        critical_path_weight: i64::from(scheduler_policy.critical_path_weight),
        fanout_weight: i64::from(scheduler_policy.fanout_weight),
        age_weight: i64::from(scheduler_policy.age_weight),
        token_cost_weight: i64::from(scheduler_policy.token_cost_weight),
        deadline_weight: i64::from(scheduler_policy.deadline_weight),
        process_priority_weight: i64::from(scheduler_policy.process_priority_weight),
        resource_pressure_weight: i64::from(scheduler_policy.resource_pressure_weight),
        budget_pressure_weight: i64::from(scheduler_policy.budget_pressure_weight),
    });

    engine.set_criteria_gate(execution.criteria_gate_enabled);
    engine.set_repeat_fuse(crate::governance::repeat_fuse::RepeatFuseConfig {
        enabled: execution.repeat_fuse.enabled,
        deny_after: execution.repeat_fuse.deny_after,
        terminate_after: execution.repeat_fuse.terminate_after,
    });
    engine.set_entropy_watch(crate::scheduler::entropy::EntropyWatchConfig {
        enabled: execution.entropy_watch.enabled,
        threshold: f64::from(execution.entropy_watch.threshold_ppm.get()) / 1_000_000.0,
        hysteresis: f64::from(execution.entropy_watch.hysteresis_ppm.get()) / 1_000_000.0,
        cooldown_turns: execution.entropy_watch.cooldown_turns,
        notify_model: execution.entropy_watch.notify_model,
    });
    install_live_policies(&mut engine, config);
    engine
        .ctx
        .set_memory_enabled(config.feature_policy.memory_enabled);
    engine
        .ctx
        .set_knowledge_enabled(config.feature_policy.knowledge_enabled);
    engine
        .ctx
        .set_plan_tool_enabled(config.feature_policy.plan_tool_enabled);
    // §7.6 · the declared skill catalog is what makes `ActivateSkill` checkable: a name outside it
    // is a capability mutation with nothing behind it.
    engine
        .ctx
        .set_available_skills(config.skill_catalog.iter().map(core_skill).collect());
    engine.ctx.set_stable_core_tools(
        config
            .feature_policy
            .stable_core_tool_ids
            .iter()
            .map(|id| id.as_str().into()),
    );
    engine.ctx.config.knowledge_budget_ratio =
        config.context_policy.knowledge_budget_ppm.as_ratio();
    engine.ctx.config.collapse_assistant_narration =
        config.context_policy.collapse_old_assistant_narration;
    engine.tools = config.tool_catalog.iter().map(core_tool_schema).collect();
    engine
}

/// Install the four §13.2 live-mutable policies onto an engine.
///
/// One installer, two callers: the genesis build and `HostCommand::ApplyPolicyPatch`. That is the
/// whole reason it exists — a patched policy that took a different code path into the engine than
/// the booted one is how "the same configuration means two things" starts.
///
/// A policy the operation never declared is deliberately **not** installed: §7.3's "the host never
/// said" is a value, distinct from an all-permissive policy the host did not state.
fn install_live_policies(engine: &mut LoopStateMachine, config: &ResolvedOperationConfig) {
    // §7.6 · the P1 gate is only a gate if the operation's declared caps actually reach it. Without
    // this the trap would allow every syscall on the canonical path regardless of what the genesis
    // record froze.
    if let Some(quota) = core_quota(&config.resource_quota) {
        engine.set_resource_quota(quota);
    }
    // The same argument for the tool gate: a governance policy the genesis record froze but the
    // engine never installed would make `RequestApproval` unpublishable and every declared rule
    // inert.
    if let Some(pipeline) = core_governance(&config.governance_policy) {
        engine.set_governance(pipeline);
    }
    engine.set_signal_policy(
        config.signal_policy.queue_max as usize,
        config.signal_policy.ttl_ms.map(WireU64::get),
        config.signal_policy.deadline_escalation,
    );
    // The two semantic ladders. Before this existed the resolved recovery policy was frozen into
    // the genesis record and then never reached the engine at all, so both the booted policy and
    // `ReplaceRecoveryPolicy` were inert and the engine's own compile-time defaults decided how
    // long a ladder ran — the exact "the record says one thing, the run does another" drift §15.2
    // forbids.
    engine.set_recovery_limits(
        config.recovery_policy.provider_recovery_attempts,
        config.recovery_policy.output_recovery_attempts,
    );
}

/// `None` when the operation declared no axis at all. §7.3: "the host never said" is a value, and
/// it is *not* the same as an all-uncapped quota — an installed quota makes the workflow budget
/// observable, which is a statement the host did not make.
fn core_quota(
    quota: &super::config::ResourceQuota,
) -> Option<crate::governance::quota::ResourceQuota> {
    if quota == &super::config::ResourceQuota::default() {
        return None;
    }
    Some(crate::governance::quota::ResourceQuota {
        max_concurrent_subagents: quota.max_concurrent_subagents,
        max_total_subagents: quota.max_total_subagents,
        max_spawn_depth: quota.max_spawn_depth,
        memory_writes_per_window: quota
            .memory_writes_per_window
            .as_ref()
            .map(|window| (window.max_events, window.window_ms.get())),
        max_workflow_nodes: quota.max_workflow_nodes.map(|max| max as usize),
    })
}

/// `None` when the operation declared no governance at all. Same "the host never said" rule as
/// [`core_quota`]: an installed all-allow pipeline is a statement the host did not make, and it
/// would silently change what a tool call means (every call would pass a gate that does not exist).
fn core_governance(
    policy: &super::config::ResolvedGovernancePolicy,
) -> Option<crate::governance::pipeline::GovernancePipeline> {
    use super::command::{ParamConstraint as WireConstraint, PolicyAction};
    use crate::governance::constraint::{ConstraintRule, ParamConstraint as CoreConstraint};
    use crate::governance::permission::PermissionRule;
    use crate::governance::rate_limit::RateLimit;

    if policy.default_action == PolicyAction::Allow
        && !policy.hide_denied_tools
        && policy.rules.is_empty()
        && policy.vetoed_tools.is_empty()
        && policy.rate_limits.is_empty()
        && policy.constraints.is_empty()
    {
        return None;
    }
    let mut pipeline = crate::governance::pipeline::GovernancePipeline::new(core_policy_action(
        policy.default_action,
    ));
    pipeline.hide_denied_tools = policy.hide_denied_tools;
    for rule in &policy.rules {
        pipeline.permission.add_rule(PermissionRule {
            tool_pattern: rule.tool_pattern.as_str().into(),
            action: core_policy_action(rule.action),
        });
    }
    for tool in &policy.vetoed_tools {
        pipeline.veto.block_tool(tool.clone());
    }
    for limit in &policy.rate_limits {
        pipeline.rate_limiter.set_limit(
            limit.tool.clone(),
            RateLimit {
                max_calls: limit.max_calls,
                window_ms: limit.window_ms.get(),
            },
        );
    }
    for constraint in &policy.constraints {
        let rule = match constraint {
            WireConstraint::Required(_) => ConstraintRule::Required,
            WireConstraint::Enum(spec) => ConstraintRule::Enum(spec.values.clone()),
            // §7.1.1 · the wire carries fixed-point micro-units so a bound is replayable; the
            // validator's own arithmetic is float, and this is the single conversion point.
            WireConstraint::Range(spec) => ConstraintRule::Range {
                min: spec.min_micros.map(|micros| micros as f64 / 1_000_000.0),
                max: spec.max_micros.map(|micros| micros as f64 / 1_000_000.0),
            },
        };
        pipeline.constraints.add(CoreConstraint {
            tool_name: constraint.tool().to_string(),
            param_path: constraint.param_path().to_string(),
            rule,
        });
    }
    Some(pipeline)
}

fn core_policy_action(
    action: super::command::PolicyAction,
) -> crate::governance::permission::PermissionAction {
    use crate::governance::permission::PermissionAction;
    match action {
        super::command::PolicyAction::Allow => PermissionAction::Allow,
        super::command::PolicyAction::Deny => PermissionAction::Deny,
        super::command::PolicyAction::AskUser => PermissionAction::AskUser,
    }
}

fn core_skill(skill: &super::config::SkillMetadata) -> crate::types::skill::SkillMetadata {
    crate::types::skill::SkillMetadata {
        name: skill.name.as_str().into(),
        description: skill.description.clone(),
        when_to_use: skill.when_to_use.clone(),
        allowed_tools: skill
            .allowed_tools
            .iter()
            .map(|tool| tool.as_str().into())
            .collect(),
        capability_grants: skill.capability_grants.clone(),
        effort: skill.effort,
        estimated_tokens: skill.estimated_tokens.unwrap_or(0),
    }
}

fn ensure_skill_grants_are_attenuated(
    grants: &[crate::types::capability::Capability],
    parent_capabilities: &[crate::types::capability::Capability],
) -> Result<(), Vec<crate::types::capability::Capability>> {
    crate::types::capability::caps_subset(grants, parent_capabilities)
}

fn skill_grant_attenuation_message(
    skill_name: &str,
    violations: &[crate::types::capability::Capability],
) -> String {
    format!(
        "skill {skill_name:?} declares capability grants that would widen the mounting agent's authority: {}",
        violations
            .iter()
            .map(|capability| capability.id.0.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn core_tool_schema(schema: &WireToolSchema) -> crate::types::message::ToolSchema {
    crate::types::message::ToolSchema {
        name: schema.name.as_str().into(),
        description: schema.description.clone(),
        parameters: schema.parameters.get().clone(),
    }
}

fn malformed(error: super::scalar::WireScalarError) -> KernelFault {
    KernelFault::new(KernelFaultCode::MalformedEnvelope, error.message)
}

#[cfg(test)]
mod tests;
