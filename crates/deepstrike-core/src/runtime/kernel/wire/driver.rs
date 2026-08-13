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

use serde::Serialize;

use super::checkpoint::{
    AuthoredMemoryQueryState, AuthoredMemoryWriteState, ChildProcessState, ContextVmStateV1,
    EntropyState, EntropyTurnState, HandleState, InlineMessageBody, KnowledgeSlotState,
    LocalChannelState, LogicalCompressionEntry, LogicalKernelState, LogicalPlanStep,
    LogicalStateProjection, LogicalTaskState, LogicalToolCall, MessagePartition, MilestoneState,
    PartitionTokenState, PendingPayloadLoadState, PendingProviderCallState, QueuedSignalState,
    ReferencedMessageBody, SchedulerStateV1, SkillLeaseState, StoredMessageBody,
    StoredMessageState, StructuredMessageBody, SyscallStateV1, TaskAttemptState, TaskControlState,
    TaskWaitConditionState, TaskWaitSetState, WorkflowGraphState, WorkflowNodeState,
};
use super::command::{
    ApplyCapabilityPatchCommand, ApplyKnowledgeMutationCommand, ApplyPolicyPatchCommand,
    ApplySkillActivationCommand, CancelCommand, CancellationReason, HostCommand, LivePolicyState,
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
    SignalFilter, SubscriptionId, TaskLifecycle, Tcb, WaitCondition, WaitMode, WaitReason,
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
use crate::types::message::{Content, ContentPart, Message, Role, ToolErrorKind, ToolResult};
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
#[derive(Debug, Clone, Serialize)]
pub struct PlannedStep {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_kind: Option<RootKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focus: Option<ExecutionFocus>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
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

/// The session slot the canonical path fills on the legacy `AgentIdentity` that `AgentRunSpec`
/// still declares (§22.6 · Task 11): nothing. Host session identity is not a kernel fact, the
/// canonical wire has no field for one, and the field itself is deleted with the legacy input enum
/// in Task 23 — until then the canonical path must be the proof that it is never populated.
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
fn message_body_parts(message: &Message) -> Option<(String, Option<String>, bool)> {
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
    let content = DurableContent {
        schema_version: DurableContent::CURRENT_SCHEMA_VERSION,
        blocks,
    };
    content.validate().map_err(|error| error.to_string())?;
    Ok(content)
}

fn content_part_to_durable(part: &ContentPart) -> Result<DurableContentBlock, String> {
    match part {
        ContentPart::Text { text } => Ok(DurableContentBlock::Text { text: text.clone() }),
        ContentPart::ToolResult { .. } => Err(
            "a structured message cannot embed a tool result; durable tool results use their separate envelope".into(),
        ),
        ContentPart::Image { url, data, media_type, detail } => {
            let source = match (url, data) {
                (Some(url), None) => DurableSource::Url { url: url.clone() },
                (None, Some(data)) => DurableSource::Base64 { data: data.clone() },
                _ => return Err("image must have exactly one durable url or base64 source".into()),
            };
            let provider_options = detail
                .as_ref()
                .map(|detail| serde_json::json!({ "detail": detail }));
            Ok(DurableContentBlock::Image {
                source,
                media_type: media_type.clone(),
                provider_options,
            })
        }
        ContentPart::Audio { data, media_type } => Ok(DurableContentBlock::Audio {
            source: DurableSource::Base64 { data: data.clone() },
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
    // A plain text-only ToolResult remains on the legacy checkpoint carrier. The structured
    // envelope is opted into only by an explicit durable_content field.
    durable_content.as_ref()?;
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
    let has_durable_content = parts.iter().any(|part| {
        matches!(
            part,
            ContentPart::ToolResult {
                durable_content: Some(_),
                ..
            }
        )
    });
    if !has_durable_content {
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
            schema_version: content.schema_version,
            call_id: call_id.to_owned(),
            is_error,
            blocks: content.blocks.clone(),
        },
        None => DurableToolResult::legacy_text(call_id.to_owned(), output.to_owned(), is_error),
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
            schema_version: result.schema_version,
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
                url: Some(url.clone()),
                data: None,
                media_type: media_type.clone(),
                detail: provider_options
                    .as_ref()
                    .and_then(|value| value.get("detail"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
            }),
            DurableSource::Base64 { data } => Ok(ContentPart::Image {
                url: None,
                data: Some(data.clone()),
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
            data: data.clone(),
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

#[cfg(test)]
mod durable_content_checkpoint_tests {
    use super::*;

    #[test]
    fn structured_checkpoint_body_uses_durable_content_and_restores_media() {
        let content = Content::Parts(vec![
            ContentPart::Text {
                text: "caption".into(),
            },
            ContentPart::Image {
                url: None,
                data: Some("aW1hZ2U=".into()),
                media_type: Some("image/png".into()),
                detail: Some("low".into()),
            },
        ]);
        let durable = content_to_durable(&content).unwrap();
        let restored = content_from_durable(&durable).unwrap();
        assert_eq!(
            serde_json::to_value(restored).unwrap(),
            serde_json::to_value(content).unwrap()
        );
    }

    #[test]
    fn checkpoint_rejects_unrestorable_durable_file_source() {
        let content = DurableContent {
            schema_version: 1,
            blocks: vec![DurableContentBlock::File {
                source: DurableSource::FileId {
                    id: "file-1".into(),
                    affinity: crate::types::durable_content::EndpointAffinity {
                        provider_id: "provider".into(),
                        endpoint_id: "endpoint".into(),
                    },
                },
                media_type: Some("application/pdf".into()),
                provider_options: None,
            }],
        };
        assert!(content_from_durable(&content).is_err());
    }

    #[test]
    fn structured_tool_result_checkpoint_form_keeps_correlation_and_blocks() {
        let result = DurableToolResult {
            schema_version: 1,
            call_id: "call-screenshot".into(),
            is_error: false,
            blocks: vec![
                DurableContentBlock::Text {
                    text: "captured".into(),
                },
                DurableContentBlock::Image {
                    source: DurableSource::Base64 {
                        data: "aW1hZ2U=".into(),
                    },
                    media_type: Some("image/png".into()),
                    provider_options: None,
                },
                DurableContentBlock::File {
                    source: DurableSource::FileId {
                        id: "file-7".into(),
                        affinity: crate::types::durable_content::EndpointAffinity {
                            provider_id: "openai".into(),
                            endpoint_id: "responses".into(),
                        },
                    },
                    media_type: Some("application/pdf".into()),
                    provider_options: None,
                },
            ],
        };
        let content = content_from_durable_tool_result(&result).unwrap();
        let durable = durable_tool_result_from_content(&content).unwrap();
        assert_eq!(durable, result);
        let Content::Parts(parts) = content else {
            panic!("tool result must restore as parts")
        };
        let [
            ContentPart::ToolResult {
                call_id,
                output,
                durable_content,
                ..
            },
        ] = parts.as_slice()
        else {
            panic!("tool result must have one correlated part")
        };
        assert_eq!(call_id.as_str(), "call-screenshot");
        assert_eq!(output, "captured");
        assert_eq!(durable_content.as_ref().unwrap().blocks, result.blocks);
    }

    #[test]
    fn legacy_multi_tool_results_keep_the_historical_checkpoint_carrier() {
        let legacy = Content::Parts(vec![
            ContentPart::ToolResult {
                call_id: "call-1".into(),
                output: "first".into(),
                is_error: false,
                durable_content: None,
            },
            ContentPart::ToolResult {
                call_id: "call-2".into(),
                output: "second".into(),
                is_error: true,
                durable_content: None,
            },
        ]);
        assert!(
            message_body_parts(&Message::tool(match legacy.clone() {
                Content::Parts(parts) => parts,
                Content::Text(_) => unreachable!(),
            }))
            .is_none()
        );
        assert!(durable_tool_results_from_content(&legacy).is_none());
        assert!(
            content_to_durable(&legacy).is_err(),
            "nested tool results remain forbidden"
        );
    }

    #[test]
    fn structured_multi_tool_result_message_has_one_durable_envelope_per_call() {
        let content = Content::Parts(vec![
            ContentPart::ToolResult {
                call_id: "call-1".into(),
                output: "first".into(),
                is_error: false,
                durable_content: Some(DurableContent::text("first")),
            },
            ContentPart::ToolResult {
                call_id: "call-2".into(),
                output: "second".into(),
                is_error: true,
                durable_content: None,
            },
        ]);
        let results = durable_tool_results_from_content(&content).unwrap();
        assert_eq!(
            results
                .iter()
                .map(|result| result.call_id.as_str())
                .collect::<Vec<_>>(),
            ["call-1", "call-2"]
        );
        assert_eq!(
            durable_tool_results_from_content(
                &content_from_durable_tool_results(&results).unwrap()
            )
            .unwrap(),
            results,
        );
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
    state: &SchedulerStateV1,
) -> Result<(), KernelFault> {
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
        tcb.wait = restore_task_wait(task)?;
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
    // spc_002-09: `tcb.wait` above (line 624) is set directly, not through `TaskTable::set_wait` —
    // the index's only other sanctioned mutation path — so a restored waiting task is otherwise
    // invisible to `wake`/`notify` even though `Tcb.wait` itself is correct. Recompute the index
    // from every task's now-restored `.wait` field.
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

fn restore_task_wait(task: &TaskControlState) -> Result<Option<WaitReason>, KernelFault> {
    match task.wait.as_deref() {
        None => Ok(None),
        Some("approval") => Ok(Some(WaitReason::Approval)),
        Some("sub_agent_join") => Ok(Some(WaitReason::SubAgentJoin(
            task.waiting_on
                .iter()
                .map(|child| child.as_str().into())
                .collect(),
        ))),
        Some(other) => Err(incompatible(format!(
            "task {} waits on {other:?}, which this kernel does not know",
            task.task_id
        ))),
    }
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
    state: &ContextVmStateV1,
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
        let message = restore_message(&slot.role, &slot.body, &[])?;
        ctx.partitions.knowledge.push_entry(
            slot.key.as_deref().map(Into::into),
            message,
            slot.tokens,
            slot.pinned,
        );
        // The boundary-eviction mark is bookkeeping the push path does not take, so it is set
        // straight onto the entry it belongs to — a slot that was marked for removal must still be
        // marked after a restore, or the next sweep keeps something the run had already dropped.
        if slot.evict_at_boundary
            && let Some(entry) = ctx.partitions.knowledge.entries.last_mut()
        {
            entry.evict_at_boundary = true;
        }
    }
    ctx.partitions.signals = state.signals.clone();
    ctx.partitions.task_state = restore_task_state(&state.task_state);
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
) -> Result<Message, KernelFault> {
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
            if structured.schema_version != DurableContent::CURRENT_SCHEMA_VERSION {
                return Err(incompatible(format!(
                    "the checkpoint carries unsupported durable content schema version {}",
                    structured.schema_version
                )));
            }
            if !structured.durable_tool_results.is_empty() {
                if structured.durable_tool_result.is_some()
                    || structured.durable_content.is_some()
                    || structured.content_json.is_some()
                {
                    return Err(incompatible(
                        "the checkpoint durable tool results must not carry another body form"
                            .to_string(),
                    ));
                }
                content_from_durable_tool_results(&structured.durable_tool_results).map_err(|error| incompatible(format!(
                    "the checkpoint carries durable tool results this runtime cannot restore: {error}"
                )))?
            } else if let Some(result) = &structured.durable_tool_result {
                if structured.durable_content.is_some() || structured.content_json.is_some() {
                    return Err(incompatible(
                        "the checkpoint durable tool result must not carry another body form"
                            .to_string(),
                    ));
                }
                content_from_durable_tool_result(result).map_err(|error| incompatible(format!(
                    "the checkpoint carries durable tool result this runtime cannot restore: {error}"
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
            } else if let Some(content_json) = &structured.content_json {
                serde_json::from_str(content_json).map_err(|error| incompatible(format!(
                    "the checkpoint carries a legacy structured message body that does not decode: {error}"
                )))?
            } else {
                return Err(incompatible(
                    "the checkpoint structured message body has no content".to_string(),
                ));
            }
        }
    };
    Ok(Message {
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
        token_count: None,
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
    /// key. Threading it here rather than through the semantic engine keeps the legacy internal
    /// action untouched (Task 23 owns that) while the canonical projection is already total.
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
    /// Bindings use this read-only projection when a legacy host completion carries only a task
    /// identity. The value comes from checkpointed kernel state; hosts must never synthesize it.
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

    // ----- §12.1 · the logical-state projection -----

    /// Project the three driver-owned partitions of §12.1, plus the two transition fields the
    /// driver rather than the transaction owns.
    ///
    /// Explicitly a **projection**, not a serialisation: every value below is read through a named
    /// accessor and written into a versioned DTO field. That is the whole point of §12.1 — adding a
    /// field to [`LoopStateMachine`] must not change the checkpoint format, and a checkpoint field
    /// must not silently vanish because an internal one was renamed. It is also why the internal
    /// enums travel as their `label()` plus their carried data: `TaskLifecycle::Done(reason)` and
    /// `Residency::External { .. }` are semantic-kernel shapes, and mirroring them would make the
    /// checkpoint a checkpoint of a private layout.
    pub fn project_logical_state(&self) -> LogicalStateProjection {
        LogicalStateProjection {
            root_kind: self.root_kind,
            focus: self.focus.clone(),
            syscall: self.project_syscall_state(),
            scheduler: self.project_scheduler_state(),
            context_vm: self.project_context_vm_state(),
        }
    }

    fn project_syscall_state(&self) -> SyscallStateV1 {
        SyscallStateV1 {
            policy_revision: self.policy.as_ref().map(LivePolicyState::revision),
            live_config: self.policy.as_ref().map(|policy| policy.config().clone()),
            provider_calls: self
                .provider_calls
                .iter()
                .map(|(effect_id, call)| PendingProviderCallState {
                    effect_id: effect_id.clone(),
                    task_id: call.task_id.clone(),
                    exposed_tools: call.exposed_tools.iter().cloned().collect(),
                })
                .collect(),
            consumed_call_ids: self.consumed_calls.iter().cloned().collect(),
            authored_memory_writes: self
                .pending_memory_writes
                .iter()
                .map(|(effect_id, write)| AuthoredMemoryWriteState {
                    effect_id: effect_id.clone(),
                    binding_id: write.binding_id.clone(),
                    name: write.name.clone(),
                    kind: write.kind,
                    size_bytes: write.size_bytes,
                })
                .collect(),
            authored_memory_queries: self
                .pending_memory_queries
                .iter()
                .map(|(effect_id, query)| AuthoredMemoryQueryState {
                    effect_id: effect_id.clone(),
                    binding_id: query.binding_id.clone(),
                    text: query.text.clone(),
                    requested_k: query.requested_k,
                })
                .collect(),
            memory_write_window_ms: self
                .engine
                .as_ref()
                .map(|engine| {
                    engine
                        .memory_write_window()
                        .iter()
                        .copied()
                        .map(WireU64::new)
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    fn project_scheduler_state(&self) -> SchedulerStateV1 {
        let Some(engine) = self.engine.as_ref() else {
            return SchedulerStateV1::default();
        };
        let (total_tokens, subagents_spawned, rounds_completed) = engine.local_budget_usage();
        let signal_state = engine.signal_checkpoint_state();
        let entropy_state = engine.entropy_checkpoint_state();
        let workflow = engine.workflow_checkpoint_nodes().map(|runtime_nodes| {
            let workflow_id = self
                .workflow_id
                .as_ref()
                .expect("an active canonical workflow has a logical identity");
            assert_eq!(
                runtime_nodes.len(),
                self.workflow_nodes.len(),
                "the semantic workflow and its canonical source DAG stay index-aligned"
            );
            WorkflowGraphState {
                workflow_id: workflow_id.clone(),
                nodes: runtime_nodes
                    .into_iter()
                    .zip(self.workflow_nodes.iter())
                    .map(|(runtime, wire)| WorkflowNodeState {
                        node_id: wire.node_id.clone(),
                        task: wire.task.clone(),
                        depends_on: wire.depends_on.clone(),
                        run_spec: wire.run_spec.clone(),
                        kind: workflow_kind_label(&runtime).to_string(),
                        status: workflow_status_label(runtime.status).to_string(),
                        active_agent_id: runtime.active_agent_id,
                        iterations_completed: runtime.iterations_completed as u32,
                    })
                    .collect(),
            }
        });
        SchedulerStateV1 {
            turn: engine.turn,
            total_tokens: WireU64::new(total_tokens),
            rounds_completed,
            subagents_spawned,
            started_at_ms: engine.started_at_ms().map(WireU64::new),
            wall_budget_ms: engine.wall_budget().map(WireU64::new),
            tasks: engine
                .task_table()
                .all()
                .iter()
                .map(|tcb| TaskControlState {
                    task_id: TaskId::new(tcb.id.as_str())
                        .expect("an internal task id is always a legal branded ref"),
                    parent_task_id: tcb
                        .parent
                        .as_ref()
                        .and_then(|parent| TaskId::new(parent.as_str()).ok()),
                    lifecycle: tcb.state.label().to_string(),
                    termination: match tcb.state {
                        TaskLifecycle::Done(reason) => Some(reason.label().to_string()),
                        _ => None,
                    },
                    wait: tcb.wait.as_ref().map(|wait| wait.label().to_string()),
                    waiting_on: match &tcb.wait {
                        Some(WaitReason::SubAgentJoin(children)) => children
                            .iter()
                            .filter_map(|child| TaskId::new(child.as_str()).ok())
                            .collect(),
                        _ => Vec::new(),
                    },
                    wait_set: tcb.wait_set.as_ref().map(project_wait_set),
                    capability_ids: tcb.caps.iter().map(|cap| cap.to_string()).collect(),
                    capabilities: tcb.capabilities.clone(),
                    process: tcb.proc.as_ref().map(|process| ChildProcessState {
                        role: agent_role_label(process.role).to_string(),
                        isolation: agent_isolation_label(process.isolation).to_string(),
                        context_inheritance: context_inheritance_label(process.context_inheritance)
                            .to_string(),
                        join_result: process.result.as_ref().map(|result| {
                            super::scalar::BoundedJson::new(
                                serde_json::to_value(result)
                                    .expect("a child join result is serializable"),
                            )
                            .expect("a child join result is bounded")
                        }),
                    }),
                    supervision: tcb.supervision.clone(),
                    supervision_events: tcb.supervision_events.clone(),
                    tokens_used: WireU64::new(tcb.budget.total_tokens),
                    turns_used: tcb.budget.turns,
                    child_budget_remaining: tcb.child_budget_remaining,
                    budget_grant: tcb.budget_grant.clone(),
                    mailbox: tcb.mailbox.clone(),
                })
                .collect(),
            attempts: self
                .attempts
                .iter()
                .filter_map(|(task_id, attempt_id)| {
                    Some(TaskAttemptState {
                        task_id: TaskId::new(task_id.as_str()).ok()?,
                        attempt_id: attempt_id.clone(),
                    })
                })
                .collect(),
            workflow,
            queued_signals: signal_state
                .queued
                .into_iter()
                .map(|queued| queued_signal_state(&queued))
                .collect(),
            signal_dedupe_keys: signal_state
                .seen_order
                .into_iter()
                .map(|key| key.to_string())
                .collect(),
            milestone: self
                .loaded_contract_id
                .as_ref()
                .map(|contract_id| MilestoneState {
                    contract_id: contract_id.clone(),
                    phase_id: engine.current_milestone_phase_id().map(str::to_string),
                    complete: engine.is_milestone_complete(),
                    blocked_count: engine.milestone_blocked_count(),
                }),
            entropy: EntropyState {
                window: entropy_state
                    .window
                    .into_iter()
                    .map(|entry| EntropyTurnState {
                        errored_results: entry.errored_results,
                        total_results: entry.total_results,
                        rollbacks: entry.rollbacks,
                    })
                    .collect(),
                rollbacks_pending: entropy_state.rollbacks_pending,
                disarmed: entropy_state.disarmed,
                last_alert_turn: entropy_state.last_alert_turn,
            },
            channels: engine
                .task_table()
                .channels()
                .iter()
                .map(|(channel_id, channel)| LocalChannelState {
                    channel_id: channel_id.0.to_string(),
                    channel: channel.clone(),
                })
                .collect(),
            objects: engine.task_table().objects().values().cloned().collect(),
        }
    }

    fn project_context_vm_state(&self) -> ContextVmStateV1 {
        let Some(engine) = self.engine.as_ref() else {
            return ContextVmStateV1::default();
        };
        let ctx = &engine.ctx;
        ContextVmStateV1 {
            handles: ctx
                .handles
                .all()
                .iter()
                .map(|handle| {
                    let (payload_ref, digest, original_size) = match &handle.residency {
                        Residency::External {
                            payload_ref,
                            digest,
                            original_size,
                        } => (
                            Some(payload_ref.clone()),
                            Some(digest.clone()),
                            Some(WireU64::new(*original_size)),
                        ),
                        Residency::PagedOut {
                            payload_ref,
                            digest,
                        } => (Some(payload_ref.clone()), Some(digest.clone()), None),
                        Residency::Resident | Residency::Collapsed => (None, None, None),
                    };
                    HandleState {
                        handle_id: handle.id,
                        kind: handle_kind_label(&handle.kind).to_string(),
                        residency: handle.residency.label().to_string(),
                        payload_ref,
                        digest,
                        original_size,
                        tokens: handle.tokens,
                        source: handle.source.as_ref().map(|source| source.to_string()),
                    }
                })
                .collect(),
            next_handle_id: ctx.next_handle_id(),
            pending_payload_loads: self
                .pending_payload_loads
                .iter()
                .map(|(effect_id, load)| PendingPayloadLoadState {
                    effect_id: effect_id.clone(),
                    handle_id: load.handle_id.clone(),
                    digest: load.digest.clone(),
                    original_size: load.original_size.map(WireU64::new),
                })
                .collect(),
            active_skills: ctx
                .active_skills
                .iter()
                .map(|(skill, lease)| SkillLeaseState {
                    skill: skill.to_string(),
                    lease_until_turn: *lease,
                })
                .collect(),
            knowledge: ctx
                .partitions
                .knowledge
                .entries
                .iter()
                .map(|entry| KnowledgeSlotState {
                    key: entry.key.as_ref().map(|key| key.to_string()),
                    role: role_label(entry.message.role).to_string(),
                    body: self.project_body(&entry.message),
                    tokens: entry.tokens,
                    pinned: entry.pinned,
                    evict_at_boundary: entry.evict_at_boundary,
                })
                .collect(),
            signals: ctx.partitions.signals.clone(),
            messages: ctx
                .partitions
                .system
                .messages
                .iter()
                .map(|message| self.project_message(MessagePartition::System, message))
                .chain(
                    ctx.partitions
                        .history
                        .messages
                        .iter()
                        .map(|message| self.project_message(MessagePartition::History, message)),
                )
                .collect(),
            task_state: project_task_state(&ctx.partitions.task_state),
            partition_tokens: PartitionTokenState {
                system: ctx.partitions.system.token_count,
                knowledge: ctx.partitions.knowledge.token_count,
                history: ctx.partitions.history.token_count,
            },
            history_len: ctx.partitions.history.messages.len() as u32,
            frozen_history_len: ctx.frozen_history_len() as u32,
            last_activity_ms: WireU64::new(ctx.last_activity_ms),
            last_compact_ms: ctx.last_compact_ms.map(WireU64::new),
        }
    }

    /// §5q-2 · one stored message, projected.
    fn project_message(
        &self,
        partition: MessagePartition,
        message: &Message,
    ) -> StoredMessageState {
        StoredMessageState {
            partition,
            role: role_label(message.role).to_string(),
            body: self.project_body(message),
            tool_calls: message
                .tool_calls
                .iter()
                .map(|call| LogicalToolCall {
                    call_id: call.id.to_string(),
                    name: call.name.to_string(),
                    arguments: call.arguments.to_string(),
                })
                .collect(),
            tokens: message.token_count.unwrap_or(0),
        }
    }

    /// §7.10 · inline or by reference, decided by the message's own residency.
    ///
    /// The rule is not "is this body big" but "where does this body live": a result whose handle
    /// says `External` was never resident in the first place (only its preview is in the message),
    /// and one that says `PagedOut` left under pressure and lives with the host now. Either way the
    /// checkpoint carries the reference and the digest that verifies a page-in — putting the bytes
    /// back would re-create exactly the round trip §7.10 exists to delete.
    fn project_body(&self, message: &Message) -> StoredMessageBody {
        // External/paged-out tool results must retain their reference form. Their legacy text
        // projection is still represented as a `DurableToolResult`, but choosing that form first
        // would discard the handle digest and make the body unreachable after restore.
        let single_tool_result_is_external =
            match &message.content {
                Content::Parts(parts) if parts.len() == 1 => match &parts[0] {
                    ContentPart::ToolResult { call_id, .. } => {
                        self.engine
                            .as_ref()
                            .and_then(|engine| {
                                engine.ctx.handles.all().iter().find(|handle| {
                                    handle.source.as_deref() == Some(call_id.as_str())
                                })
                            })
                            .is_some_and(|handle| handle.residency.digest().is_some())
                    }
                    _ => false,
                },
                _ => false,
            };
        if !single_tool_result_is_external {
            if let Some(results) = durable_tool_results_from_content(&message.content) {
                return StoredMessageBody::Structured(StructuredMessageBody {
                    schema_version: DurableContent::CURRENT_SCHEMA_VERSION,
                    durable_content: None,
                    durable_tool_result: None,
                    durable_tool_results: results,
                    content_json: None,
                });
            }
            if let Some(result) = durable_tool_result_from_content(&message.content) {
                return StoredMessageBody::Structured(StructuredMessageBody {
                    schema_version: result.schema_version,
                    durable_content: None,
                    durable_tool_result: Some(result),
                    durable_tool_results: Vec::new(),
                    content_json: None,
                });
            }
        }
        let Some((text, tool_call_id, is_error)) = message_body_parts(message) else {
            let durable_content = content_to_durable(&message.content).ok();
            let content_json = if durable_content.is_none() {
                // Keep the legacy carrier only when the new durable form cannot represent the
                // content. Writing both forms would make the checkpoint ambiguous on restore.
                serde_json::to_string(&message.content).ok()
            } else {
                None
            };
            return StoredMessageBody::Structured(StructuredMessageBody {
                schema_version: DurableContent::CURRENT_SCHEMA_VERSION,
                durable_content,
                durable_tool_result: None,
                durable_tool_results: Vec::new(),
                // An unsupported future core content variant is retained in the legacy carrier
                // rather than triggering a checkpoint-time panic.
                content_json,
            });
        };
        let Some(call_id) = tool_call_id.as_deref() else {
            return StoredMessageBody::Inline(InlineMessageBody {
                text,
                tool_call_id,
                is_error,
            });
        };
        let referenced = self.engine.as_ref().and_then(|engine| {
            let handle = engine
                .ctx
                .handles
                .all()
                .iter()
                .find(|handle| handle.source.as_deref() == Some(call_id))?;
            let digest = handle.residency.digest()?;
            Some((
                handle.id,
                digest.to_string(),
                matches!(handle.residency, Residency::PagedOut { .. }),
            ))
        });
        match referenced {
            Some((handle_id, digest, is_paged_out)) => {
                StoredMessageBody::Reference(ReferencedMessageBody {
                    handle_id,
                    digest,
                    preview: if is_paged_out {
                        crate::context::renderer::collapse_preview(&text, call_id)
                    } else {
                        truncate_on_char_boundary(&text, self.preview_bytes())
                    },
                    tool_call_id,
                    is_error,
                })
            }
            None => StoredMessageBody::Inline(InlineMessageBody {
                text,
                tool_call_id,
                is_error,
            }),
        }
    }

    fn preview_bytes(&self) -> usize {
        self.policy
            .as_ref()
            .map(|policy| policy.config().payload_policy.preview_bytes as usize)
            .unwrap_or(2 * 1024)
    }

    // ----- §12.2 · the logical-state restore -----

    /// Rebuild a driver from a checkpoint's logical state (§12.2 line 3).
    ///
    /// The exact inverse of [`Self::project_logical_state`], and deliberately nothing more: every
    /// value written here is a value the projection reads back, so "did the restore work" is not a
    /// judgement call — [`super::restore::restore_operation`] re-projects immediately afterwards and
    /// compares the digest. A field this function forgets therefore fails the restore rather than
    /// producing a runtime that is quietly one field short of the one that crashed.
    ///
    /// Task 16b makes every scheduler branch invertible here: workflow source nodes rebuild their
    /// private graph indexes, queued signals rebuild priority and dedupe state, and child process
    /// identity is restored without re-running permission defaults. Unknown labels and inconsistent
    /// relationships still fail closed as `CheckpointIncompatible`.
    pub fn restore_logical_state(
        genesis_config: &ResolvedOperationConfig,
        state: &LogicalKernelState,
    ) -> Result<Self, KernelFault> {
        let mut driver = Self::new();
        let live_config = state
            .syscall
            .live_config
            .clone()
            .unwrap_or_else(|| genesis_config.clone());

        // The engine is built from the configuration and then *moved* onto the checkpointed facts;
        // it is never deserialised. Boot-only axes come from the genesis configuration the record
        // froze, live-mutable ones from the patched configuration the checkpoint carries.
        let mut engine = build_engine(genesis_config);
        install_live_policies(&mut engine, &live_config);
        engine.set_root_workflow(state.transition.root_kind == Some(RootKind::Workflow));
        driver.policy = Some(LivePolicyState::restore(
            state.syscall.policy_revision.unwrap_or(WireU64::ZERO),
            live_config.clone(),
        ));

        restore_scheduler(&mut engine, &live_config, &state.scheduler)?;
        if let Some(preempt) =
            state
                .transition
                .pending_effects
                .iter()
                .find_map(|effect| match &effect.effect {
                    EffectKind::PreemptTasks(preempt) => Some(preempt),
                    _ => None,
                })
        {
            engine.restore_pending_preempt(
                preempt
                    .attempts
                    .iter()
                    .map(|attempt| attempt.task_id.as_str().to_string())
                    .collect(),
                preempt.reason.clone(),
            );
        }
        restore_context_vm(&mut engine, &state.context_vm)?;
        engine.restore_memory_write_window(
            state
                .syscall
                .memory_write_window_ms
                .iter()
                .map(|at| at.get())
                .collect(),
        );

        if let Some(milestone) = &state.scheduler.milestone {
            let contract = live_config
                .verification_contract(&milestone.contract_id)
                .ok_or_else(|| {
                    KernelFault::new(
                        KernelFaultCode::CheckpointIncompatible,
                        format!(
                            "the checkpoint runs verification contract {:?}, which this \
                             operation's configuration no longer declares",
                            milestone.contract_id
                        ),
                    )
                })?;
            engine.load_milestone_contract(core_milestone_contract(contract, &live_config));
            if !engine
                .restore_milestone_cursor(milestone.phase_id.as_deref(), milestone.blocked_count)
            {
                return Err(KernelFault::new(
                    KernelFaultCode::CheckpointIncompatible,
                    format!(
                        "the checkpoint sits on milestone phase {:?} of contract {:?}, which that \
                         contract does not declare",
                        milestone.phase_id, milestone.contract_id
                    ),
                ));
            }
            driver.loaded_contract_id = Some(milestone.contract_id.clone());
        }

        driver.engine = Some(engine);
        driver.root_kind = state.transition.root_kind;
        driver.focus = state.transition.focus.clone();
        if let Some(workflow) = &state.scheduler.workflow {
            driver.workflow_id = Some(workflow.workflow_id.clone());
            driver.node_ids = workflow
                .nodes
                .iter()
                .map(|node| node.node_id.clone())
                .collect();
            driver.workflow_nodes = workflow
                .nodes
                .iter()
                .map(|node| WireNode {
                    node_id: node.node_id.clone(),
                    task: node.task.clone(),
                    depends_on: node.depends_on.clone(),
                    run_spec: node.run_spec.clone(),
                })
                .collect();
        }
        driver.attempts = state
            .scheduler
            .attempts
            .iter()
            .map(|attempt| {
                (
                    attempt.task_id.as_str().to_string(),
                    attempt.attempt_id.clone(),
                )
            })
            .collect();
        driver.provider_calls = state
            .syscall
            .provider_calls
            .iter()
            .map(|call| {
                (
                    call.effect_id.clone(),
                    PendingProviderCall {
                        task_id: call.task_id.clone(),
                        exposed_tools: call.exposed_tools.iter().cloned().collect(),
                    },
                )
            })
            .collect();
        driver.consumed_calls = state.syscall.consumed_call_ids.iter().cloned().collect();
        driver.pending_memory_writes = state
            .syscall
            .authored_memory_writes
            .iter()
            .map(|write| {
                (
                    write.effect_id.clone(),
                    AuthoredMemoryWrite {
                        binding_id: write.binding_id.clone(),
                        name: write.name.clone(),
                        kind: write.kind,
                        size_bytes: write.size_bytes,
                    },
                )
            })
            .collect();
        driver.pending_memory_queries = state
            .syscall
            .authored_memory_queries
            .iter()
            .map(|query| {
                (
                    query.effect_id.clone(),
                    AuthoredMemoryQuery {
                        binding_id: query.binding_id.clone(),
                        text: query.text.clone(),
                        requested_k: query.requested_k,
                    },
                )
            })
            .collect();
        driver.pending_payload_loads = state
            .context_vm
            .pending_payload_loads
            .iter()
            .map(|load| {
                (
                    load.effect_id.clone(),
                    PendingPayloadLoad {
                        handle_id: load.handle_id.clone(),
                        digest: load.digest.clone(),
                        original_size: load.original_size.map(WireU64::get),
                    },
                )
            })
            .collect();
        Ok(driver)
    }

    // ----- the plan function -----

    /// Plan one input.
    ///
    /// Pass this to [`KernelTransaction::prepare`](super::transaction::KernelTransaction::prepare).
    /// The focus/root-kind fold does **not** advance here — call [`Self::note_committed`] once the
    /// host's append and the transaction's commit have both succeeded.
    pub fn plan(&mut self, context: &PlanContext<'_>) -> Result<PlannedStep, KernelFault> {
        if let Some(fault) = &self.poison {
            return Err(fault.clone());
        }
        if let Some(staged) = &self.staged {
            let staged_seq = staged.step_seq;
            return Err(self.poison_with(KernelFault::new(
                KernelFaultCode::TransactionConflict,
                format!(
                    "the driver still holds the plan of step {staged_seq}; its transition never \
                     committed while the semantic kernel already advanced under it, so this \
                     runtime no longer describes the journal — rebuild from the records"
                ),
            )));
        }
        let mut step = self.plan_inner(context)?;
        step.observations = self
            .engine
            .as_mut()
            .map(LoopStateMachine::take_observations)
            .unwrap_or_default();
        self.staged = Some(StagedFocus {
            step_seq: context.step_seq,
            root_kind: step.root_kind,
            focus: step.focus.clone(),
        });
        Ok(step)
    }

    /// Install the staged fold after the transaction committed the record (§7.4: a focus moves only
    /// on a committed transition).
    pub fn note_committed(&mut self, step_seq: WireU64) -> Result<(), KernelFault> {
        if let Some(fault) = &self.poison {
            return Err(fault.clone());
        }
        let Some(staged) = self.staged.take() else {
            return Err(self.poison_with(KernelFault::new(
                KernelFaultCode::TransactionConflict,
                format!("step {step_seq} committed, but the driver planned no such step"),
            )));
        };
        if staged.step_seq != step_seq {
            let planned = staged.step_seq;
            return Err(self.poison_with(KernelFault::new(
                KernelFaultCode::TransactionConflict,
                format!("step {step_seq} committed, but the driver planned step {planned}"),
            )));
        }
        if let Some(kind) = staged.root_kind {
            self.root_kind = Some(kind);
        }
        self.focus = staged.focus;
        Ok(())
    }

    /// Plan **and** fold in one call — the shape
    /// [`rebuild_from_records`](super::transaction::KernelTransaction::rebuild_from_records) needs,
    /// where every record it replays is by definition already durable.
    pub fn fold(&mut self, context: &PlanContext<'_>) -> Result<PlannedStep, KernelFault> {
        let step = self.plan(context)?;
        self.note_committed(context.step_seq)?;
        Ok(step)
    }

    // ----- §10.2 · the agent-authored workflow seam -----

    /// Enter a workflow the agent asked for, inside an agent root (§10.2).
    ///
    /// This is the P1 reduction point Task 10 wires its `SyscallRequest::SubmitWorkflow` gate to;
    /// the authority rules it enforces are already the final ones:
    ///
    /// * the root kind stays `Agent` — a syscall never re-roots an operation;
    /// * the focus moves to `WorkflowController { parent_task_id: Some(agent task) }`;
    /// * depth is at most 1. Asking for a workflow while the focus already *is* a
    ///   `WorkflowController` is an `InvalidAuthority` fault with zero mutation — workflows do not
    ///   stack (§15.4).
    pub fn begin_nested_workflow(
        &mut self,
        context: &PlanContext<'_>,
        spec: &WireSpec,
    ) -> Result<PlannedStep, KernelFault> {
        if let Some(fault) = &self.poison {
            return Err(fault.clone());
        }
        let mut index = 0;
        let outcome = self
            .enter_nested_workflow(context, spec, &mut index)
            .map_err(|refusal| match refusal {
                SyscallRefusal::Fault(fault) => fault,
                // A direct caller has no observation channel, so the gate's denial becomes the
                // transition's refusal. Through the P1 path the same denial is an audit fact.
                SyscallRefusal::Rejected(rejected) => {
                    KernelFault::new(KernelFaultCode::ResourceLimitExceeded, rejected.reason)
                }
            })?;
        let step = PlannedStep {
            root_kind: Some(RootKind::Agent),
            focus: outcome.focus,
            observations: self
                .engine
                .as_mut()
                .map(LoopStateMachine::take_observations)
                .unwrap_or_default(),
            disposition: StepDisposition::Effects(EffectsDisposition {
                effects: outcome.effects,
            }),
        };
        self.staged = Some(StagedFocus {
            step_seq: context.step_seq,
            root_kind: step.root_kind,
            focus: step.focus.clone(),
        });
        Ok(step)
    }

    /// The body of [`Self::begin_nested_workflow`], without the staging — so a syscall batch that
    /// also carries other requests composes it instead of racing it for the staging slot.
    fn enter_nested_workflow(
        &mut self,
        context: &PlanContext<'_>,
        spec: &WireSpec,
        effect_index: &mut u32,
    ) -> Result<SyscallOutcome, SyscallRefusal> {
        let staged = self.staged.as_ref().map(|staged| staged.focus.clone());
        let focus = staged.as_ref().unwrap_or(&self.focus);
        let root_kind = self.root_kind;

        let parent_task_id = match (root_kind, focus) {
            (Some(RootKind::Agent), Some(ExecutionFocus::AgentTurn(turn))) => turn.task_id.clone(),
            (Some(RootKind::Agent), Some(ExecutionFocus::WorkflowController(_))) => {
                return Err(authority(
                    "a workflow is already the execution focus; workflows do not stack, so a \
                     second start request is refused with no spawn effect (§7.4 focus depth ≤ 1)",
                ));
            }
            (Some(RootKind::Workflow), _) => {
                return Err(authority(
                    "this operation's root is a workflow; its focus never moves, and a nested \
                     workflow start is not a transition it admits (§7.4)",
                ));
            }
            _ => {
                return Err(SyscallRefusal::Fault(KernelFault::new(
                    KernelFaultCode::InvalidLifecycle,
                    "no root has started, so there is no agent turn to suspend".to_string(),
                )));
            }
        };

        for node in &spec.nodes {
            self.require_known_contract(context.config, node.run_spec.as_ref())
                .map_err(|fault| {
                    SyscallRefusal::Rejected(SyscallRejection::new("start_workflow", fault.message))
                })?;
        }
        let core_spec = build_core_spec(spec).map_err(SyscallRefusal::Fault)?;
        let node_ids = wire_node_ids(spec);
        let workflow_id = mint_workflow_id(&context.input.operation_id, context.step_seq);
        self.require_effect_support(context.config, EffectKindTag::SpawnTasks)
            .map_err(SyscallRefusal::Fault)?;

        // §10.2 · the resource gate runs before the DAG is installed, so a denial commits with no
        // spawn effect at all rather than with a workflow the run cannot afford.
        let engine = self.engine_mut().map_err(SyscallRefusal::Fault)?;
        let disposition = engine.gate_syscall(&CoreSyscall::LoadWorkflow {
            node_count: spec.nodes.len(),
        });
        if !disposition.is_allowed() {
            return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                "start_workflow",
                denial_reason(&disposition, "workflow authoring denied"),
            )));
        }

        // ----- past this line the semantic engine advances -----
        engine.set_root_workflow(false);
        let action = engine.load_workflow_as(core_spec, parent_task_id.as_str());
        self.node_ids = node_ids;
        self.workflow_nodes = spec.nodes.clone();
        self.workflow_id = Some(workflow_id.clone());
        let disposition = self
            .disposition_for_at(context, action, RootKind::Agent, effect_index)
            .map_err(SyscallRefusal::Fault)?;
        let StepDisposition::Effects(effects) = disposition else {
            return Err(SyscallRefusal::Fault(KernelFault::new(
                KernelFaultCode::InvalidLifecycle,
                "entering a nested workflow cannot terminate the operation".to_string(),
            )));
        };
        Ok(SyscallOutcome {
            effects: effects.effects,
            focus: Some(ExecutionFocus::workflow_controller(
                workflow_id,
                Some(parent_task_id),
            )),
            needs_workflow_round: false,
            ack: None,
        })
    }

    // ----- §7.6 · P1 syscalls and caller causation -----

    /// Derive the caller of every syscall a provider result carries.
    ///
    /// This is the whole of "the host does not declare a caller". The kernel already knows which
    /// provider effect it is resolving and which surface that call advertised; a `ProviderTool`
    /// causation is that knowledge written down. Three refusals, all before anything moves:
    ///
    /// * the effect is not one this driver published as a provider call — there is no turn to
    ///   attribute the request to;
    /// * the tool name was never exposed on that turn — a result may not invent a surface, and a
    ///   forged `start_workflow` in a run that never offered one dies here;
    /// * the call id already produced a syscall — a causation is spent once, so re-delivering the
    ///   same result under a fresh input id buys nothing.
    fn derive_provider_syscalls(
        &self,
        effect_id: &EffectId,
        calls: &[WireToolCall],
    ) -> Result<Vec<(SyscallCausation, WireToolCall)>, KernelFault> {
        let Some(pending) = self.provider_calls.get(effect_id) else {
            return Err(KernelFault::new(
                KernelFaultCode::InvalidAuthority,
                format!(
                    "effect {effect_id} is not a provider call this kernel published, so a tool \
                     call inside its result has no caller to derive (§7.6)"
                ),
            ));
        };
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut derived = Vec::with_capacity(calls.len());
        for call in calls {
            if !pending.exposed_tools.contains(&call.name) {
                return Err(KernelFault::new(
                    KernelFaultCode::InvalidAuthority,
                    format!(
                        "the turn behind effect {effect_id} exposed no tool named {:?}; a caller is \
                         derived from the surface the kernel published, never from the name a \
                         result carries (§7.6)",
                        call.name
                    ),
                ));
            }
            if self.consumed_calls.contains(call.call_id.as_str())
                || !seen.insert(call.call_id.as_str())
            {
                return Err(KernelFault::new(
                    KernelFaultCode::InvalidAuthority,
                    format!(
                        "call {} already produced a syscall; a causation is consumed once (§7.6)",
                        call.call_id
                    ),
                ));
            }
            derived.push((
                SyscallCausation::ProviderTool(ProviderToolCausation {
                    provider_effect_id: effect_id.clone(),
                    call_id: call.call_id.clone(),
                    task_id: pending.task_id.clone(),
                }),
                call.clone(),
            ));
        }
        Ok(derived)
    }

    /// §7.9 + §7.6 · one completed provider turn, whole.
    ///
    /// The batch a model returns can mix two populations the kernel must not confuse: **P1
    /// syscalls**, which the kernel adjudicates itself, and **host tool calls**, which it
    /// dispatches. Both halves happen here, in this order and for this reason:
    ///
    /// 1. the syscalls are adjudicated *before* the turn is fed, because they change what the next
    ///    rendered context contains — an activated skill, an edited plan, a grown DAG;
    /// 2. the assistant message is fed **whole**, tool calls included, so the model reads back the
    ///    turn it actually emitted; each syscall is closed with the kernel's own answer so every
    ///    call still has a result (the trained convention);
    /// 3. the continuation is the engine's, not this function's — [`LoopStateMachine::feed`] stays
    ///    the single place that decides what a provider turn means. All this reduction contributes
    ///    is *what the kernel already has outstanding*, which is the §5k question: a pure
    ///    control-plane batch publishes no effect, so without an answer the operation would stall.
    fn plan_provider_completed(
        &mut self,
        context: &PlanContext<'_>,
        effect_id: &EffectId,
        completed: &ProviderCompleted,
    ) -> Result<PlannedStep, KernelFault> {
        let root_kind = self.require_root_kind()?;
        self.require_pending_provider_call(effect_id)?;
        let syscalls: Vec<WireToolCall> = completed
            .message
            .tool_calls
            .iter()
            .filter(|call| is_syscall_tool(&call.name))
            .cloned()
            .collect();
        let derived = self.derive_provider_syscalls(effect_id, &syscalls)?;
        let message = core_provider_message(&completed.message)?;

        // ----- past this line the semantic engine advances -----
        let engine = self.engine_mut()?;
        if let Some(tokens) = completed.observed_input_tokens {
            engine.ctx.set_observed_prompt_tokens(tokens);
        }
        // §22.8 · the typed stop reason answers the one question the loop asks, so no vendor text
        // is classified anywhere on this path.
        engine.set_output_truncated(matches!(
            completed.stop_reason,
            Some(super::effect::ProviderStopReason::MaxTokens)
        ));

        let mut index = 0u32;
        let mut effects = Vec::new();
        let mut syscall_focus: Option<ExecutionFocus> = None;
        let mut answered: Vec<AnsweredCall> = Vec::with_capacity(derived.len());
        let mut round_caller: Option<TaskId> = None;
        for (causation, call) in &derived {
            let outcome = match decode_syscall(call) {
                Ok(request) => self.apply_syscall(context, causation, &request, &mut index),
                Err(rejection) => Err(SyscallRefusal::Rejected(rejection)),
            };
            match outcome {
                Ok(outcome) => {
                    answered.push(AnsweredCall {
                        call_id: call.call_id.as_str().into(),
                        output: outcome
                            .ack
                            .clone()
                            .unwrap_or_else(|| syscall_ack(&call.name).to_string()),
                        is_error: false,
                    });
                    effects.extend(outcome.effects);
                    if let Some(next) = outcome.focus {
                        syscall_focus = Some(next);
                    }
                    if outcome.needs_workflow_round {
                        round_caller = Some(causation_task(causation));
                    }
                }
                Err(SyscallRefusal::Fault(fault)) => return Err(fault),
                Err(SyscallRefusal::Rejected(rejection)) => {
                    answered.push(AnsweredCall {
                        call_id: call.call_id.as_str().into(),
                        output: rejection.reason.clone(),
                        is_error: true,
                    });
                    self.note_rejection(rejection.by(&causation_task(causation)))
                }
            }
        }
        // §10.3 · a batch that grew the DAG owes it a spawn round, and the author waits for the
        // work it just authored rather than taking another turn.
        let mut awaits_kernel_work = !effects.is_empty();
        if let Some(caller) = round_caller {
            awaits_kernel_work = true;
            let action = self.engine_mut()?.drive_workflow_round(caller.as_str());
            self.extend_with_action(context, action, root_kind, &mut index, &mut effects)?;
        }

        let engine = self.engine_mut()?;
        engine.stage_adjudicated_turn(AdjudicatedTurn {
            answered_calls: answered,
            idle_continuation: if awaits_kernel_work {
                IdleContinuation::Await
            } else {
                IdleContinuation::CallProvider
            },
        });
        // The syscalls' own observations happened before `feed`, which clears the buffer at the
        // head of every event — carry them across, exactly as the child-completion path does.
        let syscall_observations = engine.take_observations();
        let action = engine.feed(LoopEvent::LLMResponse { message });
        let mut step = self.continue_after_at(context, action, root_kind, &mut index)?;
        if let Some(engine) = self.engine.as_mut() {
            engine.observations.splice(0..0, syscall_observations);
        }
        if syscall_focus.is_some() {
            step.focus = syscall_focus;
        }
        if !effects.is_empty() {
            match &mut step.disposition {
                StepDisposition::Effects(published) => {
                    let mut merged = effects;
                    merged.append(&mut published.effects);
                    published.effects = merged;
                }
                StepDisposition::Terminal(_) => {
                    return Err(KernelFault::new(
                        KernelFaultCode::InvalidLifecycle,
                        "a provider turn that terminates the operation cannot also publish the \
                         effects its syscalls asked for (§7.12)"
                            .to_string(),
                    ));
                }
            }
        }

        // The causation ledger is settled last, so a transition that ends in a fault does not leave
        // this driver claiming a spent call id and a resolved provider surface that the journal has
        // no record of. (The engine advance above is guarded the same way every other plan is — by
        // the staging slot, which fails closed on a plan that never commits.)
        self.provider_calls.remove(effect_id);
        for (_, call) in &derived {
            self.consumed_calls
                .insert(call.call_id.as_str().to_string());
        }
        Ok(step)
    }

    /// Apply one already-attributed request. Every arm reads its caller from `causation` and
    /// nothing else — there is no parameter through which a host could name a different one.
    fn apply_syscall(
        &mut self,
        context: &PlanContext<'_>,
        causation: &SyscallCausation,
        request: &SyscallRequest,
        effect_index: &mut u32,
    ) -> Result<SyscallOutcome, SyscallRefusal> {
        let caller = causation_task(causation);
        let quarantined = self
            .engine
            .as_ref()
            .is_some_and(|engine| engine.task_quarantined(caller.as_str()));
        if quarantined && let Some(family) = privileged_family(request) {
            // §7.6 · a quarantined task read untrusted content. Letting it grow the DAG, mutate its
            // capability surface or reach memory would make the untrusted content the author of the
            // escalation. Fails closed on the whole family rather than trusting per-request
            // coercion to be exhaustive.
            return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                family,
                format!(
                    "quarantine: task {caller} is quarantined and may not widen its authority \
                     through a {family} syscall"
                ),
            )));
        }

        match request {
            SyscallRequest::SubmitWorkflow(submit) => {
                if submit.spec.nodes.is_empty() {
                    return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                        "start_workflow",
                        "an authored workflow with no nodes has nothing to spawn",
                    )));
                }
                if self
                    .engine
                    .as_ref()
                    .is_some_and(LoopStateMachine::workflow_active)
                {
                    // §10.2 flatten: the caller is already inside a DAG, so its spec grows that DAG
                    // rather than stacking a second one. One root lifecycle, one quota.
                    self.append_nodes(
                        context.config,
                        &submit.spec.nodes,
                        &caller,
                        CoreSyscall::LoadWorkflow { node_count: 0 },
                        "start_workflow",
                    )
                } else {
                    self.enter_nested_workflow(context, &submit.spec, effect_index)
                }
            }
            SyscallRequest::AppendWorkflowNodes(append) => {
                if append.nodes.is_empty() {
                    return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                        "submit_workflow_nodes",
                        "an empty submission appends nothing",
                    )));
                }
                if !self
                    .engine
                    .as_ref()
                    .is_some_and(LoopStateMachine::workflow_active)
                {
                    return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                        "submit_workflow_nodes",
                        "no workflow is in flight, so there is no graph to append to",
                    )));
                }
                self.append_nodes(
                    context.config,
                    &append.nodes,
                    &caller,
                    CoreSyscall::SubmitNodes { count: 0 },
                    "submit_workflow_nodes",
                )
            }
            SyscallRequest::ActivateSkill(activate) => {
                let engine = self.engine_mut().map_err(SyscallRefusal::Fault)?;
                if !engine.ctx.skill_available(&activate.name) {
                    return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                        "skill",
                        format!(
                            "this operation declares no skill named {:?}; activation is a \
                             capability mutation and is refused rather than invented",
                            activate.name
                        ),
                    )));
                }
                ensure_skill_grants_are_attenuated(
                    engine.ctx.skill_capability_grants(&activate.name),
                    engine.task_capabilities(caller.as_str()),
                )
                .map_err(|violations| {
                    SyscallRefusal::Rejected(SyscallRejection::new(
                        "skill",
                        skill_grant_attenuation_message(&activate.name, &violations),
                    ))
                })?;
                let expires_at_turn = activate
                    .lease_turns
                    .map(|turns| engine.turn.saturating_add(turns));
                engine
                    .ctx
                    .activate_skill_leased(activate.name.as_str(), expires_at_turn);
                Ok(SyscallOutcome::default())
            }
            SyscallRequest::UpdateTask(update) => {
                let engine = self.engine_mut().map_err(SyscallRefusal::Fault)?;
                engine.ctx.update_task(core_task_update(&update.update));
                Ok(SyscallOutcome::default())
            }
            SyscallRequest::RequestMemoryWrite(write) => {
                self.plan_memory_write(context, causation, &write.proposal, effect_index)
            }
            SyscallRequest::RequestMemoryQuery(query) => {
                self.plan_memory_query(context, causation, &query.query, effect_index)
            }
            SyscallRequest::PageIn(page_in) => {
                self.plan_page_in(context, &caller, &page_in.handle_id, effect_index)
            }
            SyscallRequest::SendMessage(send) => self.plan_send_message(&caller, send),
            SyscallRequest::PublishChannel(publish) => self.plan_publish_channel(&caller, publish),
            SyscallRequest::ReceiveMailbox(receive) => {
                self.plan_receive_mailbox(&caller, receive.limit)
            }
            SyscallRequest::ReceiveChannel(receive) => {
                self.plan_receive_channel(&caller, &receive.channel_id)
            }
            SyscallRequest::ReadObject(read) => self.plan_read_object(&caller, read.object_id),
        }
    }

    fn plan_send_message(
        &mut self,
        caller: &TaskId,
        request: &super::syscall::SendMessageRequest,
    ) -> Result<SyscallOutcome, SyscallRefusal> {
        validate_ipc_labels(&request.message_id, &request.message_kind)?;
        let engine = self.engine_mut().map_err(SyscallRefusal::Fault)?;
        let handle = resolve_ipc_handle(engine, &request.payload_handle)?;
        let descriptor =
            crate::mm::handle::ObjectDescriptor::from_handle(caller.as_str().into(), &handle, 1);
        if engine
            .task_table()
            .object(descriptor.id)
            .is_some_and(|existing| existing != &descriptor)
        {
            return Err(local_ipc_refusal(
                crate::scheduler::tcb::LocalIpcError::ObjectConflict,
            ));
        }
        let now = crate::scheduler::mailbox::LogicalTime(engine.turn);
        let message = crate::scheduler::mailbox::MailboxMessage {
            id: request.message_id.as_str().into(),
            from: caller.as_str().into(),
            to: request.to.as_str().into(),
            kind: request.message_kind.as_str().into(),
            payload_handle: handle.id,
            priority: crate::types::signal::Urgency::Normal,
            timestamp: now,
            expires_at: request
                .ttl_turns
                .map(|ttl| crate::scheduler::mailbox::LogicalTime(engine.turn.saturating_add(ttl))),
        };
        let accepted = engine
            .task_table_mut()
            .send_message_from(caller.as_str(), message, now)
            .map_err(local_ipc_refusal)?;
        engine
            .task_table_mut()
            .register_object(caller.as_str(), descriptor)
            .map_err(local_ipc_refusal)?;
        Ok(local_ipc_outcome(accepted))
    }

    fn plan_publish_channel(
        &mut self,
        caller: &TaskId,
        request: &super::syscall::PublishChannelRequest,
    ) -> Result<SyscallOutcome, SyscallRefusal> {
        validate_ipc_labels(&request.message_id, &request.message_kind)?;
        if request.channel_id.is_empty() || request.subscribers.is_empty() {
            return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                "publish_channel",
                "channel_id and subscribers must be non-empty",
            )));
        }
        let mut subscribers: Vec<_> = request
            .subscribers
            .iter()
            .map(|id| id.as_str().into())
            .collect();
        subscribers.sort_unstable();
        subscribers.dedup();
        if subscribers.len() != request.subscribers.len() {
            return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                "publish_channel",
                "channel subscribers must be unique",
            )));
        }
        let engine = self.engine_mut().map_err(SyscallRefusal::Fault)?;
        let handle = resolve_ipc_handle(engine, &request.payload_handle)?;
        let descriptor =
            crate::mm::handle::ObjectDescriptor::from_handle(caller.as_str().into(), &handle, 1);
        if engine
            .task_table()
            .object(descriptor.id)
            .is_some_and(|existing| existing != &descriptor)
        {
            return Err(local_ipc_refusal(
                crate::scheduler::tcb::LocalIpcError::ObjectConflict,
            ));
        }
        let now = crate::scheduler::mailbox::LogicalTime(engine.turn);
        let message = crate::scheduler::mailbox::MailboxMessage {
            id: request.message_id.as_str().into(),
            from: caller.as_str().into(),
            to: request.channel_id.as_str().into(),
            kind: request.message_kind.as_str().into(),
            payload_handle: handle.id,
            priority: crate::types::signal::Urgency::Normal,
            timestamp: now,
            expires_at: request
                .ttl_turns
                .map(|ttl| crate::scheduler::mailbox::LogicalTime(engine.turn.saturating_add(ttl))),
        };
        let accepted = engine
            .task_table_mut()
            .publish_channel(
                caller.as_str(),
                ChannelId(request.channel_id.as_str().into()),
                subscribers,
                message,
                now,
            )
            .map_err(local_ipc_refusal)?;
        engine
            .task_table_mut()
            .register_object(caller.as_str(), descriptor)
            .map_err(local_ipc_refusal)?;
        Ok(local_ipc_outcome(accepted))
    }

    fn plan_receive_mailbox(
        &mut self,
        caller: &TaskId,
        limit: u32,
    ) -> Result<SyscallOutcome, SyscallRefusal> {
        if limit == 0 || limit > 64 {
            return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                "receive_mailbox",
                "limit must be between 1 and 64",
            )));
        }
        let engine = self.engine_mut().map_err(SyscallRefusal::Fault)?;
        let now = crate::scheduler::mailbox::LogicalTime(engine.turn);
        let messages = engine
            .task_table_mut()
            .receive_mailbox(caller.as_str(), now, limit as usize)
            .map_err(local_ipc_refusal)?;
        Ok(ipc_messages_outcome(&messages))
    }

    fn plan_receive_channel(
        &mut self,
        caller: &TaskId,
        channel_id: &str,
    ) -> Result<SyscallOutcome, SyscallRefusal> {
        let engine = self.engine_mut().map_err(SyscallRefusal::Fault)?;
        let now = crate::scheduler::mailbox::LogicalTime(engine.turn);
        let messages = engine
            .task_table_mut()
            .receive_channel(caller.as_str(), &ChannelId(channel_id.into()), now)
            .map_err(local_ipc_refusal)?;
        Ok(ipc_messages_outcome(&messages))
    }

    fn plan_read_object(
        &mut self,
        caller: &TaskId,
        object_id: crate::mm::handle::ObjectId,
    ) -> Result<SyscallOutcome, SyscallRefusal> {
        let engine = self.engine_mut().map_err(SyscallRefusal::Fault)?;
        let descriptor = engine
            .task_table()
            .object(object_id)
            .cloned()
            .ok_or_else(|| {
                SyscallRefusal::Rejected(SyscallRejection::new(
                    "read_object",
                    format!("object {object_id} is not registered"),
                ))
            })?;
        if descriptor.owner.as_str() != caller.as_str()
            && !crate::mm::handle::object_access_allowed_at(
                engine.task_capabilities(caller.as_str()),
                "read",
                &descriptor,
                engine.turn,
            )
        {
            return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                "read_object",
                format!("caller {caller} has no read capability for object {object_id}"),
            )));
        }
        Ok(SyscallOutcome {
            ack: Some(
                serde_json::to_string(&descriptor)
                    .expect("canonical object descriptors are serializable"),
            ),
            ..SyscallOutcome::default()
        })
    }

    /// §10.3 · gate + trust-aware append, with the spawn round deferred to the caller.
    fn append_nodes(
        &mut self,
        config: &ResolvedOperationConfig,
        nodes: &[WireNode],
        caller: &TaskId,
        syscall: CoreSyscall,
        label: &'static str,
    ) -> Result<SyscallOutcome, SyscallRefusal> {
        // §7.3 · an authored node may not name a contract the operation never declared. Same rule
        // as a root spec, and checked on the same side of the gate as identity/acyclicity: a batch
        // with a dangling reference is malformed and must not spend quota.
        for node in nodes {
            self.require_known_contract(config, node.run_spec.as_ref())
                .map_err(|fault| {
                    SyscallRefusal::Rejected(SyscallRejection::new(label, fault.message))
                })?;
        }
        // Batch-relative identity and acyclicity are checked before the gate sees a count, so a
        // malformed batch never spends quota.
        let core_nodes = build_core_spec(&WireSpec {
            name: String::new(),
            nodes: nodes.to_vec(),
        })
        .map_err(|fault| SyscallRefusal::Rejected(SyscallRejection::new(label, fault.message)))?
        .nodes;

        let engine = self.engine_mut().map_err(SyscallRefusal::Fault)?;
        let admitted = engine.append_workflow_nodes(
            core_nodes,
            // §7.6 · the submitter is the derived caller, never an optional host field. The
            // historical `submitter_agent_id: Option<String>` erred open — omitting it skipped the
            // quarantine coercion entirely — and there is no shape here that can omit it.
            Some(caller.as_str()),
            // `append_workflow_nodes` refills the count from the batch, so the two entry points
            // cannot disagree about what they meter.
            syscall,
            label,
        );
        if !admitted {
            // `append_workflow_nodes` already pushed the rejection observation and the model-facing
            // note; re-recording it here would double the audit fact.
            return Ok(SyscallOutcome::default());
        }
        // §10.3 · deterministic node identity is a kernel fact. An appended batch lands at the end
        // of the index-addressed DAG, so mirroring its wire ids here is what lets a later spawn
        // effect and the workflow terminal still name the node the caller declared — rather than
        // falling back to the internal `wf-nodeN` id.
        self.node_ids
            .extend(nodes.iter().map(|node| node.node_id.clone()));
        self.workflow_nodes.extend_from_slice(nodes);
        Ok(SyscallOutcome {
            effects: Vec::new(),
            focus: None,
            needs_workflow_round: true,
            ack: None,
        })
    }

    fn plan_memory_write(
        &mut self,
        context: &PlanContext<'_>,
        causation: &SyscallCausation,
        proposal: &super::syscall::MemoryWriteProposal,
        effect_index: &mut u32,
    ) -> Result<SyscallOutcome, SyscallRefusal> {
        let Some(binding) = context.config.memory_access.clone() else {
            return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                "write_memory",
                "this operation holds no memory binding, so it can author no memory record",
            )));
        };
        if !binding.capabilities.write {
            return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                "write_memory",
                "this operation's memory binding is read-only",
            )));
        }
        self.require_effect_support(context.config, EffectKindTag::PersistMemory)
            .map_err(SyscallRefusal::Fault)?;
        let engine = self.engine_mut().map_err(SyscallRefusal::Fault)?;
        let disposition = engine.gate_memory_write_proposal();
        if !disposition.is_allowed() {
            return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                "write_memory",
                denial_reason(&disposition, "memory write denied"),
            )));
        }
        // §22.13 · the proposal contributed name/kind/content/evidence and nothing else. Tenant,
        // author, trust, timestamp and provenance are authored here, from the operation's binding,
        // the envelope's accepted time and the derived causation.
        let authored = AuthoredMemoryWrite {
            binding_id: binding.binding_id.clone(),
            name: proposal.name.clone(),
            kind: proposal.kind,
            size_bytes: proposal.content.len() as u32,
        };
        let effect = EffectKind::PersistMemory(PersistMemoryEffect {
            binding,
            memory: CanonicalMemoryWrite {
                name: proposal.name.clone(),
                kind: proposal.kind,
                content: proposal.content.clone(),
                description: proposal.description.clone(),
                evidence_refs: proposal.evidence_refs.clone(),
                accepted_at_ms: context.input.observed_at_ms,
                causation: causation.clone(),
            },
        });
        let published = self.mint_effect(context, effect, effect_index);
        self.pending_memory_writes
            .insert(published.effect_id.clone(), authored);
        Ok(SyscallOutcome {
            effects: vec![published],
            focus: None,
            needs_workflow_round: false,
            ack: None,
        })
    }

    fn plan_memory_query(
        &mut self,
        context: &PlanContext<'_>,
        causation: &SyscallCausation,
        proposal: &super::syscall::MemoryQueryProposal,
        effect_index: &mut u32,
    ) -> Result<SyscallOutcome, SyscallRefusal> {
        let Some(binding) = context.config.memory_access.clone() else {
            return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                "query_memory",
                "this operation holds no memory binding, so it can read no memory record",
            )));
        };
        if !binding.capabilities.read {
            return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                "query_memory",
                "this operation's memory binding is write-only",
            )));
        }
        self.require_effect_support(context.config, EffectKindTag::QueryMemory)
            .map_err(SyscallRefusal::Fault)?;
        // The retrieval width is the operation's policy, clamped — the host does not re-decide it
        // and a model cannot widen it by asking for more.
        let ceiling = context.config.memory_policy.retrieval_top_k;
        let requested_k = proposal.limit.unwrap_or(ceiling).clamp(1, ceiling);
        let authored = AuthoredMemoryQuery {
            binding_id: binding.binding_id.clone(),
            text: proposal.text.clone(),
            requested_k,
        };
        let effect = EffectKind::QueryMemory(QueryMemoryEffect {
            binding,
            query: CanonicalMemoryQuery {
                text: proposal.text.clone(),
                kinds: proposal.kinds.clone(),
                accepted_at_ms: context.input.observed_at_ms,
                causation: causation.clone(),
            },
            requested_k,
        });
        let published = self.mint_effect(context, effect, effect_index);
        self.pending_memory_queries
            .insert(published.effect_id.clone(), authored);
        Ok(SyscallOutcome {
            effects: vec![published],
            focus: None,
            needs_workflow_round: false,
            ack: None,
        })
    }

    /// §7.6 / §7.10 rule 4 · `read_result` reduces to exactly one thing: a `LoadPayload` effect for
    /// a body the caller already holds an address for.
    ///
    /// Two refusals, both *rejections* rather than faults — the caller was established, so what is
    /// refused is the address it named, and the transition that carried it still commits with an
    /// audit fact the model reads on its next turn:
    ///
    /// - an address that is not in this operation's handle table. A page-in reaches only what the
    ///   caller already holds, which is what stops `read_result` from becoming a general read
    ///   primitive — the historical SDK answered it by scanning a spool directory and then the
    ///   session log, so any path-shaped string was a readable address.
    /// - an address whose body core still holds. `Resident` and `Collapsed` are not paged out at
    ///   all. Neither yields a locator the kernel could hand back,
    ///   and fabricating one is exactly the confusion the closed union removes.
    fn plan_page_in(
        &mut self,
        context: &PlanContext<'_>,
        caller: &TaskId,
        handle_id: &super::scalar::HandleId,
        effect_index: &mut u32,
    ) -> Result<SyscallOutcome, SyscallRefusal> {
        let engine = self.engine_mut().map_err(SyscallRefusal::Fault)?;
        if let Some(handle) = engine.ctx.handles.all().iter().find(|handle| {
            handle.source.as_deref() == Some(handle_id.as_str())
                || handle.id.to_string() == handle_id.as_str()
        }) && let Some(descriptor) = engine.task_table().object(handle.id)
            && descriptor.owner.as_str() != caller.as_str()
            && !crate::mm::handle::object_access_allowed_at(
                engine.task_capabilities(caller.as_str()),
                "read",
                descriptor,
                engine.turn,
            )
        {
            return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                READ_RESULT_TOOL_NAME,
                format!(
                    "caller {caller} has no read capability for shared object {}",
                    descriptor.id
                ),
            )));
        }
        let Some(residency) = engine.ctx.payload_residency(handle_id.as_str()).cloned() else {
            return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                READ_RESULT_TOOL_NAME,
                format!(
                    "handle {handle_id} is not reachable in this operation's handle table; a \
                     page-in addresses only what the caller already holds"
                ),
            )));
        };
        let (Some(payload_ref), Some(digest)) = (residency.payload_ref(), residency.digest())
        else {
            return Err(SyscallRefusal::Rejected(SyscallRejection::new(
                READ_RESULT_TOOL_NAME,
                format!(
                    "handle {handle_id} is {} — its body is held by this kernel, not by the \
                     payload store, so there is nothing to page in",
                    residency.label()
                ),
            )));
        };
        let payload_ref = PayloadRef::new(payload_ref).map_err(|error| {
            SyscallRefusal::Fault(KernelFault::new(
                KernelFaultCode::MalformedEnvelope,
                format!(
                    "handle {handle_id} records an unusable payload locator: {}",
                    error.message
                ),
            ))
        })?;
        self.require_effect_support(context.config, EffectKindTag::LoadPayload)
            .map_err(SyscallRefusal::Fault)?;
        let effect = EffectKind::LoadPayload(LoadPayloadEffect {
            handle_id: handle_id.clone(),
            payload_ref,
        });
        let published = self.mint_effect(context, effect, effect_index);
        self.pending_payload_loads.insert(
            published.effect_id.clone(),
            PendingPayloadLoad {
                handle_id: handle_id.as_str().to_string(),
                digest: digest.to_string(),
                original_size: match &residency {
                    Residency::External { original_size, .. } => Some(*original_size),
                    _ => None,
                },
            },
        );
        Ok(SyscallOutcome {
            effects: vec![published],
            focus: None,
            needs_workflow_round: false,
            ack: None,
        })
    }

    /// Record a refused request as an audit fact the model reads on its next turn (§7.6, §7.7).
    fn note_rejection(&mut self, rejection: SyscallRejection) {
        let Some(engine) = self.engine.as_mut() else {
            return;
        };
        let note = crate::scheduler::rollback::build_control_rejection_note(
            rejection.operation,
            &rejection.reason,
            engine.ctx.config.verbose_control_notes,
        );
        engine.ctx.push_signal(note);
        let turn = engine.turn;
        engine
            .observations
            .push(KernelObservation::ControlRequestRejected {
                turn,
                operation: rejection.operation.to_string(),
                subject: rejection.subject,
                reason: rejection.reason,
            });
    }

    fn mint_effect(
        &self,
        context: &PlanContext<'_>,
        effect: EffectKind,
        effect_index: &mut u32,
    ) -> KernelEffect {
        let effect_id =
            mint_effect_id(&context.input.operation_id, context.step_seq, *effect_index);
        *effect_index += 1;
        KernelEffect {
            effect_id,
            causation_input_id: context.input.input_id.clone(),
            effect,
        }
    }

    /// Fold one engine action's effects into an accumulating step.
    fn extend_with_action(
        &mut self,
        context: &PlanContext<'_>,
        action: LoopAction,
        root_kind: RootKind,
        effect_index: &mut u32,
        effects: &mut Vec<KernelEffect>,
    ) -> Result<(), KernelFault> {
        match self.disposition_for_at(context, action, root_kind, effect_index)? {
            StepDisposition::Effects(published) => {
                effects.extend(published.effects);
                Ok(())
            }
            StepDisposition::Terminal(_) => Err(KernelFault::new(
                KernelFaultCode::InvalidLifecycle,
                "a syscall batch cannot terminate the operation; §7.12 admits effects or a \
                 terminal, never both in one step"
                    .to_string(),
            )),
        }
    }

    // ----- internals -----

    fn plan_inner(&mut self, context: &PlanContext<'_>) -> Result<PlannedStep, KernelFault> {
        // Every transition reads the observations *its own* semantic call produced. `start`/`feed`
        // clear the buffer themselves; `load_workflow` and `resolve_workflow_spawn` do not, so the
        // driver clears it here rather than letting a stale `WorkflowCompleted` from an earlier
        // step decide a later one's disposition.
        if let Some(engine) = self.engine.as_mut() {
            engine.take_observations();
            // §11.2 · the envelope's accepted time is this operation's only clock, and it is fed
            // once, here, before any semantic call. Every clock-dependent decision the step makes
            // (signal TTL and deadline escalation, rate-limit windows, the wall-time budget axis)
            // therefore reads a fact the journal already holds, so a replay decides identically.
            engine.observe_accepted_time(context.input.observed_at_ms.get());
            engine
                .task_table_mut()
                .wake_expired_timers(context.input.observed_at_ms.get());
        }
        match &context.input.input {
            NormalizedPayload::ConfigureOperation(configure) => {
                self.plan_configure(&configure.config)
            }
            NormalizedPayload::StartOperation(start) => {
                self.plan_start(context, &start.entry, &start.initial_context)
            }
            NormalizedPayload::ResolveEffect(resolve) => self.plan_resolve_effect(context, resolve),
            NormalizedPayload::DeliverExternalEvent(event) => {
                self.plan_external_event(context, &event.event)
            }
            NormalizedPayload::HostControl(control) => {
                self.plan_host_control(context, &control.command)
            }
        }
    }

    /// §6.1.2 · genesis. The engine is built from the **resolved** configuration the record froze,
    /// never from this binary's defaults, so a rebuild on a newer kernel plans the same step.
    fn plan_configure(
        &mut self,
        config: &ResolvedOperationConfig,
    ) -> Result<PlannedStep, KernelFault> {
        self.engine = Some(build_engine(config));
        // §13.2 · the live-mutable half starts at revision 0, holding exactly what the genesis
        // record froze. A patch rebases onto this, never onto a compile-time default.
        self.policy = Some(LivePolicyState::new(config.clone()));
        Ok(PlannedStep::quiet(None, None))
    }

    /// §7.4 · the one atomic root start.
    ///
    /// Both arms are ordered the same way and for the same reason: every refusal this transition
    /// can raise is decided while nothing has moved, and only then does the semantic engine
    /// advance. A rejected root start therefore leaves an operation that is still `Configured` and
    /// still free to choose a root.
    fn plan_start(
        &mut self,
        context: &PlanContext<'_>,
        entry: &RootEntry,
        initial: &InitialContext,
    ) -> Result<PlannedStep, KernelFault> {
        if self.root_kind.is_some() || self.staged.is_some() {
            return Err(KernelFault::new(
                KernelFaultCode::InvalidLifecycle,
                "this operation already has a root; a root entry is chosen once and is immutable \
                 (§6.1.3–6.1.5)"
                    .to_string(),
            ));
        }

        match entry {
            RootEntry::Agent(agent) => {
                self.require_effect_support(context.config, EffectKindTag::CallProvider)?;
                self.require_known_contract(context.config, agent.run_spec.as_ref())?;
                let task = runtime_task(&agent.task);
                let run_spec = agent.run_spec.as_ref().map(agent_run_spec);

                // ----- past this line the semantic engine advances -----
                // The cascade is installed before `start`, which is the engine's own precondition:
                // a contract loaded afterwards would leave phase 0 already behind the run.
                self.load_verification_contract(context.config, agent.run_spec.as_ref())?;
                let engine = self.engine_mut()?;
                seed_initial_context(engine, initial);
                engine.run_spec = run_spec;
                let action = engine.start(task);
                let disposition = self.disposition_for(context, action, RootKind::Agent)?;
                if !publishes(&disposition, EffectKindTag::CallProvider) {
                    return Err(KernelFault::new(
                        KernelFaultCode::InvalidLifecycle,
                        "an agent root's first committed step must publish a provider call (§7.4)"
                            .to_string(),
                    ));
                }
                Ok(PlannedStep {
                    root_kind: Some(RootKind::Agent),
                    focus: Some(ExecutionFocus::agent_turn(root_task_id())),
                    observations: Vec::new(),
                    disposition,
                })
            }
            RootEntry::Workflow(workflow) => {
                self.require_effect_support(context.config, EffectKindTag::SpawnTasks)?;
                for node in &workflow.spec.nodes {
                    self.require_known_contract(context.config, node.run_spec.as_ref())?;
                }
                if workflow.spec.nodes.is_empty() {
                    return Err(KernelFault::new(
                        KernelFaultCode::InvalidConfig,
                        "a workflow root with no nodes has no first task to spawn; a root entry \
                         must be able to publish its first effect (§10.1)"
                            .to_string(),
                    ));
                }
                let core_spec = build_core_spec(&workflow.spec)?;
                let node_ids = wire_node_ids(&workflow.spec);
                let workflow_id = mint_workflow_id(&context.input.operation_id, context.step_seq);

                // ----- past this line the semantic engine advances -----
                let engine = self.engine_mut()?;
                seed_initial_context(engine, initial);
                // §6.1.7 — this DAG *is* the root, so its completion is the operation's terminal
                // rather than one more turn of a parent agent loop.
                engine.set_root_workflow(true);
                let action = engine.load_workflow_as(core_spec, ROOT_TASK_ID);
                self.node_ids = node_ids;
                self.workflow_nodes = workflow.spec.nodes.clone();
                self.workflow_id = Some(workflow_id.clone());
                let disposition = self.disposition_for(context, action, RootKind::Workflow)?;
                if !publishes(&disposition, EffectKindTag::SpawnTasks) {
                    return Err(KernelFault::new(
                        KernelFaultCode::InvalidLifecycle,
                        "a workflow root's first committed step must publish a task spawn, never a \
                         provider call (§10.1)"
                            .to_string(),
                    ));
                }
                Ok(PlannedStep {
                    root_kind: Some(RootKind::Workflow),
                    focus: Some(ExecutionFocus::workflow_controller(workflow_id, None)),
                    observations: Vec::new(),
                    disposition,
                })
            }
        }
    }

    /// §7.9 · effect resolution — the single entry every pending effect is answered through.
    ///
    /// Two arms and no third, mirroring [`EffectOutcome`]. The transaction has already decided
    /// *admissibility* (still pending, kind matches, not a conflicting duplicate — §15.3), so what
    /// is left here is purely semantic: which internal mechanism the outcome feeds.
    fn plan_resolve_effect(
        &mut self,
        context: &PlanContext<'_>,
        resolve: &ResolveEffect,
    ) -> Result<PlannedStep, KernelFault> {
        let planned = match &resolve.outcome {
            EffectOutcome::Succeeded(success) => {
                self.plan_effect_success(context, &resolve.effect_id, &success.result)
            }
            EffectOutcome::Failed(failed) => {
                self.plan_effect_failure(context, &resolve.effect_id, &failed.failure)
            }
        };
        if planned.is_ok() {
            self.engine_mut()?
                .task_table_mut()
                .notify(&WaitKey::Effect(resolve.effect_id.clone()));
        }
        planned
    }

    /// The success half: each variant reduces onto the mechanism that already owns that fact.
    fn plan_effect_success(
        &mut self,
        context: &PlanContext<'_>,
        effect_id: &EffectId,
        success: &EffectSuccess,
    ) -> Result<PlannedStep, KernelFault> {
        match success {
            EffectSuccess::Provider(provider) => match &provider.outcome {
                ProviderOutcome::Completed(completed) => {
                    self.plan_provider_completed(context, effect_id, completed)
                }
                // §7.9 · a *semantic* outcome, not a transport failure: the kernel compacts and
                // re-emits a provider call, and no vendor text is read to decide that (§22.8).
                ProviderOutcome::ContextOverflow(overflow) => {
                    let root_kind = self.require_root_kind()?;
                    self.require_pending_provider_call(effect_id)?;
                    let engine = self.engine_mut()?;
                    if let Some(tokens) = overflow.observed_input_tokens {
                        engine.ctx.set_observed_prompt_tokens(tokens);
                    }
                    let action = engine.recover_from_context_overflow();
                    self.provider_calls.remove(effect_id);
                    self.continue_after(context, action, root_kind)
                }
            },
            EffectSuccess::Tools(tools) => {
                let root_kind = self.require_root_kind()?;
                // Zero-mutation discipline: the whole batch is adjudicated against the payload
                // policy before any of it reaches the engine, so a batch with one illegal result
                // does not half-land.
                for payload in &tools.results {
                    check_payload_policy(payload, &context.config.payload_policy)?;
                }
                let mut results: Vec<ToolResult> =
                    tools.results.iter().map(core_tool_result).collect();
                results.extend(self.close_out_fatal_batch(&tools.results)?);
                let mut action = self.engine_mut()?.feed(LoopEvent::ToolResults { results });
                self.record_external_payloads(&tools.results)?;
                self.engine_mut()?.refresh_call_llm_action(&mut action);
                self.continue_after(context, action, root_kind)
            }
            EffectSuccess::Approval(approval) => {
                let root_kind = self.require_root_kind()?;
                let approved = approval
                    .approved_call_ids
                    .iter()
                    .map(|id| id.as_str().to_string())
                    .collect();
                let denied = approval
                    .denied_call_ids
                    .iter()
                    .map(|id| id.as_str().to_string())
                    .collect();
                let action = self.engine_mut()?.resolve_approval(approved, denied);
                self.engine_mut()?
                    .task_table_mut()
                    .notify(&WaitKey::Approval(ApprovalId("pending".into())));
                self.continue_after(context, action, root_kind)
            }
            EffectSuccess::TasksSpawned(spawned) => {
                let mut started: Vec<String> = Vec::new();
                let mut failures: Vec<WorkflowSpawnFailure> = Vec::new();
                for attempt in &spawned.attempts {
                    let agent_id = attempt.task_id.as_str().to_string();
                    match &attempt.outcome {
                        super::effect::TaskLaunchStatus::Started(_) => started.push(agent_id),
                        super::effect::TaskLaunchStatus::Failed(failed) => {
                            // §10.4 · a failed launch terminates the attempt. Dropping it here is
                            // what makes a later completion naming it a stale causation rather than
                            // a resurrection of a task that never started.
                            self.attempts.remove(&agent_id);
                            failures.push(WorkflowSpawnFailure {
                                agent_id,
                                error: failed.failure.message.clone(),
                            });
                        }
                    }
                }
                let root_kind = self.require_root_kind()?;
                let action = self.engine_mut()?.resolve_workflow_spawn(started, failures);
                self.continue_after(context, action, root_kind)
            }
            EffectSuccess::TasksPreempted(preempted) => {
                let root_kind = self.require_root_kind()?;
                // §10.4 · every attempt named here is spent, whichever way it went: a preempted
                // child is gone, and one that had already finished is finished. Either way a later
                // completion naming it is a stale causation.
                for attempt in &preempted.attempts {
                    self.attempts.remove(attempt.task_id.as_str());
                }
                let action = self.engine_mut()?.resolve_preempt();
                self.continue_after(context, action, root_kind)
            }
            EffectSuccess::MemoryPersisted(persisted) => {
                self.commit_memory_write(effect_id, Some(&persisted.receipt), None)
            }
            EffectSuccess::MemoryQueried(queried) => {
                let root_kind = self.require_root_kind()?;
                let Some(query) = self.pending_memory_queries.remove(effect_id) else {
                    return Err(unowned_resolution(effect_id, "memory query"));
                };
                let turn = self.engine_mut()?.turn;
                let mut recalled = Vec::with_capacity(queried.recalls.len());
                for recall in &queried.recalls {
                    // §22.13 · the host answers with records, never with authority: the recall is
                    // rendered into history as content the model reads, and nothing about it
                    // rewrites this operation's binding, trust or provenance.
                    let content = format!(
                        "[MEMORY record_ref={} kind={}] {}",
                        recall.record_ref,
                        wire_memory_kind_label(recall.kind),
                        recall.content
                    );
                    let engine = self.engine_mut()?;
                    let tokens = engine.ctx.engine.count(&content).max(1);
                    engine.ctx.push_history(Message::user(content), tokens);
                    recalled.push(recall.record_ref.as_str().to_string());
                }
                // The recalls are in history now, so the turn that asked for them resumes with a
                // rendered context that contains them. The observation is pushed *after* the
                // resume, which clears the buffer at its head: a fact recorded before it would be
                // erased by the very continuation it describes.
                let engine = self.engine_mut()?;
                let action = engine.resume_after_preload();
                engine.observations.push(KernelObservation::MemoryQueried {
                    turn,
                    scope: binding_scope(&query.binding_id),
                    query: query.text.clone(),
                    requested_k: query.requested_k as usize,
                    requires_async_response: false,
                });
                let mut step = self.continue_after(context, action, root_kind)?;
                step.focus = self.focus.clone();
                Ok(step)
            }
            EffectSuccess::PageOutArchived(archived) => {
                let root_kind = self.require_root_kind()?;
                self.verify_page_out_receipt(context, effect_id, &archived.receipt)?;
                let receipt = &archived.receipt;
                let action = self
                    .engine_mut()?
                    .commit_page_out_archive(Some(receipt.payload_ref.as_str().to_string()));
                // B19 · the archived body is `PagedOut`, not `External`: it *was* resident and left
                // under pressure. Both states page in through the same `LoadPayload` effect, and
                // keeping them apart is what lets a restore say which of the two happened.
                let engine = self.engine_mut()?;
                let turn = engine.turn;
                let previous = engine.ctx.set_payload_residency(
                    receipt.handle_id.as_str(),
                    HandleKind::MemoryPage,
                    0,
                    Residency::PagedOut {
                        payload_ref: receipt.payload_ref.as_str().to_string(),
                        digest: receipt.digest.as_str().to_string(),
                    },
                );
                engine
                    .observations
                    .push(KernelObservation::PayloadResidencyChanged {
                        turn,
                        handle_id: receipt.handle_id.as_str().to_string(),
                        from: previous.map(|residency| residency.label().to_string()),
                        to: "paged_out".to_string(),
                        payload_ref: Some(receipt.payload_ref.as_str().to_string()),
                        original_size: receipt.original_size.get(),
                    });
                self.continue_after(context, action, root_kind)
            }
            EffectSuccess::MilestoneEvaluated(evaluated) => {
                let root_kind = self.require_root_kind()?;
                let action = self.engine_mut()?.feed(LoopEvent::MilestoneResult {
                    result: core_milestone_result(&evaluated.result),
                });
                self.continue_after(context, action, root_kind)
            }
            EffectSuccess::PayloadLoaded(loaded) => {
                self.commit_payload_load(context, effect_id, loaded)
            }
            EffectSuccess::PromptMeasured(_) => Err(KernelFault::new(
                KernelFaultCode::UnexpectedEffectOutcome,
                "prompt measurement outcomes are reserved but adaptive measurement has no scheduler producer",
            )),
        }
    }

    /// §7.10 · what a `fatal` disposition means on the batch that carried it.
    ///
    /// `fatal` is the host saying "the executor stopped here", so the calls this batch dispatched
    /// but never answered are not pending — they will never be answered at all. Left alone they
    /// would be orphan `tool_call`s: a provider replay with an assistant turn whose calls have no
    /// matching results, which is malformed on every vendor wire and is the exact failure mode the
    /// pairing repair in the SDKs exists to paper over.
    ///
    /// So the kernel closes them out, in the same feed, with the same shape the `ExecuteTools`
    /// *failure* arm uses for a batch that never ran: a committed, model-visible error result that
    /// says the call did not take effect. Same reasoning as v0.2.42's model-facing surface batch —
    /// the model adapts to a failure it can see and cannot adapt to an attempt that was erased.
    ///
    /// Returns an empty vector for an ordinary batch: no fatal result, nothing to close out.
    ///
    /// The fatality scan is **total over residency** (§7.10 rule 9): a call that failed fatally
    /// and spilled a large body is still a call that stopped the executor, so it stops the batch
    /// exactly as an inline one does. Reading only the inline arm would make the close-out depend
    /// on how big the failure's output happened to be.
    fn close_out_fatal_batch(
        &mut self,
        submitted: &[WireToolResultPayload],
    ) -> Result<Vec<ToolResult>, KernelFault> {
        let fatal = submitted
            .iter()
            .any(|payload| payload.disposition().is_fatal());
        if !fatal {
            return Ok(Vec::new());
        }
        let answered: BTreeSet<&str> = submitted
            .iter()
            .map(|payload| payload.call_id().as_str())
            .collect();
        Ok(self
            .engine_mut()?
            .dispatched_tool_calls()
            .iter()
            .filter(|call| !answered.contains(call.id.as_str()))
            .map(|call| ToolResult {
                call_id: call.id.clone(),
                output: Content::Text(
                    "not executed: an earlier call in this batch failed fatally and the executor \
                     stopped. This call did not take effect — re-issue it only if the failure it \
                     followed does not make it pointless."
                        .to_string(),
                ),
                durable_content: None,
                is_error: true,
                is_fatal: false,
                error_kind: Some(ToolErrorKind::Fatal),
                token_count: None,
            })
            .collect())
    }

    /// The failure half (§7.9 · DEC-5).
    ///
    /// One policy decision per effect kind, taken **once**. The kernel never re-emits the same
    /// intent: a host that wants another attempt asks again with a new causation and keeps its own
    /// idempotency on the effect id / launch token. The kind comes from the effect the kernel
    /// itself published — `HostEffectFailure` is deliberately kind-agnostic (§7.9), so it is not,
    /// and must not be, the thing that selects the decision.
    fn plan_effect_failure(
        &mut self,
        context: &PlanContext<'_>,
        effect_id: &EffectId,
        failure: &HostEffectFailure,
    ) -> Result<PlannedStep, KernelFault> {
        let Some(pending) = context.resolving else {
            return Err(unowned_resolution(effect_id, "effect"));
        };
        let tag = pending.tag();
        match tag {
            // The loop cannot advance without a provider turn, and asking again would be the
            // redispatch DEC-5 deletes. Terminal.
            EffectKindTag::CallProvider => {
                self.provider_calls.remove(effect_id);
                self.host_effect_terminal(tag, failure)
            }
            // An unverifiable phase must not advance — that is the whole point of the gate — and a
            // second evaluation is the same intent. Terminal.
            EffectKindTag::EvaluateMilestone => self.host_effect_terminal(tag, failure),
            // The batch did not run. Answer every dispatched call with a visible error result and
            // let the model adapt: that is a *different* next request, not a retry of this one.
            EffectKindTag::ExecuteTools => {
                let root_kind = self.require_root_kind()?;
                let engine = self.engine_mut()?;
                let results = engine
                    .dispatched_tool_calls()
                    .iter()
                    .map(|call| ToolResult {
                        call_id: call.id.clone(),
                        output: Content::Text(format!(
                            "not executed: the executor could not run this batch ({}). The call \
                             did not take effect — try a different approach or a smaller step.",
                            failure.kind.as_str()
                        )),
                        durable_content: None,
                        is_error: true,
                        is_fatal: false,
                        error_kind: Some(ToolErrorKind::Fatal),
                        token_count: None,
                    })
                    .collect();
                let action = engine.feed(LoopEvent::ToolResults { results });
                self.continue_after(context, action, root_kind)
            }
            // Fail closed: no approval was obtained, so nothing gated is approved. `resolve_approval`
            // with an empty verdict denies exactly the gated calls and resumes the rest.
            EffectKindTag::RequestApproval => {
                let root_kind = self.require_root_kind()?;
                let engine = self.engine_mut()?;
                let turn = engine.turn;
                // Pushed after the resolution, which clears the buffer at its head: a fact
                // recorded before it would be erased by the continuation it describes.
                let action = engine.resolve_approval(Vec::new(), Vec::new());
                engine
                    .observations
                    .push(KernelObservation::ApprovalResolutionFailed {
                        turn,
                        error: host_failure_text(failure),
                    });
                self.continue_after(context, action, root_kind)
            }
            // No task in the batch started. Charge the failure against exactly the batch the kernel
            // published and let the DAG's own dependency policy decide what that starves.
            EffectKindTag::SpawnTasks => {
                let root_kind = self.require_root_kind()?;
                let error = host_failure_text(failure);
                let engine = self.engine_mut()?;
                let failures: Vec<WorkflowSpawnFailure> = engine
                    .pending_spawn_agent_ids()
                    .into_iter()
                    .map(|agent_id| WorkflowSpawnFailure {
                        agent_id,
                        error: error.clone(),
                    })
                    .collect();
                for failed in &failures {
                    self.attempts.remove(&failed.agent_id);
                }
                let action = self
                    .engine_mut()?
                    .resolve_workflow_spawn(Vec::new(), failures);
                self.continue_after(context, action, root_kind)
            }
            // The children were not stopped. Record it and resume: re-issuing the preemption is the
            // unbounded `retry_preempt` loop DEC-5 deletes.
            EffectKindTag::PreemptTasks => {
                // for the refusal, not for the value: a resolution with no root is not a resolution
                self.require_root_kind()?;
                let error = host_failure_text(failure);
                let engine = self.engine_mut()?;
                let turn = engine.turn;
                let agent_ids: Vec<String> = match &pending.effect {
                    EffectKind::PreemptTasks(preempt) => preempt
                        .attempts
                        .iter()
                        .map(|attempt| attempt.task_id.as_str().to_string())
                        .collect(),
                    _ => Vec::new(),
                };
                engine
                    .observations
                    .push(KernelObservation::AgentPreemptFailed {
                        turn,
                        agent_ids,
                        reason: match &pending.effect {
                            EffectKind::PreemptTasks(preempt) => preempt.reason.clone(),
                            _ => String::new(),
                        },
                        error,
                    });
                Ok(self.quiet_step())
            }
            EffectKindTag::PersistMemory => {
                self.commit_memory_write(effect_id, None, Some(host_failure_text(failure)))
            }
            // The recall did not happen. The turn that asked for it resumes without it — a memory
            // search that found nothing and a memory store that was unreachable are the same shape
            // to the model, and neither is worth stalling the run for.
            EffectKindTag::QueryMemory => {
                let root_kind = self.require_root_kind()?;
                let Some(query) = self.pending_memory_queries.remove(effect_id) else {
                    return Err(unowned_resolution(effect_id, "memory query"));
                };
                let error = host_failure_text(failure);
                let engine = self.engine_mut()?;
                let turn = engine.turn;
                let action = engine.resume_after_preload();
                engine
                    .observations
                    .push(KernelObservation::MemoryQueryFailed {
                        turn,
                        scope: binding_scope(&query.binding_id),
                        query: query.text,
                        error,
                    });
                let mut step = self.continue_after(context, action, root_kind)?;
                step.focus = self.focus.clone();
                Ok(step)
            }
            // Abandon the archive: its compaction already happened in this kernel, so the run stays
            // live and degraded rather than dying on a best-effort durability effect.
            EffectKindTag::ArchivePageOut => {
                let root_kind = self.require_root_kind()?;
                let action = self
                    .engine_mut()?
                    .abandon_page_out_archive(host_failure_text(failure));
                self.continue_after(context, action, root_kind)
            }
            // DEC-5 · abandon the read. A body the host cannot produce leaves the operation exactly
            // where it was — the preview is still in context and the handle still names the
            // reference — so the model is told the read failed and takes its next turn, rather than
            // the kernel re-issuing the same load or killing a live run over one page-in.
            EffectKindTag::LoadPayload => {
                let root_kind = self.require_root_kind()?;
                let handle_id = self
                    .pending_payload_loads
                    .remove(effect_id)
                    .map(|pending| pending.handle_id)
                    .unwrap_or_default();
                let error = host_failure_text(failure);
                let engine = self.engine_mut()?;
                let turn = engine.turn;
                let action = engine.resume_after_preload();
                engine
                    .observations
                    .push(KernelObservation::PayloadLoadFailed {
                        turn,
                        handle_id,
                        error,
                    });
                let mut step = self.continue_after(context, action, root_kind)?;
                step.focus = self.focus.clone();
                Ok(step)
            }
            EffectKindTag::MeasurePrompt => Err(KernelFault::new(
                KernelFaultCode::UnexpectedEffectOutcome,
                "prompt measurement failures are reserved but adaptive measurement has no scheduler producer",
            )),
        }
    }

    /// The two effect kinds whose absence makes the operation unsound rather than degraded.
    fn host_effect_terminal(
        &mut self,
        tag: EffectKindTag,
        failure: &HostEffectFailure,
    ) -> Result<PlannedStep, KernelFault> {
        let root_kind = self.require_root_kind()?;
        let usage = self.usage_report();
        // The engine stops too: the operation is over, and a later input must not find a loop that
        // still believes it is running.
        if let Some(engine) = self.engine.as_mut() {
            engine.close_for_host_effect_failure();
        }
        Ok(PlannedStep {
            root_kind: Some(root_kind),
            focus: self.focus.clone(),
            observations: Vec::new(),
            disposition: StepDisposition::Terminal(TerminalDisposition {
                terminal: KernelTerminal::Failed(FailedTerminal {
                    failure: KernelFailure {
                        code: KernelFailureCode::HostEffectFailed,
                        message: format!(
                            "the host could not execute this operation's {tag} effect ({}){}",
                            failure.kind.as_str(),
                            if failure.message.is_empty() {
                                String::new()
                            } else {
                                format!(": {}", failure.message)
                            }
                        ),
                    },
                    usage,
                }),
            }),
        })
    }

    /// A transition that changes kernel state but publishes nothing.
    ///
    /// Reads the folded root kind rather than taking one: a control command is admissible while the
    /// operation is still only `Configured`, so the value it reports may legitimately be `None`,
    /// and every other caller already proved a root exists through `require_root_kind`.
    fn quiet_step(&self) -> PlannedStep {
        PlannedStep {
            root_kind: self.root_kind,
            focus: self.focus.clone(),
            observations: Vec::new(),
            disposition: StepDisposition::Effects(EffectsDisposition::default()),
        }
    }

    /// §22.13 · the memory write resolution. The record the kernel authored is the record; the
    /// host receipt contributes only its own opaque locator and digest, and never a name, kind,
    /// size, trust or provenance the kernel did not derive.
    fn commit_memory_write(
        &mut self,
        effect_id: &EffectId,
        receipt: Option<&super::effect::MemoryPersistReceipt>,
        failure: Option<String>,
    ) -> Result<PlannedStep, KernelFault> {
        // for the refusal, not for the value: a resolution with no root is not a resolution
        self.require_root_kind()?;
        let Some(authored) = self.pending_memory_writes.remove(effect_id) else {
            return Err(unowned_resolution(effect_id, "memory write"));
        };
        let engine = self.engine_mut()?;
        let turn = engine.turn;
        match (receipt, failure) {
            (Some(receipt), _) => {
                engine.observations.push(KernelObservation::MemoryWritten {
                    turn,
                    record_id: receipt.record_ref.as_str().to_string(),
                    scope: binding_scope(&authored.binding_id),
                    memory_kind: core_memory_kind(authored.kind),
                    name: authored.name,
                    size_bytes: authored.size_bytes,
                });
            }
            (None, Some(error)) => {
                engine
                    .observations
                    .push(KernelObservation::MemoryWriteFailed {
                        turn,
                        // No record exists to name, so the audit fact names the *intent* — the
                        // kernel-authored key — instead of a host id it never received.
                        record_id: authored.name,
                        error,
                    });
            }
            (None, None) => unreachable!("a memory resolution is either a receipt or a failure"),
        }
        Ok(self.quiet_step())
    }

    /// §7.10 rule 4 · a body the host paged back in.
    ///
    /// Three things are checked before a byte enters context, and all three are the same question
    /// asked from different sides: *is this the body that left?* The effect must be one this kernel
    /// published, the outcome must name the handle that effect addressed, and the content must
    /// reproduce the digest the residency recorded. The kernel never saw the body, so the digest is
    /// the only evidence there is — which is why a mismatch is an
    /// [`UnexpectedEffectOutcome`](KernelFaultCode::UnexpectedEffectOutcome) with zero mutation and
    /// not a degraded read.
    fn commit_payload_load(
        &mut self,
        context: &PlanContext<'_>,
        effect_id: &EffectId,
        loaded: &super::effect::PayloadLoadedSuccess,
    ) -> Result<PlannedStep, KernelFault> {
        let root_kind = self.require_root_kind()?;
        let Some(pending) = self.pending_payload_loads.get(effect_id).cloned() else {
            return Err(unowned_resolution(effect_id, "payload load"));
        };
        let mismatch = |what: &str| {
            Err(KernelFault::new(
                KernelFaultCode::UnexpectedEffectOutcome,
                format!(
                    "the payload loaded for effect {effect_id} is not the body the kernel paged \
                     out: {what}"
                ),
            ))
        };
        if loaded.handle_id.as_str() != pending.handle_id {
            return mismatch(&format!(
                "it names handle {}, but the effect addressed {}",
                loaded.handle_id, pending.handle_id
            ));
        }
        let content = loaded.payload.content.as_str();
        if loaded.payload.original_size.get() != content.len() as u64 {
            return mismatch(&format!(
                "it declares {} bytes and carries {}",
                loaded.payload.original_size,
                content.len()
            ));
        }
        if let Some(original_size) = pending.original_size
            && loaded.payload.original_size.get() != original_size
        {
            return mismatch(&format!(
                "it carries {} bytes and the handle records {original_size}",
                loaded.payload.original_size
            ));
        }
        let digest = super::record::canonical_digest(content.as_bytes());
        if digest.as_str() != pending.digest {
            return mismatch(&format!(
                "its content digests to {digest}, and the handle records {}",
                pending.digest
            ));
        }

        // ----- past this line the semantic engine advances -----
        self.pending_payload_loads.remove(effect_id);
        let engine = self.engine_mut()?;
        let turn = engine.turn;
        // The body enters as its own unit of history, exactly as a memory recall does: the preview
        // that stands in for it is left untouched, so nothing that was already rendered is
        // rewritten and the model reads the page-in as the answer to the read it asked for.
        let body = format!("[PAYLOAD handle_id={}]\n{content}", pending.handle_id);
        let tokens = engine.ctx.engine.count(&body).max(1);
        engine.ctx.push_history(Message::user(body), tokens);
        let previous = engine.ctx.set_payload_residency(
            &pending.handle_id,
            HandleKind::ToolResult,
            tokens,
            Residency::Resident,
        );
        let action = engine.resume_after_preload();
        engine
            .observations
            .push(KernelObservation::PayloadResidencyChanged {
                turn,
                handle_id: pending.handle_id.clone(),
                from: previous.map(|residency| residency.label().to_string()),
                to: "resident".to_string(),
                payload_ref: None,
                original_size: loaded.payload.original_size.get(),
            });
        let mut step = self.continue_after(context, action, root_kind)?;
        step.focus = self.focus.clone();
        Ok(step)
    }

    /// §7.10 rule 3 / §25.9 · move each external result's P3 handle onto the reference the host
    /// supplied, and record the transfer as a fact.
    ///
    /// Runs **after** the engine accepted the batch, because the handle this moves is minted by the
    /// engine as the result enters history — there is nothing to address before that. What lands in
    /// context is the preview; what the handle now says is where the body actually is.
    fn record_external_payloads(
        &mut self,
        payloads: &[WireToolResultPayload],
    ) -> Result<(), KernelFault> {
        for payload in payloads {
            let WireToolResultPayload::External(external) = payload else {
                continue;
            };
            let engine = self.engine_mut()?;
            let turn = engine.turn;
            let previous = engine.ctx.set_payload_residency(
                external.call_id.as_str(),
                HandleKind::ToolResult,
                // The body was never resident: only the preview is, and it is the anchored
                // message's own weight, not this handle's.
                0,
                Residency::External {
                    payload_ref: external.payload_ref.as_str().to_string(),
                    digest: external.digest.as_str().to_string(),
                    original_size: external.original_size.get(),
                },
            );
            engine
                .observations
                .push(KernelObservation::PayloadResidencyChanged {
                    turn,
                    handle_id: external.call_id.as_str().to_string(),
                    from: previous.map(|residency| residency.label().to_string()),
                    to: "external".to_string(),
                    payload_ref: Some(external.payload_ref.as_str().to_string()),
                    original_size: external.original_size.get(),
                });
        }
        Ok(())
    }

    /// The page-out receipt must describe the body the kernel handed over. A host that answers with
    /// a different handle or digest has archived something else, and accepting it would make a
    /// later page-in restore content this operation never evicted.
    fn verify_page_out_receipt(
        &self,
        context: &PlanContext<'_>,
        effect_id: &EffectId,
        receipt: &super::effect::ArchiveReceipt,
    ) -> Result<(), KernelFault> {
        let Some(KernelEffect {
            effect: EffectKind::ArchivePageOut(published),
            ..
        }) = context.resolving
        else {
            return Err(unowned_resolution(effect_id, "page-out archive"));
        };
        if receipt.handle_id != published.handle_id
            || receipt.digest != published.payload.digest
            || receipt.original_size != published.payload.original_size
        {
            return Err(KernelFault::new(
                KernelFaultCode::UnexpectedEffectOutcome,
                format!(
                    "the archive receipt for effect {effect_id} names handle {} / digest {}, but \
                     the kernel published handle {} / digest {}",
                    receipt.handle_id,
                    receipt.digest,
                    published.handle_id,
                    published.payload.digest
                ),
            ));
        }
        Ok(())
    }

    /// Build the page-out effect for one compaction's archived body.
    fn page_out_effect(
        &self,
        context: &PlanContext<'_>,
        summary: Option<&str>,
        archived: &[Message],
        effect_index: u32,
    ) -> Result<ArchivePageOutEffect, KernelFault> {
        let content = serde_json::to_string(archived).map_err(|error| {
            KernelFault::new(
                KernelFaultCode::MalformedEnvelope,
                format!("archived history is not serialisable: {error}"),
            )
        })?;
        let preview_bytes = context.config.payload_policy.preview_bytes as usize;
        let preview = summary
            .map(str::to_string)
            .unwrap_or_else(|| truncate_on_char_boundary(&content, preview_bytes));
        let handle_id = super::scalar::HandleId::new(format!(
            "{}:step:{}:page-out:{effect_index}",
            context.input.operation_id, context.step_seq
        ))
        .map_err(malformed)?;
        Ok(ArchivePageOutEffect {
            handle_id,
            payload: PageOutPayload {
                digest: super::record::canonical_digest(content.as_bytes()),
                original_size: WireU64::new(content.len() as u64),
                content,
                preview,
            },
        })
    }

    fn require_pending_provider_call(
        &self,
        effect_id: &EffectId,
    ) -> Result<&PendingProviderCall, KernelFault> {
        self.provider_calls.get(effect_id).ok_or_else(|| {
            KernelFault::new(
                KernelFaultCode::InvalidAuthority,
                format!(
                    "effect {effect_id} is not a provider call this kernel published, so its \
                     result has no turn to continue (§7.6)"
                ),
            )
        })
    }

    /// §7.7 · host-observed facts. Two arms and no third: a signal the host observed, and a child
    /// attempt that finished.
    fn plan_external_event(
        &mut self,
        context: &PlanContext<'_>,
        event: &ExternalEvent,
    ) -> Result<PlannedStep, KernelFault> {
        match event {
            ExternalEvent::DeliverSignal(delivery) => self.plan_signal(context, delivery),
            ExternalEvent::ChildCompleted(completed) => {
                self.plan_child_completed(context, completed)
            }
        }
    }

    /// §7.7 · one signal delivery.
    ///
    /// Everything decidable is decided before the router is touched, so a refused delivery is a
    /// genuine zero-mutation rejection:
    ///
    /// * a delivery needs a root to interrupt — a signal to an operation that has not started has
    ///   no attention to compete for;
    /// * `attempt` is 1-based: attempt 0 is a host that did not count its own redelivery, and the
    ///   delivery/attempt pair is the only thing that makes "one signal delivered three times"
    ///   distinguishable from "three signals" (§14.1);
    /// * a task target must name a task this kernel issued and that is still live. `SignalTarget`
    ///   is the kernel's own address space — a host session, process or thread is not a target and
    ///   has no representation on the wire (§5.2).
    ///
    /// Admission itself (TTL sweep, dedupe, deadline escalation, queue displacement) belongs to the
    /// router and runs on the **envelope's accepted time**, already fed in `plan_inner`. The
    /// signal's own `source_timestamp_ms` is audit metadata and never a clock (§11.2).
    ///
    /// The disposition is a *fact*: queueing, dropping, expiring and displacing produce
    /// observations and no effect. The one host action a signal can cause is preempting running
    /// children, and that is published as a `PreemptTasks` effect through the ordinary
    /// action-to-disposition path — committed only when its resolution comes back (§7.8).
    fn plan_signal(
        &mut self,
        context: &PlanContext<'_>,
        delivery: &DeliverSignal,
    ) -> Result<PlannedStep, KernelFault> {
        let root_kind = self.require_root_kind()?;
        if delivery.attempt == 0 {
            return Err(KernelFault::new(
                KernelFaultCode::MalformedEnvelope,
                format!(
                    "delivery {} carries attempt 0; attempts are 1-based, and a delivery that \
                     cannot say which attempt it is cannot be told apart from a redelivery (§7.7)",
                    delivery.delivery_id
                ),
            ));
        }
        if let SignalTarget::Task(target) = &delivery.signal.target {
            let live = self
                .engine
                .as_ref()
                .and_then(|engine| engine.task_lifecycle(target.task_id.as_str()))
                .is_some_and(|lifecycle| !lifecycle.is_terminal());
            if !live {
                return Err(KernelFault::new(
                    KernelFaultCode::InvalidAuthority,
                    format!(
                        "signal {} targets task {}, which this operation has no live attempt for; \
                         a signal addresses the operation or one of its own logical tasks (§7.7)",
                        delivery.signal.signal_id, target.task_id
                    ),
                ));
            }
        }
        // DEC-8 · the one effect a delivery can publish is the preemption of running children, and
        // that is reachable only for a critical signal while this kernel holds live attempts.
        // Checked here, before the router moves, so a host that cannot stop its children refuses
        // the delivery instead of planning an effect it could never execute.
        //
        // The urgency read is the **effective** one: a signal carrying `escalate_after_ms: 0`
        // becomes critical at admission when the operation enabled deadline escalation, so reading
        // the bare wire value would let it reach the router and only then discover the host has no
        // preemption path. A deadline that comes due *later* escalates inside the queue, and that
        // path is covered by the effect-minting `require_effect_support` — a fault rather than a
        // zero-mutation refusal, which is the correct asymmetry: at admission nothing has moved.
        let escalation_enabled = context.config.signal_policy.deadline_escalation;
        if delivery.signal.effective_urgency(escalation_enabled) == SignalUrgency::Critical
            && !self.attempts.is_empty()
        {
            self.require_effect_support(context.config, EffectKindTag::PreemptTasks)?;
        }

        // ----- past this line the semantic engine advances -----
        // DEC-3 · at most one pending effect per kind, so a signal may only force a fresh provider
        // request when this operation is not already waiting on one.
        let may_issue_request = self.provider_calls.is_empty();
        let signal = runtime_signal(&delivery.signal, context.input.observed_at_ms);
        let engine = self.engine_mut()?;
        let action = engine.signal_event(
            context.input.operation_id.as_str().to_string(),
            delivery.delivery_id.as_str().to_string(),
            delivery.attempt,
            signal,
            may_issue_request,
        );
        engine
            .task_table_mut()
            .notify(&WaitKey::Signal(SignalFilter(
                delivery.signal.signal_id.as_str().into(),
            )));
        match action {
            Some(action) => self.continue_after(context, action, root_kind),
            // Queued / observed / ignored / dropped: the disposition observation is the whole of
            // what happened, and the host is asked for nothing.
            None => Ok(self.quiet_step()),
        }
    }

    /// §7.7 · a child completion is the event that drains a workflow DAG.
    fn plan_child_completed(
        &mut self,
        context: &PlanContext<'_>,
        completed: &ChildCompleted,
    ) -> Result<PlannedStep, KernelFault> {
        let root_kind = self.require_root_kind()?;
        match self.attempts.get(completed.task_id.as_str()) {
            Some(minted) if minted == &completed.attempt_id => {}
            issued => {
                return Err(KernelFault::new(
                    KernelFaultCode::InvalidAuthority,
                    format!(
                        "task {} has attempt {} in this kernel, but the completion names {}; a \
                         host does not mint or rewrite child identity (§10.4)",
                        completed.task_id,
                        issued.map_or("none", AttemptId::as_str),
                        completed.attempt_id,
                    ),
                ));
            }
        }

        let mut effective_completion = completed.clone();
        if completed.result.status == ChildStatus::Failed {
            let attempt = attempt_ordinal(&completed.attempt_id).ok_or_else(|| {
                KernelFault::new(
                    KernelFaultCode::InvalidAuthority,
                    format!("attempt {} has no canonical ordinal", completed.attempt_id),
                )
            })?;
            let reason = completed
                .result
                .error
                .clone()
                .unwrap_or_else(|| "child attempt failed".to_string());
            let (strategy, max_restarts, relaunches) = {
                let engine = self.engine_mut()?;
                let task = engine
                    .task_table()
                    .get(completed.task_id.as_str())
                    .ok_or_else(|| {
                        KernelFault::new(
                            KernelFaultCode::InvalidAuthority,
                            format!("unknown completed task {}", completed.task_id),
                        )
                    })?;
                let parent = task.parent.as_ref().and_then(|id| {
                    engine.task_table().get(id.as_str()).map(|parent| {
                        (
                            parent.supervision.child_failure,
                            parent.supervision.max_restarts,
                        )
                    })
                });
                let (strategy, max_restarts) = parent.unwrap_or_default();
                let relaunches = task
                    .supervision_events
                    .iter()
                    .filter(|event| event.relaunched)
                    .count() as u32;
                (strategy, max_restarts, relaunches)
            };
            // Relaunch is opt-in *and bounded*: a restart/retry policy without an explicit maximum
            // records the failure but cannot generate an unbounded host-effect loop.
            let relaunch = matches!(
                strategy,
                crate::scheduler::tcb::ChildFailurePolicy::Restart
                    | crate::scheduler::tcb::ChildFailurePolicy::Retry
            ) && max_restarts.is_some_and(|max| relaunches < max);

            if relaunch {
                self.require_effect_support(context.config, EffectKindTag::SpawnTasks)?;
                let info = self
                    .engine
                    .as_ref()
                    .and_then(|engine| {
                        engine.workflow_spawn_info_for_agent(completed.task_id.as_str())
                    })
                    .ok_or_else(|| {
                        KernelFault::new(
                            KernelFaultCode::InvalidLifecycle,
                            format!(
                                "task {} has no active workflow launch descriptor to relaunch",
                                completed.task_id
                            ),
                        )
                    })?;
                let next_attempt = attempt.checked_add(1).ok_or_else(|| {
                    KernelFault::new(
                        KernelFaultCode::InvalidLifecycle,
                        "child attempt ordinal overflow".to_string(),
                    )
                })?;
                let launch = self.task_launch_attempt(
                    &context.input.operation_id,
                    context.step_seq,
                    &info,
                    next_attempt,
                )?;
                let event = crate::scheduler::tcb::SupervisionEvent {
                    attempt,
                    strategy,
                    reason: reason.clone().into(),
                    terminal: true,
                    relaunched: true,
                };
                let engine = self.engine_mut()?;
                engine
                    .task_table_mut()
                    .get_mut(completed.task_id.as_str())
                    .expect("validated task exists")
                    .supervision_events
                    .push(event);
                engine
                    .task_table_mut()
                    .prepare_supervised_relaunch(completed.task_id.as_str(), strategy);
                engine.mark_tasks_starting(&[completed.task_id.as_str().to_string()]);
                engine
                    .observations
                    .push(KernelObservation::ChildSupervised {
                        turn: engine.turn,
                        task_id: completed.task_id.as_str().to_string(),
                        attempt,
                        strategy: supervision_label(strategy).to_string(),
                        reason,
                        terminal: true,
                        relaunched: true,
                    });
                return Ok(PlannedStep {
                    root_kind: self.root_kind,
                    focus: self.focus.clone(),
                    observations: Vec::new(),
                    disposition: StepDisposition::Effects(EffectsDisposition {
                        effects: vec![KernelEffect {
                            effect_id: mint_effect_id(
                                &context.input.operation_id,
                                context.step_seq,
                                0,
                            ),
                            causation_input_id: context.input.input_id.clone(),
                            effect: EffectKind::SpawnTasks(SpawnTasksEffect {
                                tasks: vec![launch],
                                budget: None,
                            }),
                        }],
                    }),
                });
            }

            let event = crate::scheduler::tcb::SupervisionEvent {
                attempt,
                strategy,
                reason: reason.clone().into(),
                terminal: true,
                relaunched: false,
            };
            let engine = self.engine_mut()?;
            engine
                .task_table_mut()
                .get_mut(completed.task_id.as_str())
                .expect("validated task exists")
                .supervision_events
                .push(event);
            engine
                .observations
                .push(KernelObservation::ChildSupervised {
                    turn: engine.turn,
                    task_id: completed.task_id.as_str().to_string(),
                    attempt,
                    strategy: supervision_label(strategy).to_string(),
                    reason,
                    terminal: true,
                    relaunched: false,
                });
            if strategy == crate::scheduler::tcb::ChildFailurePolicy::Ignore {
                effective_completion.result.status = ChildStatus::Completed;
                effective_completion.result.error = None;
            }
        }

        // ----- past this line the semantic engine advances -----
        self.engine_mut()?
            .task_table_mut()
            .notify(&WaitKey::Child(completed.task_id.as_str().into()));
        // §10.4 · the attempt is spent. A second completion naming it — and any `parent_requests`
        // riding on that second completion — is a stale causation, refused by the check above.
        self.attempts.remove(completed.task_id.as_str());

        // §7.7 · the requests enter P1 first, with `ChildAttempt` causation, and the completion is
        // fed afterwards so its own drive produces the single next ready batch.
        //
        // GAP-4: this loop can neither fail the transition nor skip the completion. Each request is
        // adjudicated on its own; a refused one leaves a structured rejection observation and
        // changes neither its siblings nor the fact that the child ran.
        //
        // No arm can move the focus here, which is why only `effects` is collected: the sole
        // focus-moving syscall is `SubmitWorkflow`'s *bootstrap*, and a child attempt only exists
        // while its DAG is in flight — so that request always takes the flatten arm instead.
        let mut index = 0u32;
        let mut effects = Vec::new();
        for (seq, request) in completed.parent_requests.iter().enumerate() {
            let causation = SyscallCausation::ChildAttempt(ChildAttemptCausation {
                task_id: completed.task_id.clone(),
                attempt_id: completed.attempt_id.clone(),
                request_seq: seq as u32,
            });
            match self.apply_syscall(context, &causation, request, &mut index) {
                Ok(outcome) => effects.extend(outcome.effects),
                Err(SyscallRefusal::Fault(fault)) => self.note_rejection(
                    SyscallRejection::new(
                        "parent_request",
                        format!("request {seq} refused: {}", fault.message),
                    )
                    .by(&completed.task_id),
                ),
                Err(SyscallRefusal::Rejected(rejection)) => {
                    self.note_rejection(rejection.by(&completed.task_id))
                }
            }
        }

        // `feed` clears the observation buffer at the head of every event, and these facts happened
        // *before* it. Carrying them across is what keeps the audit trail of a refused parent
        // request from being erased by the very completion it rode in on.
        let syscall_observations = self
            .engine_mut()?
            .take_observations()
            .into_iter()
            .collect::<Vec<_>>();

        let result = sub_agent_result(&effective_completion);
        let engine = self.engine_mut()?;
        let action = engine.feed(LoopEvent::SubAgentCompleted { result });
        let mut step = self.continue_after_at(context, action, root_kind, &mut index)?;
        if let Some(engine) = self.engine.as_mut() {
            engine.observations.splice(0..0, syscall_observations);
        }
        if !effects.is_empty() {
            match &mut step.disposition {
                StepDisposition::Effects(published) => {
                    let mut merged = effects;
                    merged.append(&mut published.effects);
                    published.effects = merged;
                }
                StepDisposition::Terminal(_) => {
                    return Err(KernelFault::new(
                        KernelFaultCode::InvalidLifecycle,
                        "a completion that terminates the operation cannot also publish the \
                         effects its parent requests asked for (§7.12)"
                            .to_string(),
                    ));
                }
            }
        }
        Ok(step)
    }

    // ----- §7.5 · the live control plane -----

    /// §7.5 · reduce one host command onto the mechanism that already owns that state.
    ///
    /// Every arm here carries **host authority**, which is what separates this from the P1 syscall
    /// path in [`Self::apply_syscall`]: a host command is not gated, not quarantine-coerced and not
    /// attributed to a caller, because the host *is* the authority. `UpdateTask` is the pair that
    /// makes the split visible — the same `TaskUpdate` payload, two input classes, two authorities,
    /// preserving the authority distinction that the retired shared task-update input could not
    /// express (§7.5 现状注记).
    ///
    /// Refusals are faults rather than model-facing rejections: a control command is a host bug, so
    /// it is answered to the host and never becomes a note the model reads.
    fn plan_host_control(
        &mut self,
        context: &PlanContext<'_>,
        command: &HostCommand,
    ) -> Result<PlannedStep, KernelFault> {
        match command {
            HostCommand::Cancel(cancel) => self.plan_cancel(context, cancel),
            HostCommand::ForceCompact(_) => {
                let root_kind = self.require_root_kind()?;
                self.engine_mut()?.force_compact();
                // A compaction that archived history owes a `page_out` effect; `continue_after`
                // externalises it through the same one path an in-turn compaction takes.
                self.continue_after(context, LoopAction::AwaitingResume, root_kind)
            }
            HostCommand::UpdateTask(update) => self.plan_host_task_update(update),
            HostCommand::ApplyCapabilityPatch(patch) => self.plan_capability_patch(patch),
            HostCommand::ApplyKnowledgeMutation(mutation) => self.plan_knowledge_mutation(mutation),
            HostCommand::SeedKnowledge(seed) => self.plan_seed_knowledge(seed),
            HostCommand::ApplySkillActivation(activation) => self.plan_skill_activation(activation),
            HostCommand::ApplyPolicyPatch(patch) => self.plan_policy_patch(patch),
            HostCommand::UpdateDeadline(deadline) => self.plan_update_deadline(deadline),
        }
    }

    /// §11.1 · the cancellation ladder, and the only path an operation is cancelled through.
    ///
    /// The order is downstream-first and it happens **inside one transition**, because §7.12 admits
    /// effects or a terminal and never both:
    ///
    /// 1. every child attempt this kernel issued is settled and its wait torn down, every pending
    ///    workflow batch and deferred host effect dropped (`cancel_operation`);
    /// 2. the driver's own ledger of live attempts and pending calls is spent with them, so a late
    ///    completion or resolution naming one is a stale causation rather than a resurrection;
    /// 3. only then is the root terminal minted.
    ///
    /// The *real* I/O stop is the host's: §11.1 has the host stop provider, tool and child I/O
    /// before it submits this command, and the kernel adjudicates the terminal. That is why this
    /// step publishes no `PreemptTasks` effect — asking the host to stop what it already stopped
    /// would need a second transition, and the operation would not be cancelled until it came back.
    ///
    /// The reason is the host's, not the loop's: `cancel_operation` terminates the semantic loop
    /// with its own internal `UserAbort`, but a `Deadline` or `HostShutdown` cancellation must not
    /// be reported as a user abort, so the terminal is built from the command.
    fn plan_cancel(
        &mut self,
        context: &PlanContext<'_>,
        cancel: &CancelCommand,
    ) -> Result<PlannedStep, KernelFault> {
        let root_kind = self.root_kind;
        let focus = self.focus.clone();
        let reason = cancel.reason;
        // §7.5 · the cancel command carries no operation id of its own; the envelope owns it.
        let operation_id = context.input.operation_id.as_str().to_string();
        let engine = self.engine_mut()?;
        let action = engine.cancel_operation(
            operation_id,
            reason,
            cancel
                .pending_call_ids
                .iter()
                .map(|call_id| call_id.as_str().to_string())
                .collect(),
        );
        // Downstream identity is spent in the same transition that settles the tasks it names.
        self.attempts.clear();
        self.provider_calls.clear();
        self.pending_memory_writes.clear();
        self.pending_memory_queries.clear();

        let LoopAction::Done { result } = action else {
            return Err(KernelFault::new(
                KernelFaultCode::InvalidLifecycle,
                format!(
                    "cancelling the operation produced {} instead of a terminal; cancellation is \
                     the one control command that always ends the operation (§11.1)",
                    loop_action_label(&action)
                ),
            ));
        };
        Ok(PlannedStep {
            root_kind,
            focus,
            observations: Vec::new(),
            disposition: StepDisposition::Terminal(TerminalDisposition {
                terminal: KernelTerminal::Cancelled(CancelledTerminal {
                    reason,
                    usage: UsageReport {
                        input_tokens: WireU64::new(result.total_tokens_used),
                        output_tokens: WireU64::ZERO,
                        turns: result.turns_used,
                        cached_input_tokens: None,
                    },
                }),
            }),
        })
    }

    /// §7.5 · the host's own plan edit. Same payload as the model's `update_task` syscall, applied
    /// through the same context mechanism — and deliberately *not* through the P1 gate, because the
    /// authority is the host's rather than a derived caller's.
    fn plan_host_task_update(
        &mut self,
        update: &UpdateTaskCommand,
    ) -> Result<PlannedStep, KernelFault> {
        let engine = self.engine_mut()?;
        engine.ctx.update_task(core_task_update(&update.update));
        Ok(self.quiet_step())
    }

    /// §13.2 · mount/unmount in one command so a swap is atomic. Unmounting something absent errs
    /// open (it is already not mounted), which is the only shape that makes a retry safe.
    fn plan_capability_patch(
        &mut self,
        patch: &ApplyCapabilityPatchCommand,
    ) -> Result<PlannedStep, KernelFault> {
        let engine = self.engine_mut()?;
        for grant in &patch.patch.mount {
            engine.mount_capability(
                crate::types::capability::CapabilityDescriptor {
                    id: grant.id.as_str().into(),
                    kind: core_capability_kind(grant.kind),
                    description: grant.description.clone().unwrap_or_default(),
                    tool_schema: None,
                    skill: None,
                    metadata: serde_json::Value::Null,
                    lease: None,
                    is_pinned: false,
                    version: None,
                    mounted_by: None,
                    mount_reason: None,
                },
                None,
                None,
            );
        }
        for reference in &patch.patch.unmount {
            engine.unmount_capability(core_capability_kind(reference.kind), &reference.id);
        }
        Ok(self.quiet_step())
    }

    /// §13.2 · keyed knowledge upsert + removal. Both directions are boundary-deferred by the
    /// partition itself, so the model never sees system bytes change mid-turn.
    fn plan_knowledge_mutation(
        &mut self,
        mutation: &ApplyKnowledgeMutationCommand,
    ) -> Result<PlannedStep, KernelFault> {
        let engine = self.engine_mut()?;
        seed_knowledge(engine, &mutation.mutation.upsert);
        for key in &mutation.mutation.remove {
            engine.ctx.remove_knowledge(key);
        }
        Ok(self.quiet_step())
    }

    /// DEC-9 · the host seeding the knowledge partition. Same mechanism as the initial context's
    /// `knowledge`, and named apart from the P1 `PageIn { handle_id }` on purpose: the two are
    /// opposite directions and must never share a name again (§7.5).
    fn plan_seed_knowledge(
        &mut self,
        seed: &SeedKnowledgeCommand,
    ) -> Result<PlannedStep, KernelFault> {
        let engine = self.engine_mut()?;
        seed_knowledge(engine, &seed.entries);
        Ok(self.quiet_step())
    }

    /// §13.2 · skill activation state, validated whole before anything moves.
    ///
    /// A name outside the operation's declared catalog is refused rather than invented — the same
    /// rule the model's `ActivateSkill` syscall obeys — and because the command is atomic, one bad
    /// name refuses the whole swap instead of leaving half of it applied.
    fn plan_skill_activation(
        &mut self,
        activation: &ApplySkillActivationCommand,
    ) -> Result<PlannedStep, KernelFault> {
        let engine = self.engine_mut()?;
        for activate in &activation.activate {
            if !engine.ctx.skill_available(&activate.name) {
                return Err(KernelFault::new(
                    KernelFaultCode::InvalidConfig,
                    format!(
                        "this operation declares no skill named {:?}; activating one is a \
                         capability mutation and is refused rather than invented (§13.2)",
                        activate.name
                    ),
                ));
            }
            ensure_skill_grants_are_attenuated(
                engine.ctx.skill_capability_grants(&activate.name),
                engine.root_capabilities(),
            )
            .map_err(|violations| {
                KernelFault::new(
                    KernelFaultCode::InvalidAuthority,
                    skill_grant_attenuation_message(&activate.name, &violations),
                )
            })?;
        }
        let turn = engine.turn;
        for activate in &activation.activate {
            let expires_at_turn = activate.lease_turns.map(|turns| turn.saturating_add(turns));
            engine
                .ctx
                .activate_skill_leased(activate.name.as_str(), expires_at_turn);
        }
        for name in &activation.deactivate {
            engine.ctx.deactivate_skill(name);
        }
        Ok(self.quiet_step())
    }

    /// §13.2 / DEC-6 · one revision-guarded policy patch.
    ///
    /// [`LivePolicyState::apply`] is all-or-nothing: a stale revision, a widened quota or a policy
    /// that fails its boot validator leaves both the configuration and the revision exactly as they
    /// were, so a refused patch is a zero-mutation rejection and the writer rebases instead of
    /// silently overwriting whoever won the race. Only after it succeeds are the changed policies
    /// re-installed into the running engine, through the same installers the genesis build uses.
    fn plan_policy_patch(
        &mut self,
        patch: &ApplyPolicyPatchCommand,
    ) -> Result<PlannedStep, KernelFault> {
        let Some(policy) = self.policy.as_mut() else {
            return Err(KernelFault::new(
                KernelFaultCode::InvalidLifecycle,
                "the operation has no genesis configuration, so it has no policy to patch"
                    .to_string(),
            ));
        };
        let revision = policy.apply(patch).map_err(|rejection| {
            KernelFault::new(KernelFaultCode::InvalidConfig, rejection.message)
        })?;
        let config = policy.config().clone();
        let engine = self.engine_mut()?;
        install_live_policies(engine, &config);
        let turn = engine.turn;
        engine
            .observations
            .push(KernelObservation::LivePolicyChanged {
                turn,
                policy: live_policy_label(&patch.patch).to_string(),
                revision: revision.get(),
            });
        Ok(self.quiet_step())
    }

    /// §13.2 · the operation's absolute deadline, projected onto the wall-time budget axis the
    /// scheduler already owns.
    ///
    /// The axis is a duration measured from the operation's first accepted time, so an absolute
    /// deadline becomes `deadline − start`. A deadline already in the past is not an error: it
    /// yields a zero-length budget, and the next scheduling decision terminates on `Deadline` —
    /// the same verdict the axis would have reached on its own a moment later.
    fn plan_update_deadline(
        &mut self,
        deadline: &UpdateDeadlineCommand,
    ) -> Result<PlannedStep, KernelFault> {
        let engine = self.engine_mut()?;
        let started_at_ms = engine.started_at_ms();
        let budget = match (deadline.deadline_ms, started_at_ms) {
            (None, _) => None,
            (Some(deadline_ms), Some(started_at_ms)) => {
                Some(deadline_ms.get().saturating_sub(started_at_ms))
            }
            (Some(_), None) => {
                return Err(KernelFault::new(
                    KernelFaultCode::InvalidLifecycle,
                    "this operation has accepted no timed input yet, so an absolute deadline has \
                     no start to measure from (§11.2)"
                        .to_string(),
                ));
            }
        };
        engine.set_wall_budget(budget);
        Ok(self.quiet_step())
    }

    /// The shared tail of every non-start transition: turn the engine's action into a disposition
    /// and re-derive the focus from what the engine actually did.
    fn continue_after(
        &mut self,
        context: &PlanContext<'_>,
        action: LoopAction,
        root_kind: RootKind,
    ) -> Result<PlannedStep, KernelFault> {
        let mut index = 0;
        self.continue_after_at(context, action, root_kind, &mut index)
    }

    fn continue_after_at(
        &mut self,
        context: &PlanContext<'_>,
        action: LoopAction,
        root_kind: RootKind,
        effect_index: &mut u32,
    ) -> Result<PlannedStep, KernelFault> {
        let workflow_finished = self
            .engine()
            .map(|engine| {
                engine
                    .observations
                    .iter()
                    .any(|o| matches!(o, KernelObservation::WorkflowCompleted { .. }))
            })
            .unwrap_or(false);
        let disposition = self.disposition_for_at(context, action, root_kind, effect_index)?;
        let focus = if workflow_finished {
            // §6.1.7 / §7.4: a nested workflow's completion restores the parent agent turn; a root
            // workflow's completion commits the terminal instead and its focus stops moving.
            match (root_kind, &self.focus) {
                (RootKind::Agent, Some(ExecutionFocus::WorkflowController(controller))) => {
                    controller
                        .parent_task_id
                        .clone()
                        .map(ExecutionFocus::agent_turn)
                }
                (_, current) => current.clone(),
            }
        } else {
            self.focus.clone()
        };
        Ok(PlannedStep {
            root_kind: Some(root_kind),
            focus,
            observations: Vec::new(),
            disposition,
        })
    }

    /// Project one [`LoopAction`] onto §7.12's closed step disposition.
    fn disposition_for(
        &mut self,
        context: &PlanContext<'_>,
        action: LoopAction,
        root_kind: RootKind,
    ) -> Result<StepDisposition, KernelFault> {
        let mut index = 0;
        self.disposition_for_at(context, action, root_kind, &mut index)
    }

    /// The same projection, minting effect ids from a caller-owned counter. A step that reduces a
    /// syscall batch publishes several effects, and each one's identity has to stay unique.
    fn disposition_for_at(
        &mut self,
        context: &PlanContext<'_>,
        action: LoopAction,
        root_kind: RootKind,
        effect_index: &mut u32,
    ) -> Result<StepDisposition, KernelFault> {
        let operation_id = &context.input.operation_id;
        let causation = context.input.input_id.clone();
        let step_seq = context.step_seq;

        // A durability effect produced *inside* the transition (a compaction's page-out) is
        // published first and holds the continuation. The
        // guard inside makes a second call in the same step a no-op, so a step never activates two.
        let mut action = self.engine_mut()?.externalize_pending_host_effect(action);
        // DEC-8 · an archive the host never declared it can perform is not a reason to refuse the
        // transition that produced it: the compaction already happened in-kernel and the archive is
        // best effort. It is abandoned through the same one-decision path a host failure takes, so
        // the audit fact is identical whether the host said "I cannot" or "I could not".
        while matches!(action, LoopAction::ArchivePageOut { .. })
            && !context
                .config
                .host_effect_support
                .supports(EffectKindTag::ArchivePageOut)
        {
            action = self.engine_mut()?.abandon_page_out_archive(
                "this operation's host declares no archive_page_out support".to_string(),
            );
        }

        let effect = match action {
            LoopAction::AwaitingResume => {
                // A root workflow that just drained its DAG terminates here, and publishes nothing.
                if root_kind == RootKind::Workflow
                    && let Some(terminal) = self.root_workflow_terminal()
                {
                    return Ok(StepDisposition::Terminal(TerminalDisposition { terminal }));
                }
                return Ok(StepDisposition::Effects(EffectsDisposition::default()));
            }
            LoopAction::Done { result } => {
                return Ok(StepDisposition::Terminal(TerminalDisposition {
                    terminal: agent_terminal(&result),
                }));
            }
            LoopAction::CallLLM {
                context: rendered,
                tools,
            } => EffectKind::CallProvider(CallProviderEffect {
                context: rendered_context(&rendered),
                tools: tools.iter().map(tool_schema).collect(),
            }),
            LoopAction::ExecuteTools { calls } => EffectKind::ExecuteTools(ExecuteToolsEffect {
                calls: calls.iter().map(wire_tool_call).collect::<Result<_, _>>()?,
            }),
            LoopAction::RequestApproval { requests } => {
                EffectKind::RequestApproval(RequestApprovalEffect {
                    requests: requests
                        .iter()
                        .map(wire_approval_request)
                        .collect::<Result<_, _>>()?,
                })
            }
            LoopAction::SpawnWorkflow { nodes, budget } => {
                let mut tasks = Vec::with_capacity(nodes.len());
                for node in &nodes {
                    tasks.push(self.task_launch(operation_id, step_seq, node)?);
                }
                EffectKind::SpawnTasks(SpawnTasksEffect {
                    tasks,
                    budget: budget.as_ref().map(workflow_budget),
                })
            }
            LoopAction::PreemptSubAgents { agent_ids, reason } => {
                // §10.4 · a preemption names the attempt this kernel issued. A task with no live
                // attempt has nothing to preempt, so it is dropped rather than named with a
                // fabricated one.
                let attempts = agent_ids
                    .iter()
                    .filter_map(|agent_id| {
                        let attempt_id = self.attempts.get(agent_id)?.clone();
                        let task_id = TaskId::new(agent_id).ok()?;
                        Some(TaskAttemptRef {
                            task_id,
                            attempt_id,
                        })
                    })
                    .collect();
                EffectKind::PreemptTasks(PreemptTasksEffect { attempts, reason })
            }
            // The engine's action carries the legacy internal trio — criteria, required evidence
            // and the verifier — and the canonical projection consumes none of them. All three
            // are host-owned by decision (§5.2, adjudication §5m-3/§5p): the host looks the
            // verifier and its criteria up from `(contract_id, phase_id)`, which is why the
            // request carries that pair and nothing else. The internal fields stay for the legacy
            // path until Task 23 deletes it.
            LoopAction::EvaluateMilestone {
                phase_id,
                criteria: _,
                required_evidence: _,
                verifier: _,
            } => EffectKind::EvaluateMilestone(EvaluateMilestoneEffect {
                request: super::effect::MilestoneRequest {
                    contract_id: self.require_loaded_contract()?,
                    phase_id,
                },
            }),
            LoopAction::ArchivePageOut {
                summary, archived, ..
            } => EffectKind::ArchivePageOut(self.page_out_effect(
                context,
                summary.as_deref(),
                &archived,
                *effect_index,
            )?),
            // The engine never emits these two: they exist only for the legacy `WriteMemory` /
            // `QueryMemory` inputs, which the canonical wire replaced with a P1 syscall that mints
            // the effect directly (see `plan_memory_write` / `plan_memory_query`).
            action @ (LoopAction::PersistMemory { .. } | LoopAction::QueryMemory { .. }) => {
                return Err(KernelFault::new(
                    KernelFaultCode::InvalidLifecycle,
                    format!(
                        "the semantic kernel emitted {}, which only the deleted legacy memory \
                         inputs can produce; on the canonical wire a memory effect is minted by \
                         the P1 syscall that proposed it",
                        loop_action_label(&action)
                    ),
                ));
            }
        };

        let tag = effect.tag();
        self.require_effect_support(context.config, tag)?;
        let effect_id = mint_effect_id(operation_id, step_seq, *effect_index);
        *effect_index += 1;

        match &effect {
            // §7.6 · remember what this turn advertised and whose turn it was. That pair *is* the
            // `ProviderTool` causation a syscall in the result will be attributed to.
            EffectKind::CallProvider(call) => {
                self.provider_calls.insert(
                    effect_id.clone(),
                    PendingProviderCall {
                        task_id: self.turn_task_id(),
                        exposed_tools: call.tools.iter().map(|tool| tool.name.clone()).collect(),
                    },
                );
            }
            // §10.4 · the launch is published, so every task it names leaves `PendingLaunch`. Only
            // the host's acknowledgement moves them on to `Running`.
            EffectKind::SpawnTasks(spawn) => {
                let launched: Vec<String> = spawn
                    .tasks
                    .iter()
                    .map(|task| task.task_id.as_str().to_string())
                    .collect();
                if let Some(engine) = self.engine.as_mut() {
                    engine.mark_tasks_starting(&launched);
                }
            }
            _ => {}
        }

        Ok(StepDisposition::Effects(EffectsDisposition {
            effects: vec![KernelEffect {
                effect_id,
                causation_input_id: causation,
                effect,
            }],
        }))
    }

    /// The task whose turn is currently issuing provider calls. An agent focus names it directly;
    /// a workflow controller's provider call is the parent agent's turn resuming, and before the
    /// first focus is folded there is only the root.
    fn turn_task_id(&self) -> TaskId {
        let staged = self
            .staged
            .as_ref()
            .and_then(|staged| staged.focus.as_ref());
        match staged.or(self.focus.as_ref()) {
            Some(ExecutionFocus::AgentTurn(turn)) => turn.task_id.clone(),
            Some(ExecutionFocus::WorkflowController(controller)) => controller
                .parent_task_id
                .clone()
                .unwrap_or_else(root_task_id),
            None => root_task_id(),
        }
    }

    /// Build the workflow terminal from the outcome the engine just published. Reads the
    /// observation rather than re-deriving it: the DAG's own `finish()` is the authority on which
    /// nodes completed.
    fn root_workflow_terminal(&mut self) -> Option<KernelTerminal> {
        let workflow_id = self.workflow_id.clone()?;
        let engine = self.engine.as_mut()?;
        let outcomes = engine
            .observations
            .iter()
            .find_map(|observation| match observation {
                KernelObservation::WorkflowCompleted { node_outcomes, .. } => {
                    Some(node_outcomes.clone())
                }
                _ => None,
            })?;
        let mut completed = Vec::new();
        let mut failed = Vec::new();
        for outcome in &outcomes {
            let node_id = self.node_id_for(&outcome.node_id);
            match outcome.status {
                WorkflowNodeStatus::Completed | WorkflowNodeStatus::CompletedPartial => {
                    completed.push(node_id)
                }
                WorkflowNodeStatus::Failed | WorkflowNodeStatus::SkippedUpstreamFailed => {
                    failed.push(node_id)
                }
            }
        }
        let status = if failed.is_empty() {
            WorkflowStatus::Completed
        } else {
            WorkflowStatus::Failed
        };
        Some(KernelTerminal::Workflow(WorkflowTerminal {
            outcome: WorkflowOutcome {
                workflow_id,
                status,
                completed_nodes: completed,
                failed_nodes: failed,
            },
            usage: self.usage_report(),
        }))
    }

    fn usage_report(&self) -> UsageReport {
        let Some(engine) = self.engine.as_ref() else {
            return UsageReport::default();
        };
        let (tokens, _, _) = engine.local_budget_usage();
        UsageReport {
            input_tokens: WireU64::new(tokens),
            output_tokens: WireU64::ZERO,
            turns: engine.turn,
            cached_input_tokens: None,
        }
    }

    /// One child launch, with kernel-minted identity. The task/attempt/launch triple exists as a
    /// committed fact before the host is ever asked to start anything (§10.4).
    fn task_launch(
        &mut self,
        operation_id: &OperationId,
        step_seq: WireU64,
        info: &crate::orchestration::workflow::WorkflowSpawnInfo,
    ) -> Result<TaskLaunch, KernelFault> {
        self.task_launch_attempt(operation_id, step_seq, info, 1)
    }

    fn task_launch_attempt(
        &mut self,
        operation_id: &OperationId,
        step_seq: WireU64,
        info: &crate::orchestration::workflow::WorkflowSpawnInfo,
        attempt: u32,
    ) -> Result<TaskLaunch, KernelFault> {
        let task_id = TaskId::new(&info.agent_id).map_err(malformed)?;
        let attempt_id =
            AttemptId::new(format!("{}:attempt:{attempt}", info.agent_id)).map_err(malformed)?;
        let launch_token = LaunchToken::new(if attempt == 1 {
            format!("{operation_id}:step:{step_seq}:launch:{}", info.agent_id)
        } else {
            format!(
                "{operation_id}:step:{step_seq}:launch:{}:attempt:{attempt}",
                info.agent_id
            )
        })
        .map_err(malformed)?;
        self.attempts
            .insert(info.agent_id.clone(), attempt_id.clone());
        let mut metadata = serde_json::Map::new();
        if let Some(model_hint) = &info.model_hint {
            metadata.insert(
                "model_hint".to_string(),
                serde_json::Value::String(model_hint.clone()),
            );
        }
        if let Some(output_schema) = &info.output_schema {
            metadata.insert("output_schema".to_string(), output_schema.clone());
        }
        if !info.input_agent_ids.is_empty() {
            metadata.insert(
                "input_agent_ids".to_string(),
                serde_json::Value::Array(
                    info.input_agent_ids
                        .iter()
                        .cloned()
                        .map(serde_json::Value::String)
                        .collect(),
                ),
            );
            let dependency_outputs = info
                .input_agent_ids
                .iter()
                .filter_map(|agent_id| {
                    let output = self
                        .engine
                        .as_ref()?
                        .task_table()
                        .get(agent_id)?
                        .proc
                        .as_ref()?
                        .result
                        .as_ref()?
                        .result
                        .final_message
                        .as_ref()
                        .and_then(message_body_parts)?
                        .0;
                    Some((agent_id.clone(), serde_json::Value::String(output)))
                })
                .collect();
            metadata.insert(
                "dependency_outputs".to_string(),
                serde_json::Value::Object(dependency_outputs),
            );
        }
        Ok(TaskLaunch {
            task_id,
            attempt_id,
            launch_token,
            node_id: self.node_id_for(&info.agent_id),
            spec: LogicalAgentSpec {
                goal: info.goal.clone(),
                role: parse_wire_role(&info.role),
                isolation: parse_wire_isolation(&info.isolation),
                context_inheritance: parse_wire_context_inheritance(&info.context_inheritance),
                verification_contract_id: None,
                capability_filter: Default::default(),
                exposure_baseline: None,
                loop_round: None,
                metadata: super::scalar::BoundedJson::new(serde_json::Value::Object(metadata))
                    .map_err(malformed)?,
            },
        })
    }

    /// Wire identity of the DAG node an internal agent id belongs to. Falls back to the internal id
    /// when the node came from a runtime append rather than the original spec (Task 10 territory).
    fn node_id_for(&self, agent_id: &str) -> NodeId {
        parse_node_index(agent_id)
            .and_then(|index| self.node_ids.get(index).cloned())
            .unwrap_or_else(|| {
                NodeId::new(agent_id).expect("an internal agent id is a legal branded ref")
            })
    }

    /// §7.3 · a spec's `verification_contract_id` must name a contract this operation declared.
    ///
    /// A reference that resolves to nothing is a gate the run believes it has and does not: the
    /// agent would start with no phase cascade, never publish an `EvaluateMilestone`, and finish
    /// having passed a contract that was never evaluated. Refused before the engine moves, so a
    /// rejected start leaves the operation free to start again with a spec that resolves.
    fn require_known_contract(
        &self,
        config: &ResolvedOperationConfig,
        spec: Option<&LogicalAgentSpec>,
    ) -> Result<(), KernelFault> {
        let Some(contract_id) = spec.and_then(|spec| spec.verification_contract_id.as_deref())
        else {
            return Ok(());
        };
        if config.verification_contract(contract_id).is_some() {
            return Ok(());
        }
        Err(KernelFault::new(
            KernelFaultCode::InvalidConfig,
            format!(
                "the run spec names verification contract {contract_id:?}, which this operation's \
                 catalog does not declare; a contract reference that resolves to nothing is a \
                 milestone gate the run believes it has (§7.3)"
            ),
        ))
    }

    /// Install the phase cascade an agent's contract reference selects.
    ///
    /// This is `EvaluateMilestone`'s canonical producer (Task 12 SPEC-ISSUE-4): without it the
    /// effect existed in the union, had a resolution path and a failure path, and nothing on the
    /// wire could ever cause the kernel to emit one.
    fn load_verification_contract(
        &mut self,
        config: &ResolvedOperationConfig,
        spec: Option<&LogicalAgentSpec>,
    ) -> Result<(), KernelFault> {
        let Some(contract) = spec
            .and_then(|spec| spec.verification_contract_id.as_deref())
            .and_then(|id| config.verification_contract(id))
        else {
            return Ok(());
        };
        // A contract means the operation *will* ask for a verdict, so the effect it will publish
        // has to be declared now rather than faulting mid-cascade (DEC-8).
        self.require_effect_support(config, EffectKindTag::EvaluateMilestone)?;
        let contract_id = contract.contract_id.clone();
        let cascade = core_milestone_contract(contract, config);
        self.engine_mut()?.load_milestone_contract(cascade);
        // Remembered so every `EvaluateMilestone` the cascade produces can name the contract its
        // phase belongs to — the half of the host's lookup key the semantic engine does not carry.
        self.loaded_contract_id = Some(contract_id);
        Ok(())
    }

    /// The contract id every `EvaluateMilestone` this operation publishes belongs to.
    ///
    /// Fails closed rather than sending an empty id: a cascade can only be running because
    /// `load_verification_contract` installed one, so a milestone request with no contract behind
    /// it means the engine produced a phase the canonical wire never declared — and a request the
    /// host cannot resolve to a verifier is worse than no request at all.
    fn require_loaded_contract(&self) -> Result<String, KernelFault> {
        self.loaded_contract_id.clone().ok_or_else(|| {
            KernelFault::new(
                KernelFaultCode::InvalidLifecycle,
                "the semantic kernel asked for a milestone verdict, but this operation installed \
                 no verification contract; a milestone request names the (contract_id, phase_id) \
                 pair the host looks its verifier up by (§7.8)"
                    .to_string(),
            )
        })
    }

    fn require_effect_support(
        &self,
        config: &ResolvedOperationConfig,
        tag: EffectKindTag,
    ) -> Result<(), KernelFault> {
        if config.host_effect_support.supports(tag) {
            return Ok(());
        }
        Err(KernelFault::new(
            KernelFaultCode::UnsupportedEffect,
            format!(
                "this operation's host does not declare support for {tag} effects, so the \
                 transition that would publish one is refused before anything moves"
            ),
        ))
    }

    fn require_root_kind(&self) -> Result<RootKind, KernelFault> {
        self.root_kind.ok_or_else(|| {
            KernelFault::new(
                KernelFaultCode::InvalidLifecycle,
                "no root has started, so there is nothing to advance".to_string(),
            )
        })
    }

    fn engine_mut(&mut self) -> Result<&mut LoopStateMachine, KernelFault> {
        self.engine.as_mut().ok_or_else(|| {
            KernelFault::new(
                KernelFaultCode::InvalidLifecycle,
                "the operation has no genesis configuration, so it has no semantic kernel to drive"
                    .to_string(),
            )
        })
    }

    fn poison_with(&mut self, fault: KernelFault) -> KernelFault {
        self.staged = None;
        self.poison.get_or_insert(fault).clone()
    }
}

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
/// SPEC-ISSUE: `SyscallRequest::RequestMemoryWrite` has no entry here because core advertises no
/// model-facing memory *write* surface — `memory` is a search tool, and long-term writes are
/// extracted host-side today (§22.13's 现状定位). Its only caller channel is therefore a child's
/// `parent_requests`. §7.6 lists the request without saying which tool reaches it, so either the
/// kernel's meta-tool set gains a write surface or the spec should state that memory writes are a
/// child→parent request only.
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

/// §7.4 · the logical spec carries no host session identity, so the internal identity it builds
/// carries none either. (Task 11 removes the field from `AgentIdentity` outright.)
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
            allowed_kinds: Vec::new(),
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

fn live_policy_label(patch: &super::command::LivePolicyPatch) -> &'static str {
    use super::command::LivePolicyPatch;
    match patch {
        LivePolicyPatch::ReplaceSignalPolicy(_) => "signal",
        LivePolicyPatch::ReplaceGovernancePolicy(_) => "governance",
        LivePolicyPatch::TightenResourceQuota(_) => "resource_quota",
        LivePolicyPatch::ReplaceRecoveryPolicy(_) => "recovery",
    }
}

fn logical_message(message: &super::root::LogicalMessage) -> Message {
    Message {
        role: core_role_of(message.role),
        content: Content::Text(message.content.clone()),
        tool_calls: Vec::new(),
        token_count: message.tokens,
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

fn rendered_context(context: &crate::context::renderer::RenderedContext) -> WireRenderedContext {
    WireRenderedContext {
        system_stable: context.system_stable.clone(),
        system_knowledge: context.system_knowledge.clone(),
        turns: context.turns.iter().map(provider_message).collect(),
        state_turn: context.state_turn.as_ref().map(provider_message),
        frozen_prefix_len: context.frozen_prefix_len.map(|len| len as u32),
    }
}

fn provider_message(message: &Message) -> ProviderMessage {
    let (content, tool_call_id) = message_body_parts(message)
        .map(|(text, tool_call_id, _is_error)| (text, tool_call_id))
        .unwrap_or_default();
    ProviderMessage {
        role: wire_role_of(message.role),
        content,
        tool_calls: message
            .tool_calls
            .iter()
            .filter_map(|call| wire_tool_call(call).ok())
            .collect(),
        tool_call_id: tool_call_id.and_then(|call_id| super::scalar::CallId::new(call_id).ok()),
        tokens: message.token_count,
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
                .map(|text| Message::assistant(text.clone())),
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
                .and_then(|usage| usage.output_tokens)
                .map_or(0, WireU64::get),
            loop_continue: None,
            classify_branch: None,
            pace_decision: None,
            tournament_winner: None,
        },
    }
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
fn core_provider_message(message: &ProviderMessage) -> Result<Message, KernelFault> {
    Ok(Message {
        role: core_role_of(message.role),
        content: Content::Text(message.content.clone()),
        tool_calls: message.tool_calls.iter().map(core_tool_call).collect(),
        token_count: message.tokens,
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
            token_count: inline.result.tokens,
        },
        WireToolResultPayload::External(external) => ToolResult {
            call_id: external.call_id.as_str().into(),
            output: Content::Text(external.preview.clone()),
            durable_content: None,
            is_error,
            is_fatal: disposition.is_fatal(),
            error_kind,
            token_count: None,
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
        version: crate::scheduler::policy::SCHEDULER_POLICY_VERSION,
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
    engine.set_dispatch_gate_exposed(matches!(
        config.feature_policy.tool_dispatch_gate,
        super::config::ToolDispatchGate::Exposed
    ));
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
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use serde_json::{Value, json};

    use super::*;
    use crate::runtime::kernel::wire::checkpoint::{
        CanonicalInput, CheckpointCandidate, CheckpointDraft, KernelCheckpoint,
    };
    use crate::runtime::kernel::wire::config::ConfigDefaults;
    use crate::runtime::kernel::wire::config::TailBounds;
    use crate::runtime::kernel::wire::config::{
        EntropyWatchPolicy, ExecutionPolicy, HostEffectSupport,
        MilestonePhase as WireMilestonePhase, OperationConfig,
        VerificationContract as WireVerificationContract,
    };
    use crate::runtime::kernel::wire::effect::{
        EffectSucceeded, TaskLaunchOutcome, TaskLaunchStarted, TaskLaunchStatus,
        TasksSpawnedSuccess,
    };
    use crate::runtime::kernel::wire::envelope::{
        ConfigureOperation, DeliverExternalEvent, KernelInput, ResolveEffect, StartOperation,
        WireEnvelope,
    };
    use crate::runtime::kernel::wire::event::{ChildResult, DeliverSignal, LogicalSignal};
    use crate::runtime::kernel::wire::fault::PrepareToken;
    use crate::runtime::kernel::wire::record::{
        KernelRecord, RecordPreparation, canonical_bytes, canonical_digest, verify_record_chain,
    };
    use crate::runtime::kernel::wire::restore::{
        RestoreCost, RestoredOperation, restore_operation,
    };
    use crate::runtime::kernel::wire::root::{
        RootAgentEntry, RootWorkflowEntry, WorkflowNode as WireNode,
    };
    use crate::runtime::kernel::wire::scalar::{
        AttemptId as WireAttemptId, DeliveryId, InputId, Ppm, SignalId,
    };
    use crate::runtime::kernel::wire::transaction::{
        CheckpointBoundary, CommittedTransition, InMemoryRecordIndex, KernelTransaction,
        TailPressure,
    };
    use crate::scheduler::tcb::TaskLifecycle;

    const OPERATION: &str = "op-driver-1";

    // -----------------------------------------------------------------------------------------
    // §8.2 host loop: prepare → (CAS append) → commit → fold
    // -----------------------------------------------------------------------------------------

    /// The whole durable path, exactly as a host runs it. Nothing here reaches into the driver
    /// behind the transaction's back: a step is planned by the driver, appended by the "host"
    /// (this in-memory journal), committed by the transaction, and only then folded into the
    /// driver's root-kind/focus state.
    struct Runtime {
        tx: KernelTransaction<PlannedStep, InMemoryRecordIndex>,
        driver: CanonicalOperationDriver,
        journal: Vec<KernelRecord>,
        last_observations: Vec<KernelObservation>,
        /// What the restore that produced this runtime read, when it came from one.
        restore_cost: Option<RestoreCost>,
    }

    impl Runtime {
        fn new() -> Self {
            Self {
                tx: KernelTransaction::new(ConfigDefaults::default(), InMemoryRecordIndex::new()),
                driver: CanonicalOperationDriver::new(),
                journal: Vec::new(),
                last_observations: Vec::new(),
                restore_cost: None,
            }
        }

        fn prepare(&mut self, envelope: &WireEnvelope) -> RecordPreparation<PlannedStep> {
            let Self { tx, driver, .. } = self;
            tx.prepare(envelope, |context| driver.plan(context))
        }

        fn append_and_commit(
            &mut self,
            preparation: RecordPreparation<PlannedStep>,
        ) -> CommittedTransition<PlannedStep> {
            let token: PrepareToken = preparation
                .token()
                .unwrap_or_else(|| {
                    panic!("expected a prepared step, got {:?}", preparation.fault())
                })
                .clone();
            let head = preparation.record().unwrap().record_digest().clone();
            let committed = self.tx.commit(&token, &head).expect("commit must succeed");
            self.journal.push(committed.record.clone());
            self.last_observations = committed.step.observations.clone();
            committed
        }

        fn submit(&mut self, envelope: &WireEnvelope) -> CommittedTransition<PlannedStep> {
            let preparation = self.prepare(envelope);
            let committed = self.append_and_commit(preparation);
            self.driver
                .note_committed(committed.step_seq)
                .expect("the driver folds the step it planned");
            committed
        }

        /// Same durable path, but the step comes from a planner the *test* supplies. Used for the
        /// two transitions Task 9 deliberately leaves to Task 10/12 — the P1 syscall that starts a
        /// nested workflow, and the provider resolution that frees the pending provider effect.
        fn submit_planned<F>(
            &mut self,
            envelope: &WireEnvelope,
            plan: F,
        ) -> CommittedTransition<PlannedStep>
        where
            F: FnOnce(
                &mut CanonicalOperationDriver,
                &PlanContext<'_>,
            ) -> Result<PlannedStep, KernelFault>,
        {
            let preparation = {
                let Self { tx, driver, .. } = self;
                tx.prepare(envelope, |context| plan(driver, context))
            };
            self.append_and_commit(preparation)
        }

        fn reject(&mut self, envelope: &WireEnvelope) -> KernelFault {
            let preparation = self.prepare(envelope);
            preparation
                .fault()
                .unwrap_or_else(|| panic!("expected a rejection, got a prepared step"))
                .clone()
        }

        /// §12.2 · the host's own restore call: a checkpoint blob plus the journal records above the
        /// step it covers. Nothing else — in particular, **not** the records the checkpoint covers,
        /// which is what makes the cost assertions meaningful.
        fn restore(&self, checkpoint: &KernelCheckpoint) -> Runtime {
            let after: Vec<KernelRecord> = self
                .journal
                .iter()
                .filter(|record| record.step_seq().get() > checkpoint.through_step_seq().get())
                .cloned()
                .collect();
            Self::restore_with(Some(checkpoint), &after)
        }

        fn restore_with(
            checkpoint: Option<&KernelCheckpoint>,
            records: &[KernelRecord],
        ) -> Runtime {
            let RestoredOperation {
                transaction,
                driver,
                cost,
            } = restore_operation(
                checkpoint,
                records,
                ConfigDefaults::default(),
                InMemoryRecordIndex::from_records(records),
            )
            .expect("the restore ladder runs to completion");
            Runtime {
                tx: transaction,
                driver,
                journal: Vec::new(),
                last_observations: Vec::new(),
                restore_cost: Some(cost),
            }
        }

        /// The whole journal, as the host holds it.
        fn journal_from(&self, checkpoint: &KernelCheckpoint) -> Vec<KernelRecord> {
            self.journal
                .iter()
                .filter(|record| record.step_seq().get() > checkpoint.through_step_seq().get())
                .cloned()
                .collect()
        }

        fn pending_effect_kinds(&self) -> Vec<EffectKindTag> {
            self.tx.pending_effects().map(|e| e.tag()).collect()
        }

        fn observations(&self) -> &[KernelObservation] {
            &self.last_observations
        }

        /// §12.3 · exactly the host's call: project the driver's three partitions, hand them to
        /// the transaction, get a candidate. Nothing here reaches around either layer.
        fn checkpoint(&self) -> CheckpointCandidate {
            self.tx
                .checkpoint_candidate(self.driver.project_logical_state())
                .expect("a configured operation has a logical state to checkpoint")
        }
    }

    // -----------------------------------------------------------------------------------------
    // envelopes
    // -----------------------------------------------------------------------------------------

    fn operation() -> OperationId {
        OperationId::new(OPERATION).unwrap()
    }

    fn envelope(id: &str, observed_at_ms: u64, input: KernelInput) -> WireEnvelope {
        WireEnvelope::new(
            operation(),
            InputId::new(id).unwrap(),
            WireU64::new(observed_at_ms),
            input,
        )
    }

    fn boot_config(supported: impl IntoIterator<Item = EffectKindTag>) -> OperationConfig {
        OperationConfig {
            execution_policy: Some(ExecutionPolicy {
                max_turns: Some(12),
                ..ExecutionPolicy::default()
            }),
            host_effect_support: HostEffectSupport::new(supported),
            ..OperationConfig::default()
        }
    }

    fn configure() -> WireEnvelope {
        configure_supporting([EffectKindTag::CallProvider, EffectKindTag::SpawnTasks])
    }

    fn configure_supporting(supported: impl IntoIterator<Item = EffectKindTag>) -> WireEnvelope {
        envelope(
            "in-configure",
            1_700_000_000_000,
            KernelInput::ConfigureOperation(ConfigureOperation {
                config: boot_config(supported),
            }),
        )
    }

    fn agent_start(id: &str, observed_at_ms: u64) -> WireEnvelope {
        envelope(
            id,
            observed_at_ms,
            KernelInput::StartOperation(StartOperation {
                entry: RootEntry::Agent(RootAgentEntry {
                    task: LogicalTask::new("write the research brief"),
                    run_spec: None,
                }),
                initial_context: InitialContext::default(),
            }),
        )
    }

    fn agent_start_with_capabilities(
        id: &str,
        observed_at_ms: u64,
        requested_capabilities: Vec<crate::types::capability::Capability>,
    ) -> WireEnvelope {
        envelope(
            id,
            observed_at_ms,
            KernelInput::StartOperation(StartOperation {
                entry: RootEntry::Agent(RootAgentEntry {
                    task: LogicalTask::new("write the research brief"),
                    run_spec: None,
                }),
                initial_context: InitialContext {
                    requested_capabilities,
                    ..InitialContext::default()
                },
            }),
        )
    }

    fn wire_node(node_id: &str, goal: &str, depends_on: &[&str]) -> WireNode {
        WireNode {
            node_id: NodeId::new(node_id).unwrap(),
            task: LogicalTask::new(goal),
            depends_on: depends_on
                .iter()
                .map(|id| NodeId::new(*id).unwrap())
                .collect(),
            run_spec: None,
        }
    }

    fn two_node_spec() -> WireSpec {
        WireSpec {
            name: "brief".to_string(),
            nodes: vec![
                wire_node("collect", "collect the sources", &[]),
                wire_node("write", "write the brief", &["collect"]),
            ],
        }
    }

    fn workflow_start(id: &str, observed_at_ms: u64, spec: WireSpec) -> WireEnvelope {
        envelope(
            id,
            observed_at_ms,
            KernelInput::StartOperation(StartOperation {
                entry: RootEntry::Workflow(RootWorkflowEntry { spec }),
                initial_context: InitialContext::default(),
            }),
        )
    }

    fn spawned(
        id: &str,
        observed_at_ms: u64,
        effect_id: &EffectId,
        tasks: &[&str],
    ) -> WireEnvelope {
        envelope(
            id,
            observed_at_ms,
            KernelInput::ResolveEffect(ResolveEffect {
                effect_id: effect_id.clone(),
                outcome: EffectOutcome::Succeeded(EffectSucceeded {
                    result: EffectSuccess::TasksSpawned(TasksSpawnedSuccess {
                        attempts: tasks
                            .iter()
                            .map(|task| TaskLaunchOutcome {
                                task_id: TaskId::new(*task).unwrap(),
                                attempt_id: WireAttemptId::new(format!("{task}:attempt:1"))
                                    .unwrap(),
                                outcome: TaskLaunchStatus::Started(TaskLaunchStarted {}),
                            })
                            .collect(),
                    }),
                }),
            }),
        )
    }

    fn spawned_attempt(
        id: &str,
        observed_at_ms: u64,
        effect_id: &EffectId,
        task: &str,
        attempt: u32,
    ) -> WireEnvelope {
        envelope(
            id,
            observed_at_ms,
            KernelInput::ResolveEffect(ResolveEffect {
                effect_id: effect_id.clone(),
                outcome: EffectOutcome::Succeeded(EffectSucceeded {
                    result: EffectSuccess::TasksSpawned(TasksSpawnedSuccess {
                        attempts: vec![TaskLaunchOutcome {
                            task_id: TaskId::new(task).unwrap(),
                            attempt_id: WireAttemptId::new(format!("{task}:attempt:{attempt}"))
                                .unwrap(),
                            outcome: TaskLaunchStatus::Started(TaskLaunchStarted {}),
                        }],
                    }),
                }),
            }),
        )
    }

    fn child_done(id: &str, observed_at_ms: u64, task: &str, output: &str) -> WireEnvelope {
        envelope(
            id,
            observed_at_ms,
            KernelInput::DeliverExternalEvent(DeliverExternalEvent {
                event: ExternalEvent::ChildCompleted(ChildCompleted {
                    task_id: TaskId::new(task).unwrap(),
                    attempt_id: WireAttemptId::new(format!("{task}:attempt:1")).unwrap(),
                    result: ChildResult {
                        status: ChildStatus::Completed,
                        output: Some(output.to_string()),
                        ..ChildResult::default()
                    },
                    parent_requests: Vec::new(),
                }),
            }),
        )
    }

    /// Drives [`CanonicalOperationDriver::begin_nested_workflow`] as a direct API call, on an
    /// envelope that carries no request of its own.
    ///
    /// The real P1 carrier is now the provider resolution (see `provider_result` and the §7.6
    /// tests); these Task 9 tests keep using the direct entry point because what they assert is the
    /// focus/authority arc, which both paths share — `plan_provider_syscalls` reduces
    /// `SubmitWorkflow` onto exactly this code.
    fn syscall_carrier(id: &str, observed_at_ms: u64) -> WireEnvelope {
        envelope(
            id,
            observed_at_ms,
            KernelInput::DeliverExternalEvent(DeliverExternalEvent {
                event: ExternalEvent::DeliverSignal(DeliverSignal {
                    delivery_id: DeliveryId::new(format!("delivery-{id}")).unwrap(),
                    attempt: 1,
                    signal: LogicalSignal::new(SignalId::new(format!("sig-{id}")).unwrap()),
                }),
            }),
        )
    }

    fn effect_id(step_seq: WireU64) -> EffectId {
        EffectId::new(format!("{OPERATION}:step:{step_seq}:effect:0")).unwrap()
    }

    fn effect_id_at(step_seq: WireU64, index: u32) -> EffectId {
        EffectId::new(format!("{OPERATION}:step:{step_seq}:effect:{index}")).unwrap()
    }

    // -----------------------------------------------------------------------------------------
    // §7.6 · syscall envelopes
    // -----------------------------------------------------------------------------------------

    fn syscall_tool_catalog() -> Vec<WireToolSchema> {
        SYSCALL_TOOL_NAMES
            .iter()
            .chain(std::iter::once(&"search"))
            .map(|name| WireToolSchema {
                name: (*name).to_string(),
                description: String::new(),
                parameters: Default::default(),
            })
            .collect()
    }

    /// A configuration whose operation can actually reach every P1 syscall: the meta-tool surface
    /// is in the catalog, memory is bound read+write, one skill is declared, and the host supports
    /// the effects the memory syscalls publish.
    fn syscall_config() -> WireEnvelope {
        syscall_config_with(|_| {})
    }

    /// The same configuration with one edit applied — how the effect-resolution tests declare the
    /// extra host support (approval, page-out, milestone) or a governance policy their arc needs.
    fn syscall_config_with(edit: impl FnOnce(&mut OperationConfig)) -> WireEnvelope {
        use crate::runtime::kernel::wire::config::{
            MemoryPolicy, ResourceQuota, SkillMetadata as WireSkill,
        };
        use crate::runtime::kernel::wire::effect::{MemoryAccessBinding, MemoryCapabilities};
        use crate::runtime::kernel::wire::scalar::MemoryBindingId;

        let mut config = {
            OperationConfig {
                execution_policy: Some(ExecutionPolicy {
                    max_turns: Some(12),
                    ..ExecutionPolicy::default()
                }),
                host_effect_support: HostEffectSupport::new([
                    EffectKindTag::CallProvider,
                    EffectKindTag::ExecuteTools,
                    EffectKindTag::LoadPayload,
                    EffectKindTag::SpawnTasks,
                    EffectKindTag::PreemptTasks,
                    EffectKindTag::PersistMemory,
                    EffectKindTag::QueryMemory,
                ]),
                tool_catalog: syscall_tool_catalog(),
                skill_catalog: vec![WireSkill {
                    name: "debug".to_string(),
                    description: "debug helper".to_string(),
                    when_to_use: None,
                    allowed_tools: Vec::new(),
                    capability_grants: Vec::new(),
                    effort: None,
                    estimated_tokens: None,
                }],
                memory_access: Some(MemoryAccessBinding {
                    binding_id: MemoryBindingId::new("mem-binding-1").unwrap(),
                    capabilities: MemoryCapabilities {
                        read: true,
                        write: true,
                    },
                }),
                memory_policy: Some(MemoryPolicy {
                    retrieval_top_k: Some(4),
                    ..MemoryPolicy::default()
                }),
                resource_quota: Some(ResourceQuota {
                    max_workflow_nodes: Some(3),
                    ..ResourceQuota::default()
                }),
                ..OperationConfig::default()
            }
        };
        edit(&mut config);
        envelope(
            "in-configure",
            1_700_000_000_000,
            KernelInput::ConfigureOperation(ConfigureOperation { config }),
        )
    }

    fn tool_call(call_id: &str, name: &str, arguments: Value) -> WireToolCall {
        WireToolCall {
            call_id: super::super::scalar::CallId::new(call_id).unwrap(),
            name: name.to_string(),
            arguments: super::super::scalar::BoundedJson::new(arguments).unwrap(),
        }
    }

    /// A provider result carrying tool calls — the only wire shape from which a `ProviderTool`
    /// causation can be derived.
    fn provider_result(
        id: &str,
        observed_at_ms: u64,
        effect: &EffectId,
        calls: Vec<WireToolCall>,
    ) -> WireEnvelope {
        envelope(
            id,
            observed_at_ms,
            KernelInput::ResolveEffect(ResolveEffect {
                effect_id: effect.clone(),
                outcome: EffectOutcome::Succeeded(EffectSucceeded {
                    result: EffectSuccess::Provider(super::super::effect::ProviderSuccess {
                        outcome: super::super::effect::ProviderOutcome::Completed(
                            super::super::effect::ProviderCompleted {
                                message: ProviderMessage {
                                    role: MessageRole::Assistant,
                                    content: String::new(),
                                    tool_calls: calls,
                                    tool_call_id: None,
                                    tokens: None,
                                },
                                observed_input_tokens: None,
                                observed_output_tokens: None,
                                stop_reason: None,
                            },
                        ),
                    }),
                }),
            }),
        )
    }

    fn child_done_with(
        id: &str,
        observed_at_ms: u64,
        task: &str,
        attempt: &str,
        requests: Vec<SyscallRequest>,
    ) -> WireEnvelope {
        envelope(
            id,
            observed_at_ms,
            KernelInput::DeliverExternalEvent(DeliverExternalEvent {
                event: ExternalEvent::ChildCompleted(ChildCompleted {
                    task_id: TaskId::new(task).unwrap(),
                    attempt_id: WireAttemptId::new(attempt).unwrap(),
                    result: ChildResult {
                        status: ChildStatus::Completed,
                        output: Some("done".to_string()),
                        ..ChildResult::default()
                    },
                    parent_requests: requests,
                }),
            }),
        )
    }

    fn child_failed(
        id: &str,
        observed_at_ms: u64,
        task: &str,
        attempt: u32,
        reason: &str,
    ) -> WireEnvelope {
        envelope(
            id,
            observed_at_ms,
            KernelInput::DeliverExternalEvent(DeliverExternalEvent {
                event: ExternalEvent::ChildCompleted(ChildCompleted {
                    task_id: TaskId::new(task).unwrap(),
                    attempt_id: WireAttemptId::new(format!("{task}:attempt:{attempt}")).unwrap(),
                    result: ChildResult {
                        status: ChildStatus::Failed,
                        error: Some(reason.to_string()),
                        ..ChildResult::default()
                    },
                    parent_requests: Vec::new(),
                }),
            }),
        )
    }

    fn node_args(nodes: &[WireNode]) -> Value {
        json!({ "nodes": serde_json::to_value(nodes).unwrap() })
    }

    /// `(operation, subject, reason)` of every structured rejection the last transition recorded.
    fn rejections(runtime: &Runtime) -> Vec<(String, Option<String>, String)> {
        runtime
            .observations()
            .iter()
            .filter_map(|observation| match observation {
                KernelObservation::ControlRequestRejected {
                    operation,
                    subject,
                    reason,
                    ..
                } => Some((operation.clone(), subject.clone(), reason.clone())),
                _ => None,
            })
            .collect()
    }

    fn sole_effect(committed: &CommittedTransition<PlannedStep>) -> &KernelEffect {
        let effects = committed.published_effects();
        assert_eq!(effects.len(), 1, "expected exactly one published effect");
        &effects[0]
    }

    // -----------------------------------------------------------------------------------------
    // fixture: agent-configure-is-genesis
    // -----------------------------------------------------------------------------------------

    #[test]
    fn the_first_accepted_input_must_be_the_configuration() {
        let mut runtime = Runtime::new();

        // a business input before any configuration
        let fault = runtime.reject(&agent_start("in-early-start", 1_700_000_000_100));
        assert_eq!(fault.code, KernelFaultCode::InvalidLifecycle);
        assert_eq!(runtime.tx.head(), None, "a rejection moves nothing");
        assert_eq!(runtime.tx.lifecycle(), OperationLifecycle::Created);
        assert!(
            runtime.driver.engine().is_none(),
            "no semantic kernel exists"
        );

        let genesis = runtime.submit(&configure());
        assert_eq!(genesis.step_seq, WireU64::ZERO);
        assert_eq!(
            genesis.record.previous_record_digest(),
            None,
            "the genesis record has no predecessor (§8.1)"
        );
        assert_eq!(runtime.tx.lifecycle(), OperationLifecycle::Configured);
        assert!(runtime.driver.engine().is_some());
        assert_eq!(
            runtime.driver.root_kind(),
            None,
            "configuring is not starting"
        );

        // the genesis record stores the *resolved* configuration, not the sparse input
        let stored = genesis.record.normalized_input().unwrap();
        let resolved = stored
            .resolved_config()
            .expect("genesis carries the config");
        assert_eq!(resolved.execution_policy.max_turns, 12);
        assert_eq!(
            resolved.execution_policy.max_context_tokens,
            ConfigDefaults::default()
                .baseline
                .execution_policy
                .max_context_tokens,
            "every default this operation runs on is frozen in its first record"
        );
    }

    // -----------------------------------------------------------------------------------------
    // fixture: agent-root-start-is-atomic + agent-start-issues-model-turn
    // -----------------------------------------------------------------------------------------

    #[test]
    fn an_agent_root_start_is_one_atomic_input_that_issues_the_model_turn() {
        let mut runtime = Runtime::new();
        runtime.submit(&configure());
        let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));

        // root kind and initial context are fixed by this one accepted input
        assert_eq!(started.step.root_kind, Some(RootKind::Agent));
        assert_eq!(runtime.driver.root_kind(), Some(RootKind::Agent));
        assert_eq!(
            runtime.driver.focus(),
            Some(&ExecutionFocus::agent_turn(root_task_id())),
        );
        assert_eq!(runtime.tx.lifecycle(), OperationLifecycle::Running);

        // exactly one pending effect, and it is the provider call
        let effect = sole_effect(&started);
        assert_eq!(effect.tag(), EffectKindTag::CallProvider);
        assert_eq!(
            effect.effect_id,
            effect_id(started.step_seq),
            "the effect id is minted by the kernel from the step it belongs to"
        );
        assert_eq!(
            effect.causation_input_id.as_str(),
            "in-start",
            "every effect names the accepted input that produced it"
        );
        assert_eq!(
            runtime.pending_effect_kinds(),
            vec![EffectKindTag::CallProvider]
        );

        // the rendered context is real: the goal reached the P3 context VM
        let EffectKind::CallProvider(call) = &effect.effect else {
            panic!("expected a provider call");
        };
        let rendered = format!(
            "{}{}",
            call.context.system_stable, call.context.system_knowledge
        );
        let state_turn = call
            .context
            .state_turn
            .as_ref()
            .map(|turn| turn.content.clone())
            .unwrap_or_default();
        assert!(
            rendered.contains("research brief") || state_turn.contains("research brief"),
            "the root task's goal must be in the rendered context, not merely stored"
        );
        assert!(
            !call.context.turns.is_empty(),
            "the start seeded a first turn"
        );
    }

    #[test]
    fn a_second_root_start_is_refused_with_zero_mutation() {
        let mut runtime = Runtime::new();
        runtime.submit(&configure());
        let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));

        let before = (
            runtime.tx.head(),
            runtime.tx.lifecycle(),
            runtime.pending_effect_kinds(),
            runtime.driver.root_kind(),
            runtime.driver.focus().cloned(),
        );

        let fault = runtime.reject(&agent_start("in-start-again", 1_700_000_002_000));
        assert_eq!(fault.code, KernelFaultCode::InvalidLifecycle);

        let after = (
            runtime.tx.head(),
            runtime.tx.lifecycle(),
            runtime.pending_effect_kinds(),
            runtime.driver.root_kind(),
            runtime.driver.focus().cloned(),
        );
        assert_eq!(before, after, "a refused root start moves nothing");
        assert_eq!(runtime.journal.len(), 2, "no third record exists");
        assert_eq!(started.step.root_kind, Some(RootKind::Agent));
    }

    #[test]
    fn a_workflow_root_start_after_an_agent_root_cannot_re_root_the_operation() {
        let mut runtime = Runtime::new();
        runtime.submit(&configure());
        runtime.submit(&agent_start("in-start", 1_700_000_001_000));

        let fault = runtime.reject(&workflow_start(
            "in-reroot",
            1_700_000_002_000,
            two_node_spec(),
        ));
        assert_eq!(fault.code, KernelFaultCode::InvalidLifecycle);
        assert_eq!(
            runtime.driver.root_kind(),
            Some(RootKind::Agent),
            "the root kind is immutable for the operation's lifetime (§6.1.5)"
        );
    }

    /// DEC-8 fail-closed, at the root start rather than at the first emission. `call_provider`
    /// support is already mandatory at configuration time (every operation reaches a provider
    /// call), so the reachable half of the rule is a workflow root on a host that cannot spawn.
    #[test]
    fn a_workflow_root_is_refused_when_the_host_cannot_spawn_tasks() {
        let mut runtime = Runtime::new();
        runtime.submit(&configure_supporting([EffectKindTag::CallProvider]));
        let fault = runtime.reject(&workflow_start(
            "in-start",
            1_700_000_001_000,
            two_node_spec(),
        ));
        assert_eq!(fault.code, KernelFaultCode::UnsupportedEffect);
        assert_eq!(runtime.driver.root_kind(), None);
        assert_eq!(runtime.tx.lifecycle(), OperationLifecycle::Configured);
        assert_eq!(runtime.journal.len(), 1, "only the genesis record exists");
    }

    // -----------------------------------------------------------------------------------------
    // fixture: workflow-root-entry-is-atomic
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_workflow_root_start_spawns_tasks_and_never_calls_the_provider() {
        let mut runtime = Runtime::new();
        runtime.submit(&configure());
        let started = runtime.submit(&workflow_start(
            "in-start",
            1_700_000_001_000,
            two_node_spec(),
        ));

        assert_eq!(started.step.root_kind, Some(RootKind::Workflow));
        let effect = sole_effect(&started);
        assert_eq!(
            effect.tag(),
            EffectKindTag::SpawnTasks,
            "a workflow root's first effect is a task spawn (§10.1)"
        );
        assert_eq!(
            runtime.pending_effect_kinds(),
            vec![EffectKindTag::SpawnTasks],
            "no provider effect is published, so none can be overwritten by a workflow load"
        );

        // the DAG's identity is kernel-minted and its first ready node is the only launch
        let EffectKind::SpawnTasks(spawn) = &effect.effect else {
            panic!("expected a task spawn");
        };
        assert_eq!(spawn.tasks.len(), 1, "only `collect` is ready");
        assert_eq!(spawn.tasks[0].node_id.as_str(), "collect");
        assert_eq!(spawn.tasks[0].task_id.as_str(), "wf-node0");
        assert!(
            !spawn.tasks[0].launch_token.as_str().is_empty(),
            "the launch token exists as a committed fact before the host launches anything"
        );

        // focus is the root controller, with no parent to restore
        assert_eq!(
            runtime.driver.focus(),
            Some(&ExecutionFocus::workflow_controller(
                runtime.driver.workflow_id().unwrap().clone(),
                None
            ))
        );
        assert!(
            !runtime.driver.focus().unwrap().is_nested_in_agent(),
            "the root workflow is not nested in an agent"
        );
    }

    #[test]
    fn a_workflow_launch_preserves_logical_context_inheritance() {
        let mut spec = two_node_spec();
        spec.nodes[0].run_spec = Some(LogicalAgentSpec {
            context_inheritance: Some(WireContextInheritance::SystemOnly),
            ..LogicalAgentSpec::new("collect the sources")
        });

        let mut runtime = Runtime::new();
        runtime.submit(&configure());
        let started = runtime.submit(&workflow_start("in-start", 1_700_000_001_000, spec));

        let EffectKind::SpawnTasks(spawn) = &sole_effect(&started).effect else {
            panic!("expected a task spawn");
        };
        assert_eq!(
            spawn.tasks[0].spec.context_inheritance,
            Some(WireContextInheritance::SystemOnly),
        );
    }

    #[test]
    fn a_workflow_root_with_no_nodes_has_no_first_effect_and_is_refused() {
        let mut runtime = Runtime::new();
        runtime.submit(&configure());
        let fault = runtime.reject(&workflow_start(
            "in-start",
            1_700_000_001_000,
            WireSpec::default(),
        ));
        assert_eq!(fault.code, KernelFaultCode::InvalidConfig);
        assert_eq!(runtime.driver.root_kind(), None);
    }

    #[test]
    fn a_workflow_spec_whose_dependency_names_no_declared_node_is_refused() {
        let mut runtime = Runtime::new();
        runtime.submit(&configure());
        let spec = WireSpec {
            name: "broken".to_string(),
            nodes: vec![wire_node("write", "write", &["collect"])],
        };
        let fault = runtime.reject(&workflow_start("in-start", 1_700_000_001_000, spec));
        assert_eq!(fault.code, KernelFaultCode::InvalidConfig);
        assert_eq!(runtime.driver.root_kind(), None);
        assert_eq!(runtime.journal.len(), 1, "only the genesis record exists");
    }

    // -----------------------------------------------------------------------------------------
    // fixture: workflow-root-completion-commits-terminal
    // -----------------------------------------------------------------------------------------

    /// The full §10.1 path: configure → root start → spawn → ack → completions → workflow terminal,
    /// with no `LoadWorkflow`, no placeholder agent run and no host-privileged `CompleteRun`.
    fn drive_workflow_root_to_terminal() -> Runtime {
        let mut runtime = Runtime::new();
        runtime.submit(&configure());
        let started = runtime.submit(&workflow_start(
            "in-start",
            1_700_000_001_000,
            two_node_spec(),
        ));

        runtime.submit(&spawned(
            "in-ack-1",
            1_700_000_002_000,
            &effect_id(started.step_seq),
            &["wf-node0"],
        ));
        let advanced = runtime.submit(&child_done(
            "in-done-1",
            1_700_000_003_000,
            "wf-node0",
            "sources collected",
        ));
        let second = sole_effect(&advanced);
        assert_eq!(second.tag(), EffectKindTag::SpawnTasks);
        let EffectKind::SpawnTasks(spawn) = &second.effect else {
            panic!("expected a task spawn");
        };
        assert_eq!(spawn.tasks[0].node_id.as_str(), "write");

        runtime.submit(&spawned(
            "in-ack-2",
            1_700_000_004_000,
            &effect_id(advanced.step_seq),
            &["wf-node1"],
        ));
        runtime.submit(&child_done(
            "in-done-2",
            1_700_000_005_000,
            "wf-node1",
            "brief written",
        ));
        runtime
    }

    #[test]
    fn a_root_workflow_completion_commits_the_workflow_terminal_itself() {
        let runtime = drive_workflow_root_to_terminal();

        let terminal = runtime.tx.terminal().expect("the run terminated");
        let KernelTerminal::Workflow(workflow) = terminal else {
            panic!("a workflow root terminates with a workflow terminal, got {terminal:?}");
        };
        assert_eq!(workflow.outcome.status, WorkflowStatus::Completed);
        assert_eq!(
            workflow
                .outcome
                .completed_nodes
                .iter()
                .map(|node| node.as_str())
                .collect::<Vec<_>>(),
            vec!["collect", "write"],
            "the terminal names the wire node ids the host declared"
        );
        assert!(workflow.outcome.failed_nodes.is_empty());
        assert_eq!(runtime.tx.lifecycle(), OperationLifecycle::Completed);
        assert_eq!(
            runtime.pending_effect_kinds(),
            Vec::<EffectKindTag>::new(),
            "a root workflow issues no provider call after it completes"
        );
    }

    #[test]
    fn spc_019_10_restart_publishes_a_distinct_bounded_attempt() {
        use crate::scheduler::tcb::ChildFailurePolicy;

        let mut runtime = Runtime::new();
        runtime.submit(&syscall_config());
        let started = runtime.submit(&workflow_start(
            "in-start",
            1_700_000_001_000,
            two_node_spec(),
        ));
        runtime.submit(&spawned(
            "in-ack-1",
            1_700_000_002_000,
            &effect_id(started.step_seq),
            &["wf-node0"],
        ));
        let root = runtime
            .driver
            .engine
            .as_mut()
            .unwrap()
            .task_table_mut()
            .get_mut("root")
            .unwrap();
        root.supervision.child_failure = ChildFailurePolicy::Restart;
        root.supervision.max_restarts = Some(1);
        let child = runtime
            .driver
            .engine
            .as_mut()
            .unwrap()
            .task_table_mut()
            .get_mut("wf-node0")
            .unwrap();
        child.budget.turns = 4;
        child.budget.total_tokens = 80;

        let failed = envelope(
            "in-failed-1",
            1_700_000_003_000,
            KernelInput::DeliverExternalEvent(DeliverExternalEvent {
                event: ExternalEvent::ChildCompleted(ChildCompleted {
                    task_id: TaskId::new("wf-node0").unwrap(),
                    attempt_id: WireAttemptId::new("wf-node0:attempt:1").unwrap(),
                    result: ChildResult {
                        status: ChildStatus::Failed,
                        error: Some("worker crashed".to_string()),
                        ..ChildResult::default()
                    },
                    parent_requests: Vec::new(),
                }),
            }),
        );
        let restarted = runtime.submit(&failed);
        let EffectKind::SpawnTasks(spawn) = &sole_effect(&restarted).effect else {
            panic!("restart must publish an explicit spawn effect");
        };
        assert_eq!(spawn.tasks[0].attempt_id.as_str(), "wf-node0:attempt:2");
        let restart_effect = sole_effect(&restarted).effect_id.clone();
        let child = runtime
            .driver
            .engine
            .as_ref()
            .unwrap()
            .task_table()
            .get("wf-node0")
            .unwrap();
        assert_eq!((child.budget.turns, child.budget.total_tokens), (0, 0));
        assert_eq!(
            child.supervision_events[0].reason.as_str(),
            "worker crashed"
        );
        assert!(child.supervision_events[0].terminal);
        assert!(child.supervision_events[0].relaunched);
        assert_eq!(
            runtime
                .driver
                .engine
                .as_ref()
                .unwrap()
                .task_lifecycle("wf-node0"),
            Some(crate::scheduler::tcb::TaskLifecycle::Starting)
        );

        let checkpoint = runtime.checkpoint().decode().expect("verifies");
        let restored = Runtime::restore_with(Some(&checkpoint), &[]);
        assert_eq!(surface(&restored), surface(&runtime));

        runtime.submit(&spawned_attempt(
            "in-ack-2",
            1_700_000_004_000,
            &restart_effect,
            "wf-node0",
            2,
        ));
        let failed_again = envelope(
            "in-failed-2",
            1_700_000_005_000,
            KernelInput::DeliverExternalEvent(DeliverExternalEvent {
                event: ExternalEvent::ChildCompleted(ChildCompleted {
                    task_id: TaskId::new("wf-node0").unwrap(),
                    attempt_id: WireAttemptId::new("wf-node0:attempt:2").unwrap(),
                    result: ChildResult {
                        status: ChildStatus::Failed,
                        error: Some("crashed again".to_string()),
                        ..ChildResult::default()
                    },
                    parent_requests: Vec::new(),
                }),
            }),
        );
        let terminal = runtime.submit(&failed_again);
        assert!(matches!(
            terminal.step.disposition,
            StepDisposition::Terminal(_)
        ));
        let events = &runtime
            .driver
            .engine
            .as_ref()
            .unwrap()
            .task_table()
            .get("wf-node0")
            .unwrap()
            .supervision_events;
        assert_eq!(events.len(), 2);
        assert!(!events[1].relaunched, "the explicit limit stops attempt 3");
    }

    #[test]
    fn spc_019_10_retry_preserves_usage_while_ignore_accepts_the_terminal_attempt() {
        use crate::scheduler::tcb::ChildFailurePolicy;

        let mut retry = Runtime::new();
        retry.submit(&syscall_config());
        let started = retry.submit(&workflow_start(
            "retry-start",
            1_700_000_001_000,
            two_node_spec(),
        ));
        retry.submit(&spawned(
            "retry-ack",
            1_700_000_002_000,
            &effect_id(started.step_seq),
            &["wf-node0"],
        ));
        {
            let table = retry.driver.engine.as_mut().unwrap().task_table_mut();
            table.get_mut("root").unwrap().supervision = crate::scheduler::tcb::SupervisionPolicy {
                child_failure: ChildFailurePolicy::Retry,
                max_restarts: Some(1),
                cancel_children_on_exit: true,
            };
            table.get_mut("wf-node0").unwrap().budget.turns = 3;
            table.get_mut("wf-node0").unwrap().budget.total_tokens = 55;
        }
        let retried = retry.submit(&child_failed(
            "retry-failed",
            1_700_000_003_000,
            "wf-node0",
            1,
            "transient",
        ));
        let EffectKind::SpawnTasks(spawn) = &sole_effect(&retried).effect else {
            panic!("retry must be an explicit spawn");
        };
        assert_eq!(spawn.tasks[0].attempt_id.as_str(), "wf-node0:attempt:2");
        let retry_child = retry
            .driver
            .engine
            .as_ref()
            .unwrap()
            .task_table()
            .get("wf-node0")
            .unwrap();
        assert_eq!(
            (retry_child.budget.turns, retry_child.budget.total_tokens),
            (3, 55),
            "retry preserves logical-task usage"
        );

        let mut ignore = Runtime::new();
        ignore.submit(&syscall_config());
        let started = ignore.submit(&workflow_start(
            "ignore-start",
            1_700_000_001_000,
            two_node_spec(),
        ));
        ignore.submit(&spawned(
            "ignore-ack",
            1_700_000_002_000,
            &effect_id(started.step_seq),
            &["wf-node0"],
        ));
        ignore
            .driver
            .engine
            .as_mut()
            .unwrap()
            .task_table_mut()
            .get_mut("root")
            .unwrap()
            .supervision
            .child_failure = ChildFailurePolicy::Ignore;
        let advanced = ignore.submit(&child_failed(
            "ignore-failed",
            1_700_000_003_000,
            "wf-node0",
            1,
            "non-critical",
        ));
        let EffectKind::SpawnTasks(spawn) = &sole_effect(&advanced).effect else {
            panic!("ignored failure should advance the dependent node");
        };
        assert_eq!(spawn.tasks[0].task_id.as_str(), "wf-node1");
        let event = &ignore
            .driver
            .engine
            .as_ref()
            .unwrap()
            .task_table()
            .get("wf-node0")
            .unwrap()
            .supervision_events[0];
        assert_eq!(event.strategy, ChildFailurePolicy::Ignore);
        assert!(event.terminal);
        assert!(!event.relaunched);
    }

    #[test]
    fn spc_019_10_propagate_and_isolate_keep_distinct_terminal_audit_strategies() {
        use crate::scheduler::tcb::ChildFailurePolicy;

        for (index, strategy) in [ChildFailurePolicy::Propagate, ChildFailurePolicy::Isolate]
            .into_iter()
            .enumerate()
        {
            let mut runtime = Runtime::new();
            runtime.submit(&syscall_config());
            let started = runtime.submit(&workflow_start(
                &format!("terminal-start-{index}"),
                1_700_000_001_000,
                two_node_spec(),
            ));
            runtime.submit(&spawned(
                &format!("terminal-ack-{index}"),
                1_700_000_002_000,
                &effect_id(started.step_seq),
                &["wf-node0"],
            ));
            runtime
                .driver
                .engine
                .as_mut()
                .unwrap()
                .task_table_mut()
                .get_mut("root")
                .unwrap()
                .supervision
                .child_failure = strategy;
            let terminal = runtime.submit(&child_failed(
                &format!("terminal-failed-{index}"),
                1_700_000_003_000,
                "wf-node0",
                1,
                "terminal failure",
            ));
            assert!(matches!(
                terminal.step.disposition,
                StepDisposition::Terminal(_)
            ));
            let event = &runtime
                .driver
                .engine
                .as_ref()
                .unwrap()
                .task_table()
                .get("wf-node0")
                .unwrap()
                .supervision_events[0];
            assert_eq!(event.strategy, strategy);
            assert!(event.terminal);
            assert!(!event.relaunched);
        }
    }

    #[test]
    fn a_completion_naming_an_attempt_the_kernel_never_minted_is_refused() {
        let mut runtime = Runtime::new();
        runtime.submit(&configure());
        let started = runtime.submit(&workflow_start(
            "in-start",
            1_700_000_001_000,
            two_node_spec(),
        ));
        runtime.submit(&spawned(
            "in-ack-1",
            1_700_000_002_000,
            &effect_id(started.step_seq),
            &["wf-node0"],
        ));

        let forged = envelope(
            "in-forged",
            1_700_000_003_000,
            KernelInput::DeliverExternalEvent(DeliverExternalEvent {
                event: ExternalEvent::ChildCompleted(ChildCompleted {
                    task_id: TaskId::new("wf-node0").unwrap(),
                    attempt_id: WireAttemptId::new("wf-node0:attempt:7").unwrap(),
                    result: ChildResult::default(),
                    parent_requests: Vec::new(),
                }),
            }),
        );
        let before = runtime.pending_effect_kinds();
        let fault = runtime.reject(&forged);
        assert_eq!(
            fault.code,
            KernelFaultCode::InvalidAuthority,
            "a host does not mint or rewrite child identity (§10.4)"
        );
        assert_eq!(runtime.pending_effect_kinds(), before);
        assert!(runtime.tx.terminal().is_none());
    }

    #[test]
    fn a_terminated_root_workflow_refuses_every_later_state_changing_input() {
        let mut runtime = drive_workflow_root_to_terminal();
        let fault = runtime.reject(&child_done(
            "in-done-late",
            1_700_000_006_000,
            "wf-node1",
            "again",
        ));
        assert_eq!(fault.code, KernelFaultCode::InvalidLifecycle);
    }

    // -----------------------------------------------------------------------------------------
    // fixture: workflow-no-stack-and-root-kind-immutable
    // -----------------------------------------------------------------------------------------

    /// Free the pending provider effect so the nested-workflow arc has a clean `call_provider`
    /// slot. The real provider reduction has its own tests below; this planner only clears the
    /// registration so the focus assertions read against a quiet step.
    fn provider_settled(
        driver: &mut CanonicalOperationDriver,
        _context: &PlanContext<'_>,
    ) -> Result<PlannedStep, KernelFault> {
        Ok(PlannedStep::quiet(
            driver.root_kind(),
            driver.focus().cloned(),
        ))
    }

    fn agent_root_with_nested_workflow() -> (Runtime, TaskId) {
        let mut runtime = Runtime::new();
        runtime.submit(&configure());
        let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));

        // settle the provider call the agent root issued
        let settle = envelope(
            "in-provider",
            1_700_000_002_000,
            KernelInput::ResolveEffect(ResolveEffect {
                effect_id: effect_id(started.step_seq),
                outcome: EffectOutcome::Succeeded(EffectSucceeded {
                    result: EffectSuccess::Provider(super::super::effect::ProviderSuccess {
                        outcome: super::super::effect::ProviderOutcome::ContextOverflow(
                            super::super::effect::ProviderContextOverflow::default(),
                        ),
                    }),
                }),
            }),
        );
        runtime.submit_planned(&settle, provider_settled);

        let spec = two_node_spec();
        let carrier = syscall_carrier("in-submit-workflow", 1_700_000_003_000);
        let entered = runtime.submit_planned(&carrier, |driver, context| {
            driver.begin_nested_workflow(context, &spec)
        });
        runtime
            .driver
            .note_committed(entered.step_seq)
            .expect("the nested start folds like any other planned step");
        assert_eq!(sole_effect(&entered).tag(), EffectKindTag::SpawnTasks);
        (runtime, root_task_id())
    }

    /// The historical bootstrap replaced a live provider registration with a workflow spawn, so a
    /// pending provider call could disappear without ever being resolved. Here the two effects are
    /// separate registrations and neither evicts the other.
    #[test]
    fn starting_a_workflow_never_overwrites_a_live_provider_effect() {
        let mut runtime = Runtime::new();
        runtime.submit(&configure());
        let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));
        let provider_effect = sole_effect(&started).effect_id.clone();

        let spec = two_node_spec();
        let carrier = syscall_carrier("in-submit-workflow", 1_700_000_002_000);
        let entered = runtime.submit_planned(&carrier, |driver, context| {
            driver.begin_nested_workflow(context, &spec)
        });
        runtime.driver.note_committed(entered.step_seq).unwrap();

        let mut pending = runtime.pending_effect_kinds();
        pending.sort();
        assert_eq!(
            pending,
            vec![EffectKindTag::CallProvider, EffectKindTag::SpawnTasks],
            "the provider call is still outstanding; the spawn is a second registration"
        );
        assert!(
            runtime
                .tx
                .pending_effects()
                .any(|effect| effect.effect_id == provider_effect),
            "the provider effect kept its identity, so the host can still resolve it"
        );
    }

    #[test]
    fn an_agent_authored_workflow_moves_the_focus_but_never_the_root_kind() {
        let (runtime, parent) = agent_root_with_nested_workflow();

        assert_eq!(
            runtime.driver.root_kind(),
            Some(RootKind::Agent),
            "a syscall never re-roots an operation (§10.2)"
        );
        let focus = runtime.driver.focus().expect("a focus exists");
        assert!(
            focus.is_nested_in_agent(),
            "the focus records the parent agent task it must restore"
        );
        assert_eq!(
            focus,
            &ExecutionFocus::workflow_controller(
                runtime.driver.workflow_id().unwrap().clone(),
                Some(parent),
            )
        );
    }

    #[test]
    fn a_second_workflow_inside_a_workflow_focus_is_an_authority_refusal_with_no_spawn() {
        let (mut runtime, _) = agent_root_with_nested_workflow();

        let before = (
            runtime.tx.head(),
            runtime.pending_effect_kinds(),
            runtime.driver.root_kind(),
            runtime.driver.focus().cloned(),
        );

        let spec = two_node_spec();
        let carrier = syscall_carrier("in-submit-again", 1_700_000_004_000);
        let preparation = {
            let Runtime { tx, driver, .. } = &mut runtime;
            tx.prepare(&carrier, |context| {
                driver.begin_nested_workflow(context, &spec)
            })
        };
        let fault = preparation.fault().expect("expected a refusal").clone();
        assert_eq!(
            fault.code,
            KernelFaultCode::InvalidAuthority,
            "workflows do not stack — depth is at most 1 (§7.4)"
        );

        let after = (
            runtime.tx.head(),
            runtime.pending_effect_kinds(),
            runtime.driver.root_kind(),
            runtime.driver.focus().cloned(),
        );
        assert_eq!(before, after, "the refusal produced no derived action");
    }

    #[test]
    fn a_nested_workflow_completion_restores_the_parent_agent_and_resumes_its_turn() {
        let (mut runtime, parent) = agent_root_with_nested_workflow();
        let spawn_step = runtime.journal.last().unwrap().step_seq();

        runtime.submit(&spawned(
            "in-ack-1",
            1_700_000_004_000,
            &effect_id(spawn_step),
            &["wf-node0"],
        ));
        let advanced = runtime.submit(&child_done(
            "in-done-1",
            1_700_000_005_000,
            "wf-node0",
            "sources collected",
        ));
        runtime.submit(&spawned(
            "in-ack-2",
            1_700_000_006_000,
            &effect_id(advanced.step_seq),
            &["wf-node1"],
        ));
        let finished = runtime.submit(&child_done(
            "in-done-2",
            1_700_000_007_000,
            "wf-node1",
            "brief written",
        ));

        assert!(
            runtime.tx.terminal().is_none(),
            "a nested workflow's completion is not the operation's terminal (§6.1.7)"
        );
        assert_eq!(
            runtime.driver.focus(),
            Some(&ExecutionFocus::agent_turn(parent)),
            "the focus returns to the agent turn it left"
        );
        assert_eq!(
            sole_effect(&finished).tag(),
            EffectKindTag::CallProvider,
            "the parent agent's turn resumes with a provider call"
        );
        assert_eq!(runtime.driver.root_kind(), Some(RootKind::Agent));
    }

    #[test]
    fn a_workflow_root_admits_no_nested_workflow_at_all() {
        let mut runtime = Runtime::new();
        runtime.submit(&configure());
        runtime.submit(&workflow_start(
            "in-start",
            1_700_000_001_000,
            two_node_spec(),
        ));

        let spec = two_node_spec();
        let carrier = syscall_carrier("in-submit", 1_700_000_002_000);
        let preparation = {
            let Runtime { tx, driver, .. } = &mut runtime;
            tx.prepare(&carrier, |context| {
                driver.begin_nested_workflow(context, &spec)
            })
        };
        assert_eq!(
            preparation.fault().unwrap().code,
            KernelFaultCode::InvalidAuthority
        );
    }

    #[test]
    fn a_workflow_roots_focus_never_moves_while_its_dag_runs() {
        let mut runtime = Runtime::new();
        runtime.submit(&configure());
        let started = runtime.submit(&workflow_start(
            "in-start",
            1_700_000_001_000,
            two_node_spec(),
        ));
        let focus = runtime.driver.focus().cloned();

        runtime.submit(&spawned(
            "in-ack-1",
            1_700_000_002_000,
            &effect_id(started.step_seq),
            &["wf-node0"],
        ));
        assert_eq!(
            runtime.driver.focus().cloned(),
            focus,
            "an ack moves nothing"
        );

        let advanced = runtime.submit(&child_done(
            "in-done-1",
            1_700_000_003_000,
            "wf-node0",
            "done",
        ));
        assert_eq!(
            runtime.driver.focus().cloned(),
            focus,
            "a DAG node's agent execution is a child attempt, not a focus change"
        );
        assert_eq!(sole_effect(&advanced).tag(), EffectKindTag::SpawnTasks);
    }

    // -----------------------------------------------------------------------------------------
    // fixture: agent-syscall-caller-is-derived
    // -----------------------------------------------------------------------------------------

    /// Configure → agent root → provider result carrying one meta-tool call. Returns the runtime
    /// and the provider effect the start published.
    fn agent_awaiting_provider() -> (Runtime, EffectId) {
        let mut runtime = Runtime::new();
        runtime.submit(&syscall_config());
        let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));
        let effect = sole_effect(&started);
        assert_eq!(effect.tag(), EffectKindTag::CallProvider);
        let EffectKind::CallProvider(call) = &effect.effect else {
            panic!("expected a provider call");
        };
        assert!(
            call.tools.iter().any(|tool| tool.name == "start_workflow"),
            "the turn must actually expose the meta-tool the model is about to call"
        );
        (runtime, effect.effect_id.clone())
    }

    #[test]
    fn a_provider_tool_call_derives_its_caller_and_enters_p1() {
        let (mut runtime, provider) = agent_awaiting_provider();

        let spec = serde_json::to_value(two_node_spec()).unwrap();
        let entered = runtime.submit(&provider_result(
            "in-authored",
            1_700_000_002_000,
            &provider,
            vec![tool_call("call-1", "start_workflow", spec)],
        ));

        // the request became a real transition: a DAG was bootstrapped and its first node launched
        let effect = sole_effect(&entered);
        assert_eq!(effect.tag(), EffectKindTag::SpawnTasks);
        let EffectKind::SpawnTasks(spawn) = &effect.effect else {
            panic!("expected a task spawn");
        };
        for task in &spawn.tasks {
            assert_eq!(
                runtime
                    .driver
                    .engine()
                    .unwrap()
                    .task_table()
                    .get(task.task_id.as_str())
                    .and_then(|tcb| tcb.parent.as_ref())
                    .map(|parent| parent.as_str()),
                Some(ROOT_TASK_ID),
                "the provider-tool causation projects the suspended agent turn as caller"
            );
        }
        assert_eq!(
            runtime.driver.root_kind(),
            Some(RootKind::Agent),
            "a syscall never re-roots an operation (§10.2)"
        );
        assert!(
            runtime.driver.focus().unwrap().is_nested_in_agent(),
            "the focus moved to the workflow controller, under the agent turn it suspended"
        );

        // and the host declared nobody: the accepted input carries no caller field at all
        let stored = entered.record.normalized_input().unwrap();
        let json = serde_json::to_value(&stored).unwrap();
        let text = json.to_string();
        for forbidden in ["submitter_agent_id", "actor_id", "parent_session_id"] {
            assert!(
                !text.contains(forbidden),
                "the canonical input still carries {forbidden}"
            );
        }
    }

    #[test]
    fn a_successful_effect_resolution_notifies_the_durable_effect_wait() {
        use crate::scheduler::tcb::{WaitCondition, WaitMode, WaitSet};

        let (mut runtime, provider) = agent_awaiting_provider();
        runtime
            .driver
            .engine_mut()
            .unwrap()
            .task_table_mut()
            .register_wait_set(
                ROOT_TASK_ID,
                WaitSet {
                    mode: WaitMode::Any,
                    conditions: vec![WaitCondition::Effect(provider.clone())],
                },
            );

        runtime.submit(&provider_answer(
            "in-effect-wake",
            1_700_000_002_000,
            &provider,
            "done",
        ));
        assert!(
            runtime
                .driver
                .engine()
                .unwrap()
                .task_table()
                .get(ROOT_TASK_ID)
                .unwrap()
                .wait_set
                .is_none(),
            "the ResolveEffect transition is the sole effect-wait producer"
        );
    }

    #[test]
    fn a_tool_the_turn_never_exposed_has_no_caller_to_derive() {
        let (mut runtime, provider) = agent_awaiting_provider();

        // An unexposed *task* tool is not an authority claim — it is a call the model made up, and
        // the kernel's fail-closed dispatch gate answers it the way the model is trained to read:
        // a visible error result, no host dispatch, and the turn continues.
        let committed = runtime.submit(&provider_result(
            "in-forged",
            1_700_000_002_000,
            &provider,
            vec![tool_call(
                "call-1",
                "escalate_privileges",
                json!({"nodes": []}),
            )],
        ));
        assert_eq!(
            committed
                .published_effects()
                .iter()
                .map(|effect| effect.tag())
                .collect::<Vec<_>>(),
            vec![EffectKindTag::CallProvider],
            "a phantom tool is never dispatched; the turn is answered and re-asked"
        );

        // The authority half: a *syscall* name the turn did not advertise. Narrow the recorded
        // surface to nothing and ask again — the request is well-formed and still has no caller.
        let next = effect_id(committed.step_seq);
        runtime
            .driver
            .provider_calls
            .get_mut(&next)
            .expect("the driver recorded the turn it published")
            .exposed_tools
            .clear();
        let before = (
            runtime.tx.head(),
            runtime.pending_effect_kinds(),
            runtime.driver.focus().cloned(),
        );
        let fault = runtime
            .driver
            .derive_provider_syscalls(&next, &[tool_call("call-2", "start_workflow", json!({}))])
            .expect_err("a tool the turn never exposed has no causation");
        assert_eq!(fault.code, KernelFaultCode::InvalidAuthority);
        assert!(fault.message.contains("exposed no tool"));

        let after = (
            runtime.tx.head(),
            runtime.pending_effect_kinds(),
            runtime.driver.focus().cloned(),
        );
        assert_eq!(before, after, "a forged causation moves nothing");
    }

    #[test]
    fn a_provider_effect_this_kernel_never_published_has_no_causation() {
        let (runtime, _) = agent_awaiting_provider();
        let unknown = EffectId::new("op-driver-1:step:99:effect:0").unwrap();
        let fault = runtime
            .driver
            .derive_provider_syscalls(
                &unknown,
                &[tool_call("call-1", "start_workflow", json!({}))],
            )
            .expect_err("an unpublished effect names no turn");
        assert_eq!(fault.code, KernelFaultCode::InvalidAuthority);
        assert!(fault.message.contains("not a provider call"));
    }

    #[test]
    fn a_call_id_that_already_produced_a_syscall_cannot_produce_a_second() {
        let (mut runtime, provider) = agent_awaiting_provider();

        // two calls sharing one id inside a single result: the second has no causation left
        let fault = runtime.reject(&provider_result(
            "in-double",
            1_700_000_002_000,
            &provider,
            vec![
                tool_call("call-1", "skill", json!({"name": "debug"})),
                tool_call("call-1", "skill", json!({"name": "debug"})),
            ],
        ));
        assert_eq!(fault.code, KernelFaultCode::InvalidAuthority);
        assert!(fault.message.contains("consumed once"));

        // and a causation that *was* spent stays spent for the operation's lifetime
        runtime.submit(&provider_result(
            "in-skill",
            1_700_000_002_000,
            &provider,
            vec![tool_call("call-1", "skill", json!({"name": "debug"}))],
        ));
        assert!(
            runtime.driver.consumed_calls.contains("call-1"),
            "the spent causation is remembered, so a redelivery under a fresh input id buys nothing"
        );
        assert!(
            !runtime.driver.provider_calls.contains_key(&provider),
            "a resolved provider call is no longer a surface anything can be attributed to"
        );
    }

    #[test]
    fn a_skill_the_operation_never_declared_cannot_be_activated() {
        let (mut runtime, provider) = agent_awaiting_provider();
        runtime.submit(&provider_result(
            "in-skill",
            1_700_000_002_000,
            &provider,
            vec![tool_call(
                "call-1",
                "skill",
                json!({"name": "not-declared"}),
            )],
        ));
        let rejected = rejections(&runtime);
        assert_eq!(rejected.len(), 1, "the refusal is an audit fact");
        assert_eq!(rejected[0].0, "skill");
        assert_eq!(
            rejected[0].1.as_deref(),
            Some(ROOT_TASK_ID),
            "the audit fact names the caller the kernel derived, not one a host supplied"
        );
        assert!(rejected[0].2.contains("declares no skill"));
        assert_eq!(
            runtime.pending_effect_kinds(),
            vec![EffectKindTag::CallProvider],
            "a rejected capability mutation publishes no effect of its own; the turn still \
             continues (the §5k syscall-only continuation)"
        );
    }

    #[test]
    fn a_skill_without_capability_grants_keeps_name_only_activation_semantics() {
        let (mut runtime, provider) = agent_awaiting_provider();
        runtime.submit(&provider_result(
            "in-skill",
            1_700_000_002_000,
            &provider,
            vec![tool_call("call-1", "skill", json!({"name": "debug"}))],
        ));

        let engine = runtime.driver.engine().unwrap();
        assert!(engine.ctx.active_skills.contains_key("debug"));
        assert!(engine.ctx.active_skill_capabilities().is_empty());
    }

    #[test]
    fn an_append_with_no_graph_to_append_to_is_an_audit_fact_not_a_derived_action() {
        let (mut runtime, provider) = agent_awaiting_provider();
        runtime.submit(&provider_result(
            "in-append",
            1_700_000_002_000,
            &provider,
            vec![tool_call(
                "call-1",
                "submit_workflow_nodes",
                node_args(&[wire_node("stray", "stray", &[])]),
            )],
        ));

        let rejected = rejections(&runtime);
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0].0, "submit_workflow_nodes");
        assert_eq!(
            rejected[0].1.as_deref(),
            Some(ROOT_TASK_ID),
            "a provider-tool causation names the task whose turn issued the call"
        );
        assert!(rejected[0].2.contains("no workflow is in flight"));
        assert_eq!(
            runtime.pending_effect_kinds(),
            vec![EffectKindTag::CallProvider],
            "a refused append spawns nothing; the turn continues with the next provider call"
        );
    }

    // -----------------------------------------------------------------------------------------
    // fixture: workflow-dynamic-append-preserves-authority (+ §7.7 GAP-4)
    // -----------------------------------------------------------------------------------------

    /// Workflow root, first node launched and acknowledged.
    fn workflow_root_awaiting_first_child() -> Runtime {
        let mut runtime = Runtime::new();
        runtime.submit(&syscall_config());
        let started = runtime.submit(&workflow_start(
            "in-start",
            1_700_000_001_000,
            two_node_spec(),
        ));
        runtime.submit(&spawned(
            "in-ack-1",
            1_700_000_002_000,
            &effect_id(started.step_seq),
            &["wf-node0"],
        ));
        runtime
    }

    #[test]
    fn parent_requests_are_adjudicated_independently_and_never_undo_the_completion() {
        let mut runtime = workflow_root_awaiting_first_child();
        assert_eq!(
            runtime
                .driver
                .engine()
                .unwrap()
                .task_table()
                .get(ROOT_TASK_ID)
                .unwrap()
                .wait_set
                .as_ref()
                .unwrap()
                .conditions,
            vec![WaitCondition::Child("wf-node0".into())],
            "workflow join is a durable child wait before the completion arrives"
        );

        let good = SyscallRequest::AppendWorkflowNodes(
            super::super::syscall::AppendWorkflowNodesRequest {
                nodes: vec![wire_node("verify", "verify the sources", &[])],
            },
        );
        // batch-relative dependency that names nothing in its own batch — refused on its own merits
        let bad = SyscallRequest::AppendWorkflowNodes(
            super::super::syscall::AppendWorkflowNodesRequest {
                nodes: vec![wire_node("orphan", "orphan", &["nowhere"])],
            },
        );
        let another_good = SyscallRequest::UpdateTask(super::super::syscall::UpdateTaskRequest {
            update: WireTaskUpdate {
                progress: Some("sources collected".to_string()),
                ..WireTaskUpdate::default()
            },
        });

        let advanced = runtime.submit(&child_done_with(
            "in-done-1",
            1_700_000_003_000,
            "wf-node0",
            "wf-node0:attempt:1",
            vec![good, bad, another_good],
        ));

        // the completion committed unconditionally and drained into the next ready batch
        assert_eq!(
            runtime.tx.lifecycle(),
            OperationLifecycle::Running,
            "a denied parent request does not undo the child's execution (GAP-4)"
        );
        let effect = sole_effect(&advanced);
        assert_eq!(effect.tag(), EffectKindTag::SpawnTasks);
        let EffectKind::SpawnTasks(spawn) = &effect.effect else {
            panic!("expected a task spawn");
        };
        assert!(
            !runtime
                .driver
                .engine()
                .unwrap()
                .task_table()
                .wait_index()
                .lookup(&WaitKey::Child("wf-node0".into()))
                .contains(&crate::scheduler::tcb::TaskId::from(ROOT_TASK_ID)),
            "ChildCompleted consumed the completed child's wait before installing the next join"
        );
        let launched: Vec<&str> = spawn
            .tasks
            .iter()
            .map(|task| task.node_id.as_str())
            .collect();
        assert!(
            launched.contains(&"write") && launched.contains(&"verify"),
            "the admitted append reached the next ready batch alongside the original DAG, got \
             {launched:?}"
        );
        assert!(
            !launched.contains(&"orphan"),
            "the refused append produced no derived action"
        );

        // The next runnable batch was released by `wf-node0`'s committed child attempt. Its
        // caller is therefore that attempt, not the workflow table's structural root.
        for task in &spawn.tasks {
            let parent = runtime
                .driver
                .engine()
                .unwrap()
                .task_table()
                .get(task.task_id.as_str())
                .and_then(|tcb| tcb.parent.as_ref())
                .map(|parent| parent.as_str());
            assert_eq!(
                parent,
                Some("wf-node0"),
                "{} must retain the child-attempt caller that caused its launch",
                task.task_id
            );
        }

        // exactly one structured rejection, and the third request was unaffected by the second
        let rejected = rejections(&runtime);
        assert_eq!(rejected.len(), 1, "each request is adjudicated on its own");
        assert_eq!(rejected[0].0, "submit_workflow_nodes");
        assert_eq!(
            rejected[0].1.as_deref(),
            Some("wf-node0"),
            "the refusal names the child attempt it was derived from"
        );
        assert!(
            runtime
                .driver
                .engine()
                .unwrap()
                .ctx
                .partitions
                .task_state
                .progress
                .contains("sources collected"),
            "a sibling's refusal does not stop the requests after it"
        );

        let replay = Runtime::restore_with(None, &runtime.journal);
        for task in &spawn.tasks {
            let live_parent = runtime
                .driver
                .engine()
                .unwrap()
                .task_table()
                .get(task.task_id.as_str())
                .and_then(|tcb| tcb.parent.clone());
            let replay_parent = replay
                .driver
                .engine()
                .unwrap()
                .task_table()
                .get(task.task_id.as_str())
                .and_then(|tcb| tcb.parent.clone());
            assert_eq!(
                replay_parent, live_parent,
                "replay preserves caller lineage"
            );
            assert_eq!(
                replay.driver.attempt_id(task.task_id.as_str()),
                runtime.driver.attempt_id(task.task_id.as_str()),
                "replay preserves the kernel-minted child attempt"
            );
        }
    }

    #[test]
    fn spc_019_08_child_parent_requests_route_handles_through_durable_local_ipc() {
        use crate::scheduler::tcb::ChannelId;

        let mut runtime = workflow_root_awaiting_first_child();
        runtime
            .driver
            .engine_mut()
            .unwrap()
            .ctx
            .handles
            .insert(Handle::resident_for(
                77,
                HandleKind::ToolResult,
                1,
                "ipc-handle",
            ));
        let send = SyscallRequest::SendMessage(super::super::syscall::SendMessageRequest {
            message_id: "message-1".to_string(),
            to: TaskId::new(ROOT_TASK_ID).unwrap(),
            message_kind: "child_result".to_string(),
            payload_handle: HandleId::new("ipc-handle").unwrap(),
            ttl_turns: Some(4),
        });
        let publish =
            SyscallRequest::PublishChannel(super::super::syscall::PublishChannelRequest {
                channel_id: "results".to_string(),
                message_id: "channel-message-1".to_string(),
                subscribers: vec![TaskId::new(ROOT_TASK_ID).unwrap()],
                message_kind: "child_result".to_string(),
                payload_handle: HandleId::new("ipc-handle").unwrap(),
                ttl_turns: Some(4),
            });
        runtime.submit(&child_done_with(
            "in-done-1",
            1_700_000_003_000,
            "wf-node0",
            "wf-node0:attempt:1",
            vec![send.clone(), send, publish],
        ));

        let table = runtime.driver.engine_mut().unwrap().task_table_mut();
        let messages = table
            .receive_mailbox(ROOT_TASK_ID, crate::scheduler::mailbox::LogicalTime(0), 8)
            .unwrap();
        assert_eq!(messages.len(), 1, "duplicate message id is enqueued once");
        assert_eq!(messages[0].from.as_str(), "wf-node0");
        assert_eq!(messages[0].payload_handle, 77);
        assert_eq!(
            table
                .receive_channel(
                    ROOT_TASK_ID,
                    &ChannelId("results".into()),
                    crate::scheduler::mailbox::LogicalTime(0),
                )
                .unwrap()
                .len(),
            1
        );

        let checkpoint = runtime.checkpoint().decode().expect("verifies");
        let restored = Runtime::restore_with(Some(&checkpoint), &[]);
        assert_eq!(surface(&restored), surface(&runtime));
    }

    #[test]
    fn an_append_beyond_the_workflow_node_quota_is_denied_without_touching_the_graph() {
        let mut runtime = workflow_root_awaiting_first_child();
        let nodes_before = runtime.driver.engine().unwrap().workflow_node_count();

        // the quota allows 3 nodes; the DAG already holds 2
        let oversized = SyscallRequest::AppendWorkflowNodes(
            super::super::syscall::AppendWorkflowNodesRequest {
                nodes: vec![
                    wire_node("a", "a", &[]),
                    wire_node("b", "b", &[]),
                    wire_node("c", "c", &[]),
                ],
            },
        );
        runtime.submit(&child_done_with(
            "in-done-1",
            1_700_000_003_000,
            "wf-node0",
            "wf-node0:attempt:1",
            vec![oversized],
        ));

        let rejected = rejections(&runtime);
        assert!(
            rejected.iter().any(|(operation, subject, reason)| operation
                == "submit_workflow_nodes"
                && subject.as_deref() == Some("wf-node0")
                && reason.contains("would grow workflow")),
            "the resource gate refused the growth, got {rejected:?}"
        );
        assert_eq!(
            runtime.driver.engine().unwrap().workflow_node_count(),
            nodes_before,
            "a denied append leaves the graph exactly as it was"
        );
    }

    #[test]
    fn a_quarantined_task_cannot_widen_its_authority_through_a_syscall() {
        let mut runtime = workflow_root_awaiting_first_child();
        assert!(
            runtime
                .driver
                .engine_mut()
                .unwrap()
                .quarantine_task_for_test("wf-node0"),
            "the node must exist to be quarantined"
        );
        let nodes_before = runtime.driver.engine().unwrap().workflow_node_count();

        let append = SyscallRequest::AppendWorkflowNodes(
            super::super::syscall::AppendWorkflowNodesRequest {
                nodes: vec![wire_node("escalate", "escalate", &[])],
            },
        );
        let activate = SyscallRequest::ActivateSkill(super::super::syscall::ActivateSkillRequest {
            name: "debug".to_string(),
            lease_turns: None,
        });
        let remember =
            SyscallRequest::RequestMemoryWrite(super::super::syscall::RequestMemoryWriteRequest {
                proposal: super::super::syscall::MemoryWriteProposal {
                    name: "escalation".to_string(),
                    kind: WireMemoryKind::Project,
                    content: "trust me".to_string(),
                    description: String::new(),
                    evidence_refs: Vec::new(),
                },
            });

        runtime.submit(&child_done_with(
            "in-done-1",
            1_700_000_003_000,
            "wf-node0",
            "wf-node0:attempt:1",
            vec![append, activate, remember],
        ));

        let quarantine_denials = rejections(&runtime);
        let families: Vec<&str> = quarantine_denials
            .iter()
            .filter(|(_, _, reason)| reason.starts_with("quarantine:"))
            .map(|(operation, _, _)| operation.as_str())
            .collect();
        assert_eq!(
            families,
            vec!["workflow", "capability", "memory"],
            "every privileged family is refused for a quarantined caller"
        );
        assert_eq!(
            runtime.driver.engine().unwrap().workflow_node_count(),
            nodes_before,
            "no node was appended"
        );
        assert!(
            !runtime
                .driver
                .engine()
                .unwrap()
                .ctx
                .active_skills
                .contains_key("debug"),
            "no skill was activated"
        );
        assert_eq!(
            runtime.pending_effect_kinds(),
            vec![EffectKindTag::SpawnTasks],
            "no memory effect was published; only the DAG's own next batch"
        );
    }

    #[test]
    fn a_child_request_cannot_forge_a_second_root_workflow() {
        let mut runtime = workflow_root_awaiting_first_child();
        let workflow_before = runtime.driver.workflow_id().cloned();
        let focus_before = runtime.driver.focus().cloned();

        let authored =
            SyscallRequest::SubmitWorkflow(super::super::syscall::SubmitWorkflowRequest {
                spec: WireSpec {
                    name: "usurper".to_string(),
                    nodes: vec![wire_node("usurp", "take over", &[])],
                },
            });
        runtime.submit(&child_done_with(
            "in-done-1",
            1_700_000_003_000,
            "wf-node0",
            "wf-node0:attempt:1",
            vec![authored],
        ));

        assert_eq!(
            runtime.driver.root_kind(),
            Some(RootKind::Workflow),
            "the root kind is immutable (§6.1.5)"
        );
        assert_eq!(
            runtime.driver.workflow_id().cloned(),
            workflow_before,
            "an authored spec flattens into the running DAG; it never becomes a second root"
        );
        assert_eq!(
            runtime.driver.focus().cloned(),
            focus_before,
            "a workflow root's focus never moves (§7.4)"
        );
        assert!(
            rejections(&runtime).is_empty(),
            "flattening is the admitted path, not a refusal"
        );
    }

    // -----------------------------------------------------------------------------------------
    // fixture: workflow-child-identity-is-kernel-issued (TCB launch arc)
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_child_moves_pending_launch_then_starting_then_running_on_the_acknowledgement() {
        let mut runtime = Runtime::new();
        runtime.submit(&syscall_config());

        // the spawn effect is planned but not yet committed: the identity exists, the launch does not
        let preparation = runtime.prepare(&workflow_start(
            "in-start",
            1_700_000_001_000,
            two_node_spec(),
        ));
        assert_eq!(
            runtime.driver.engine().unwrap().task_lifecycle("wf-node0"),
            Some(TaskLifecycle::Starting),
            "the launch effect is planned, so the task left PendingLaunch and awaits the host"
        );
        let started = runtime.append_and_commit(preparation);
        runtime.driver.note_committed(started.step_seq).unwrap();
        assert_eq!(
            runtime.driver.engine().unwrap().task_lifecycle("wf-node0"),
            Some(TaskLifecycle::Starting),
            "a published launch is not a running task"
        );

        runtime.submit(&spawned(
            "in-ack-1",
            1_700_000_002_000,
            &effect_id(started.step_seq),
            &["wf-node0"],
        ));
        assert_eq!(
            runtime.driver.engine().unwrap().task_lifecycle("wf-node0"),
            Some(TaskLifecycle::Running),
            "only the acknowledgement makes a task Running (§10.4, §15.3)"
        );
    }

    /// The launch arc at its own layer, where all three states are separately observable. Through
    /// the driver `PendingLaunch` and `Starting` both happen inside one `plan` call, because minting
    /// identity and building the launch effect are two moments of the same transition.
    #[test]
    fn an_ack_gated_spawn_mints_identity_in_pending_launch_before_the_effect_is_published() {
        let mut engine = LoopStateMachine::new(SchedulerBudget::default());
        let action = engine.load_workflow(
            build_core_spec(&WireSpec {
                name: String::new(),
                nodes: vec![wire_node("only", "only", &[])],
            })
            .unwrap(),
        );
        assert!(matches!(action, LoopAction::SpawnWorkflow { .. }));
        assert_eq!(
            engine.task_lifecycle("wf-node0"),
            Some(TaskLifecycle::PendingLaunch),
            "identity is minted, the launch is not published yet"
        );

        engine.mark_tasks_starting(&["wf-node0".to_string()]);
        assert_eq!(
            engine.task_lifecycle("wf-node0"),
            Some(TaskLifecycle::Starting),
            "the launch effect is published; the host has not answered"
        );

        engine.resolve_workflow_spawn(vec!["wf-node0".to_string()], Vec::new());
        assert_eq!(
            engine.task_lifecycle("wf-node0"),
            Some(TaskLifecycle::Running),
            "only the acknowledgement makes it Running"
        );
    }

    #[test]
    fn a_failed_launch_ends_the_attempt_and_refuses_any_later_completion_for_it() {
        use crate::runtime::kernel::wire::effect::{TaskLaunchFailed, TaskLaunchStatus};

        // two independent nodes, so failing one leaves the DAG running rather than terminating it
        let parallel = WireSpec {
            name: "parallel".to_string(),
            nodes: vec![
                wire_node("left", "left", &[]),
                wire_node("right", "right", &[]),
            ],
        };
        let mut runtime = Runtime::new();
        runtime.submit(&syscall_config());
        let started = runtime.submit(&workflow_start("in-start", 1_700_000_001_000, parallel));

        let failure = envelope(
            "in-ack-fail",
            1_700_000_002_000,
            KernelInput::ResolveEffect(ResolveEffect {
                effect_id: effect_id(started.step_seq),
                outcome: EffectOutcome::Succeeded(EffectSucceeded {
                    result: EffectSuccess::TasksSpawned(TasksSpawnedSuccess {
                        attempts: vec![
                            TaskLaunchOutcome {
                                task_id: TaskId::new("wf-node0").unwrap(),
                                attempt_id: WireAttemptId::new("wf-node0:attempt:1").unwrap(),
                                outcome: TaskLaunchStatus::Failed(TaskLaunchFailed {
                                    failure: super::super::effect::TaskLaunchFailure {
                                        kind:
                                            super::super::effect::HostEffectFailureKind::StorageUnavailable,
                                        message: "no worker".to_string(),
                                    },
                                }),
                            },
                            TaskLaunchOutcome {
                                task_id: TaskId::new("wf-node1").unwrap(),
                                attempt_id: WireAttemptId::new("wf-node1:attempt:1").unwrap(),
                                outcome: TaskLaunchStatus::Started(TaskLaunchStarted {}),
                            },
                        ],
                    }),
                }),
            }),
        );
        runtime.submit(&failure);
        assert!(
            runtime
                .driver
                .engine()
                .unwrap()
                .task_lifecycle("wf-node0")
                .is_some_and(|state| state.is_terminal()),
            "a failed launch terminates the attempt"
        );

        let fault = runtime.reject(&child_done_with(
            "in-late",
            1_700_000_003_000,
            "wf-node0",
            "wf-node0:attempt:1",
            Vec::new(),
        ));
        assert_eq!(
            fault.code,
            KernelFaultCode::InvalidAuthority,
            "a terminated attempt is a stale causation"
        );
    }

    #[test]
    fn a_second_completion_for_a_spent_attempt_carries_no_authority() {
        let mut runtime = workflow_root_awaiting_first_child();
        runtime.submit(&child_done_with(
            "in-done-1",
            1_700_000_003_000,
            "wf-node0",
            "wf-node0:attempt:1",
            Vec::new(),
        ));
        let nodes_before = runtime.driver.engine().unwrap().workflow_node_count();

        let replayed = child_done_with(
            "in-done-1-again",
            1_700_000_004_000,
            "wf-node0",
            "wf-node0:attempt:1",
            vec![SyscallRequest::AppendWorkflowNodes(
                super::super::syscall::AppendWorkflowNodesRequest {
                    nodes: vec![wire_node("smuggled", "smuggled", &[])],
                },
            )],
        );
        let fault = runtime.reject(&replayed);
        assert_eq!(fault.code, KernelFaultCode::InvalidAuthority);
        assert_eq!(
            runtime.driver.engine().unwrap().workflow_node_count(),
            nodes_before,
            "a refused completion appends nothing"
        );
    }

    // -----------------------------------------------------------------------------------------
    // §7.6 · memory proposals
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_memory_proposal_becomes_a_kernel_authored_write_with_derived_provenance() {
        let mut runtime = workflow_root_awaiting_first_child();
        let write =
            SyscallRequest::RequestMemoryWrite(super::super::syscall::RequestMemoryWriteRequest {
                proposal: super::super::syscall::MemoryWriteProposal {
                    name: "source-set".to_string(),
                    kind: WireMemoryKind::Project,
                    content: "12 primary sources".to_string(),
                    description: String::new(),
                    evidence_refs: Vec::new(),
                },
            });
        let advanced = runtime.submit(&child_done_with(
            "in-done-1",
            1_700_000_003_000,
            "wf-node0",
            "wf-node0:attempt:1",
            vec![write],
        ));

        let persisted = advanced
            .published_effects()
            .iter()
            .find(|effect| effect.tag() == EffectKindTag::PersistMemory)
            .expect("the proposal published a memory write");
        assert_eq!(
            persisted.effect_id,
            effect_id_at(advanced.step_seq, 0),
            "syscall effects mint their own identity from the step they belong to"
        );
        let EffectKind::PersistMemory(effect) = &persisted.effect else {
            panic!("expected a memory write");
        };
        assert_eq!(effect.binding.binding_id.as_str(), "mem-binding-1");
        assert_eq!(
            effect.memory.accepted_at_ms,
            WireU64::new(1_700_000_003_000),
            "provenance time is the envelope's accepted time, never a host clock (DEC-2)"
        );
        match &effect.memory.causation {
            SyscallCausation::ChildAttempt(child) => {
                assert_eq!(child.task_id.as_str(), "wf-node0");
                assert_eq!(child.attempt_id.as_str(), "wf-node0:attempt:1");
                assert_eq!(child.request_seq, 0, "the seq is the list's own order");
            }
            other => panic!("expected a child-attempt causation, got {other:?}"),
        }
        // the proposal contributed no security field, and the record does not grow one
        let json = serde_json::to_value(&effect.memory).unwrap().to_string();
        for forbidden in ["tenant", "author", "trust_level", "record_id", "session"] {
            assert!(
                !json.contains(forbidden),
                "the kernel-authored write leaked {forbidden}"
            );
        }
    }

    #[test]
    fn a_memory_query_is_clamped_to_the_operations_retrieval_policy() {
        let (mut runtime, provider) = agent_awaiting_provider();
        let resolved = runtime.submit(&provider_result(
            "in-recall",
            1_700_000_002_000,
            &provider,
            vec![tool_call(
                "call-1",
                crate::context::manager::MEMORY_TOOL_NAME,
                json!({"query": "past briefs", "top_k": 999}),
            )],
        ));
        let queried = sole_effect(&resolved);
        let EffectKind::QueryMemory(effect) = &queried.effect else {
            panic!("expected a memory query, got {:?}", queried.tag());
        };
        assert_eq!(
            effect.requested_k, 4,
            "the model cannot widen the operation's retrieval policy by asking for more"
        );
        assert!(matches!(
            effect.query.causation,
            SyscallCausation::ProviderTool(_)
        ));
        // the query the kernel authored is binding + causation + accepted time, nothing else
        let json = serde_json::to_value(&effect.query).unwrap().to_string();
        for forbidden in ["session", "tenant", "author", "trust", "agent_id"] {
            assert!(
                !json.contains(forbidden),
                "the kernel-authored query leaked {forbidden}"
            );
        }
        assert_eq!(effect.binding.binding_id.as_str(), "mem-binding-1");
    }

    // -----------------------------------------------------------------------------------------
    // fixture: no-host-session-identity (§22.6 · Task 11)
    // -----------------------------------------------------------------------------------------

    /// Every JSON key anywhere in `value`.
    fn all_keys(value: &Value, into: &mut BTreeSet<String>) {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    into.insert(key.clone());
                    all_keys(child, into);
                }
            }
            Value::Array(items) => items.iter().for_each(|item| all_keys(item, into)),
            _ => {}
        }
    }

    /// One full canonical arc — configure → workflow root start → spawn effect → spawn
    /// acknowledgement → child completion carrying a memory proposal — as a host runs it.
    ///
    /// Returns everything the arc made durable or published: the record chain (bytes and digests),
    /// the effects, and the observations. Nothing here is host-timed or host-named beyond the
    /// opaque ids §5.3 admits.
    fn canonical_arc() -> (Vec<Value>, Vec<Value>, Vec<Value>) {
        let mut runtime = Runtime::new();
        let mut effects = Vec::new();
        let mut observations = Vec::new();
        let collect = |_runtime: &Runtime,
                       committed: &CommittedTransition<PlannedStep>,
                       effects: &mut Vec<Value>,
                       observations: &mut Vec<Value>| {
            for effect in committed.published_effects() {
                effects.push(serde_json::to_value(effect).unwrap());
            }
            for observation in &committed.step.observations {
                observations.push(serde_json::to_value(observation).unwrap());
            }
        };

        let configured = runtime.submit(&syscall_config());
        collect(&runtime, &configured, &mut effects, &mut observations);

        let started = runtime.submit(&workflow_start(
            "in-start",
            1_700_000_001_000,
            two_node_spec(),
        ));
        collect(&runtime, &started, &mut effects, &mut observations);

        let acked = runtime.submit(&spawned(
            "in-ack-1",
            1_700_000_002_000,
            &effect_id(started.step_seq),
            &["wf-node0"],
        ));
        collect(&runtime, &acked, &mut effects, &mut observations);

        let write =
            SyscallRequest::RequestMemoryWrite(super::super::syscall::RequestMemoryWriteRequest {
                proposal: super::super::syscall::MemoryWriteProposal {
                    name: "source-set".to_string(),
                    kind: WireMemoryKind::Project,
                    content: "12 primary sources".to_string(),
                    description: String::new(),
                    evidence_refs: Vec::new(),
                },
            });
        let advanced = runtime.submit(&child_done_with(
            "in-done-1",
            1_700_000_003_000,
            "wf-node0",
            "wf-node0:attempt:1",
            vec![write],
        ));
        collect(&runtime, &advanced, &mut effects, &mut observations);

        let records = runtime
            .journal
            .iter()
            .map(|record| {
                json!({
                    "bytes": String::from_utf8_lossy(record.record_bytes().as_slice()).into_owned(),
                    "digest": record.record_digest().as_str(),
                })
            })
            .collect();
        (records, effects, observations)
    }

    /// §22.6 · "host mapping does not affect kernel deterministic output", proved forwards: the arc
    /// is a pure function of its logical inputs, so two hosts — whatever they call their sessions —
    /// build the same record chain, publish the same effect bytes and read the same observations.
    #[test]
    fn the_canonical_arc_is_byte_identical_for_any_host() {
        let (records_a, effects_a, observations_a) = canonical_arc();
        let (records_b, effects_b, observations_b) = canonical_arc();

        assert!(!records_a.is_empty() && !effects_a.is_empty() && !observations_a.is_empty());
        assert_eq!(records_a, records_b, "the record chain is not reproducible");
        assert_eq!(
            effects_a, effects_b,
            "published effects are not reproducible"
        );
        assert_eq!(
            observations_a, observations_b,
            "observations are not reproducible"
        );
    }

    /// The scan half of Task 11: no session key, and no session *value*, exists anywhere on the
    /// canonical output surface — the durable records, the effects the host executes, or the
    /// observations it projects. A host may keep its own session mapping; the kernel holds none.
    #[test]
    fn no_canonical_record_effect_or_observation_names_a_session() {
        const BANNED_KEYS: [&str; 5] = [
            "session_id",
            "parent_session_id",
            "session",
            "submitter_agent_id",
            "actor_id",
        ];

        let (records, effects, observations) = canonical_arc();
        for (surface, values) in [
            ("record", &records),
            ("effect", &effects),
            ("observation", &observations),
        ] {
            for value in values {
                let mut keys = BTreeSet::new();
                all_keys(value, &mut keys);
                for banned in BANNED_KEYS {
                    assert!(
                        !keys.contains(banned),
                        "canonical {surface} carries the host-owned key {banned:?}: {value}"
                    );
                }
                assert!(
                    !value.to_string().contains("session"),
                    "canonical {surface} mentions a session: {value}"
                );
            }
        }

        // and the child the arc launched is correlated by logical identity alone
        let launch = effects
            .iter()
            .find(|effect| effect["effect"]["kind"] == "spawn_tasks")
            .expect("the arc launched a child");
        let task = &launch["effect"]["tasks"][0];
        assert_eq!(task["task_id"], "wf-node0");
        assert_eq!(task["attempt_id"], "wf-node0:attempt:1");
        assert!(
            task["launch_token"].as_str().is_some_and(|t| !t.is_empty()),
            "a child is named by task/attempt/launch token, never by a session"
        );
    }

    /// The process observation the arc publishes states the logical **parent task**, and the child
    /// spec the kernel derives for the legacy engine carries an empty session — the canonical path
    /// cannot populate one because no canonical input has a field for it.
    #[test]
    fn a_spawned_process_is_reported_by_its_logical_parent_task() {
        let (_, _, observations) = canonical_arc();
        let process = observations
            .iter()
            .find(|observation| observation["kind"] == "agent_process_changed")
            .expect("the arc published a process observation");
        assert_eq!(process["agent_id"], "wf-node0");
        assert_eq!(process["parent_task_id"], "root");

        let spec = agent_run_spec(&LogicalAgentSpec::new("write the brief"));
        assert_eq!(spec.identity.session_id.as_str(), NO_HOST_SESSION);
        assert!(spec.identity.parent_session_id.is_none());
    }

    // -----------------------------------------------------------------------------------------
    // fixture: removed-self-declared-caller
    // -----------------------------------------------------------------------------------------

    /// Every P1 request shape, serialized. None of them has a field through which a host could say
    /// who is asking — §22.10's whole point, and the reason omitting an id can no longer mean
    /// "skip the trust downgrade".
    #[test]
    fn no_syscall_request_shape_carries_a_self_declared_caller() {
        use super::super::syscall::*;

        let requests = vec![
            SyscallRequest::SubmitWorkflow(SubmitWorkflowRequest {
                spec: two_node_spec(),
            }),
            SyscallRequest::AppendWorkflowNodes(AppendWorkflowNodesRequest {
                nodes: vec![wire_node("n", "n", &[])],
            }),
            SyscallRequest::ActivateSkill(ActivateSkillRequest {
                name: "debug".to_string(),
                lease_turns: Some(3),
            }),
            SyscallRequest::UpdateTask(UpdateTaskRequest {
                update: WireTaskUpdate::default(),
            }),
            SyscallRequest::RequestMemoryWrite(RequestMemoryWriteRequest {
                proposal: MemoryWriteProposal {
                    name: "n".to_string(),
                    kind: WireMemoryKind::Project,
                    content: "c".to_string(),
                    description: String::new(),
                    evidence_refs: Vec::new(),
                },
            }),
            SyscallRequest::RequestMemoryQuery(RequestMemoryQueryRequest {
                query: MemoryQueryProposal::default(),
            }),
            SyscallRequest::PageIn(PageInRequest {
                handle_id: super::super::scalar::HandleId::new("h-1").unwrap(),
            }),
        ];

        for request in &requests {
            let text = serde_json::to_value(request).unwrap().to_string();
            for forbidden in [
                "submitter_agent_id",
                "actor_id",
                "agent_id",
                "session_id",
                "parent_session_id",
                "caller",
                "author",
                "trust",
            ] {
                assert!(
                    !text.contains(forbidden),
                    "{request:?} still exposes {forbidden}"
                );
            }
        }

        // a host cannot smuggle one in either: the shapes deny unknown fields
        let smuggled = r#"{"kind":"append_workflow_nodes","nodes":[],"submitter_agent_id":"root"}"#;
        assert!(
            serde_json::from_str::<SyscallRequest>(smuggled).is_err(),
            "a self-declared submitter must not decode"
        );
    }

    /// The three authority families a quarantined caller is refused, kept exhaustive against the
    /// request union so a new syscall cannot be added without a decision about it.
    #[test]
    fn every_syscall_is_classified_against_the_quarantine_rule() {
        use super::super::syscall::*;

        let classified = [
            (
                SyscallRequest::SubmitWorkflow(SubmitWorkflowRequest {
                    spec: WireSpec::default(),
                }),
                Some("workflow"),
            ),
            (
                SyscallRequest::AppendWorkflowNodes(AppendWorkflowNodesRequest {
                    nodes: Vec::new(),
                }),
                Some("workflow"),
            ),
            (
                SyscallRequest::ActivateSkill(ActivateSkillRequest {
                    name: String::new(),
                    lease_turns: None,
                }),
                Some("capability"),
            ),
            (
                SyscallRequest::RequestMemoryWrite(RequestMemoryWriteRequest {
                    proposal: MemoryWriteProposal {
                        name: String::new(),
                        kind: WireMemoryKind::Project,
                        content: String::new(),
                        description: String::new(),
                        evidence_refs: Vec::new(),
                    },
                }),
                Some("memory"),
            ),
            (
                SyscallRequest::RequestMemoryQuery(RequestMemoryQueryRequest {
                    query: MemoryQueryProposal::default(),
                }),
                Some("memory"),
            ),
            (
                SyscallRequest::UpdateTask(UpdateTaskRequest {
                    update: WireTaskUpdate::default(),
                }),
                None,
            ),
            (
                SyscallRequest::PageIn(PageInRequest {
                    handle_id: super::super::scalar::HandleId::new("h-1").unwrap(),
                }),
                None,
            ),
        ];
        for (request, family) in &classified {
            assert_eq!(&privileged_family(request), family, "{request:?}");
        }
    }

    #[test]
    fn a_page_in_of_a_handle_the_caller_does_not_hold_is_refused() {
        let (mut runtime, provider) = agent_awaiting_provider();
        runtime.submit(&provider_result(
            "in-read",
            1_700_000_002_000,
            &provider,
            vec![tool_call(
                "call-1",
                crate::context::manager::READ_RESULT_TOOL_NAME,
                json!({"call_id": "never-existed"}),
            )],
        ));
        let rejected = rejections(&runtime);
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0].0, "read_result");
        assert!(
            rejected[0].2.contains("not reachable"),
            "got {:?}",
            rejected[0].2
        );
        assert_eq!(
            runtime.pending_effect_kinds(),
            vec![EffectKindTag::CallProvider],
            "an address the caller does not hold produces no page-in effect; the turn continues"
        );
    }

    // -----------------------------------------------------------------------------------------
    // §7.10 · external payload (Task 13)
    // -----------------------------------------------------------------------------------------

    /// The body a host persists before it ever submits a result. Long enough to be over the test
    /// threshold, so both directions of the partition are exercised by real sizes.
    const BODY: &str = "the full report body, far larger than this operation keeps resident, \
                        repeated so it clears the inline threshold by a comfortable margin";

    fn body_digest() -> Digest {
        super::super::record::canonical_digest(BODY.as_bytes())
    }

    /// A payload policy small enough to test with real strings: results reaching 64 bytes must be
    /// externalised, and at most 32 bytes of preview stay resident.
    fn payload_config() -> WireEnvelope {
        use crate::runtime::kernel::wire::config::PayloadPolicy;
        syscall_config_with(|config| {
            config.host_effect_support = support_with([EffectKindTag::ArchivePageOut]);
            config.payload_policy = Some(PayloadPolicy {
                inline_threshold_bytes: Some(64),
                preview_bytes: Some(32),
            });
        })
    }

    fn external_payload(
        call_id: &str,
        digest: Digest,
        original_size: u64,
        preview: &str,
    ) -> WireToolResultPayload {
        external_payload_with(
            call_id,
            digest,
            original_size,
            preview,
            false,
            ToolResultDisposition::Recoverable,
        )
    }

    /// The same, with the two §7.10 rule 9 failure facts stated.
    fn external_payload_with(
        call_id: &str,
        digest: Digest,
        original_size: u64,
        preview: &str,
        is_error: bool,
        disposition: ToolResultDisposition,
    ) -> WireToolResultPayload {
        WireToolResultPayload::External(super::super::effect::ExternalToolResult {
            call_id: CallId::new(call_id).unwrap(),
            payload_ref: PayloadRef::new("payload:01J8Y2QK7C4N0V").unwrap(),
            digest,
            original_size: WireU64::new(original_size),
            preview: preview.to_string(),
            is_error,
            disposition,
        })
    }

    fn payloads_resolved(
        id: &str,
        at: u64,
        effect: &EffectId,
        results: Vec<WireToolResultPayload>,
    ) -> WireEnvelope {
        resolved(
            id,
            at,
            effect,
            EffectSuccess::Tools(ToolsSuccess { results }),
        )
    }

    /// An agent that called one host tool and is waiting for its results, under `payload_config`.
    fn agent_awaiting_tool_results() -> (Runtime, EffectId) {
        let mut runtime = Runtime::new();
        runtime.submit(&payload_config());
        let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));
        let acted = runtime.submit(&provider_result(
            "in-acted",
            1_700_000_002_000,
            &effect_id(started.step_seq),
            vec![tool_call("call-1", "search", json!({"q": "sources"}))],
        ));
        let tools = sole_effect(&acted);
        assert_eq!(tools.tag(), EffectKindTag::ExecuteTools);
        (runtime, tools.effect_id.clone())
    }

    /// Structured durable content is measured as part of the inline result, so this fixture uses
    /// a threshold large enough for the mixed text/image/file envelope while keeping the
    /// external-payload tests on their intentionally small threshold above.
    fn agent_awaiting_structured_tool_results() -> (Runtime, EffectId) {
        use crate::runtime::kernel::wire::config::PayloadPolicy;
        let mut runtime = Runtime::new();
        runtime.submit(&syscall_config_with(|config| {
            config.payload_policy = Some(PayloadPolicy {
                inline_threshold_bytes: Some(1024),
                preview_bytes: Some(256),
            });
        }));
        let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));
        let acted = runtime.submit(&provider_result(
            "in-acted",
            1_700_000_002_000,
            &effect_id(started.step_seq),
            vec![tool_call("call-1", "search", json!({"q": "sources"}))],
        ));
        let tools = sole_effect(&acted);
        assert_eq!(tools.tag(), EffectKindTag::ExecuteTools);
        (runtime, tools.effect_id.clone())
    }

    /// The whole point of the contract: the *preview* enters context and the handle says where the
    /// body is. Nothing about the body itself is inside this kernel.
    #[test]
    fn an_external_tool_result_lands_as_a_preview_and_an_external_handle() {
        let (mut runtime, tools) = agent_awaiting_tool_results();
        let committed = runtime.submit(&payloads_resolved(
            "in-results",
            1_700_000_003_000,
            &tools,
            vec![external_payload(
                "call-1",
                body_digest(),
                BODY.len() as u64,
                "the full report body, far la…",
            )],
        ));
        assert_eq!(
            kinds(&committed),
            vec![EffectKindTag::CallProvider],
            "an external result resumes the turn exactly like an inline one"
        );
        let EffectKind::CallProvider(provider) = &sole_effect(&committed).effect else {
            panic!("external residency must resume through the provider");
        };
        assert!(
            provider
                .tools
                .iter()
                .any(|tool| tool.name.as_str() == READ_RESULT_TOOL_NAME),
            "the refreshed provider projection must advertise the newly reachable payload"
        );

        let engine = runtime.driver.engine().expect("the arc built an engine");
        assert_eq!(
            engine.ctx.payload_residency("call-1"),
            Some(&Residency::External {
                payload_ref: "payload:01J8Y2QK7C4N0V".to_string(),
                digest: body_digest().as_str().to_string(),
                original_size: BODY.len() as u64,
            }),
            "§7.10 rule 3 · the P3 handle is where the reference lives"
        );

        let rendered = serde_json::to_string(&engine.ctx.partitions.history.messages).unwrap();
        assert!(
            rendered.contains("the full report body, far la"),
            "the preview is what occupies working context"
        );
        assert!(
            !rendered.contains("clears the inline threshold"),
            "the body must not be in context: {rendered}"
        );
        assert!(observation_kinds(&runtime).contains(&"payload_residency_changed"));
        assert_eq!(
            runtime
                .observations()
                .iter()
                .filter(|observation| matches!(
                    observation,
                    KernelObservation::CheckpointTaken { .. }
                ))
                .count(),
            1,
            "re-projecting external residency must not execute the provider-call boundary twice",
        );
    }

    /// §7.10 rule 2 · a digest this kernel cannot recompute is a payload it could never prove it
    /// restored, so it is refused at admission rather than at page-in.
    #[test]
    fn an_external_tool_result_the_kernel_cannot_verify_is_refused() {
        let (mut runtime, tools) = agent_awaiting_tool_results();
        let fault = runtime.reject(&payloads_resolved(
            "in-results",
            1_700_000_003_000,
            &tools,
            vec![external_payload(
                "call-1",
                Digest::new("md5:deadbeef").unwrap(),
                BODY.len() as u64,
                "preview",
            )],
        ));
        assert_eq!(fault.code, KernelFaultCode::MalformedEnvelope);
        assert!(
            fault.message.contains("sha256:<64 hex>"),
            "the refusal names the only digest shape a page-in can be checked against: {}",
            fault.message
        );
    }

    /// §7.10 rule 2 · the configured threshold is a **total** partition. A body small enough to
    /// inline may not be externalised: it would buy a `LoadPayload` round trip to read something
    /// that fitted in the turn that produced it, and it would leave the one rule a host and the
    /// kernel must agree on with a hole in the middle.
    #[test]
    fn an_external_tool_result_below_the_threshold_is_refused() {
        let (mut runtime, tools) = agent_awaiting_tool_results();
        let small = "tiny";
        let fault = runtime.reject(&payloads_resolved(
            "in-results",
            1_700_000_003_000,
            &tools,
            vec![external_payload(
                "call-1",
                super::super::record::canonical_digest(small.as_bytes()),
                small.len() as u64,
                small,
            )],
        ));
        assert_eq!(fault.code, KernelFaultCode::MalformedEnvelope);
        assert!(
            fault.message.contains("inlines below 64"),
            "{}",
            fault.message
        );
    }

    /// The preview is the part that actually occupies context, so it is the part the policy bounds.
    #[test]
    fn an_external_preview_over_the_resident_budget_is_refused() {
        let (mut runtime, tools) = agent_awaiting_tool_results();
        let fault = runtime.reject(&payloads_resolved(
            "in-results",
            1_700_000_003_000,
            &tools,
            vec![external_payload(
                "call-1",
                body_digest(),
                BODY.len() as u64,
                BODY,
            )],
        ));
        assert_eq!(fault.code, KernelFaultCode::ResourceLimitExceeded);
        assert!(fault.message.contains("preview"), "{}", fault.message);
    }

    /// §7.10 rules 1 and 5 · the kernel does not externalise on the host's behalf. Accepting an
    /// oversized inline result and spooling it back out is the historical round trip where the body
    /// crossed core twice and entered the journal twice; the only answer that keeps rule 5 true is
    /// to refuse it.
    #[test]
    fn an_inline_tool_result_over_the_threshold_is_refused() {
        let (mut runtime, tools) = agent_awaiting_tool_results();
        let fault = runtime.reject(&tools_resolved(
            "in-results",
            1_700_000_003_000,
            &tools,
            &[("call-1", BODY, false)],
        ));
        assert_eq!(fault.code, KernelFaultCode::ResourceLimitExceeded);
        assert!(
            fault.message.contains("externalises at 64"),
            "{}",
            fault.message
        );
        assert_eq!(
            runtime.pending_effect_kinds(),
            vec![EffectKindTag::ExecuteTools],
            "a rejected batch leaves the effect it was answering pending — zero mutation"
        );
    }

    /// A batch is adjudicated whole: one illegal result and none of it lands.
    #[test]
    fn one_illegal_result_rejects_the_whole_batch() {
        let mut runtime = Runtime::new();
        runtime.submit(&payload_config());
        let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));
        let acted = runtime.submit(&provider_result(
            "in-acted",
            1_700_000_002_000,
            &effect_id(started.step_seq),
            vec![
                tool_call("call-1", "search", json!({"q": "a"})),
                tool_call("call-2", "search", json!({"q": "b"})),
            ],
        ));
        let tools = sole_effect(&acted).effect_id.clone();
        runtime.reject(&payloads_resolved(
            "in-results",
            1_700_000_003_000,
            &tools,
            vec![
                WireToolResultPayload::Inline(InlineToolResult {
                    call_id: CallId::new("call-1").unwrap(),
                    result: WireToolResult {
                        output: "small and legal".to_string(),
                        durable_content: None,
                        is_error: false,
                        disposition: ToolResultDisposition::Recoverable,
                        tokens: None,
                    },
                }),
                external_payload("call-2", Digest::new("md5:deadbeef").unwrap(), 9_000, "p"),
            ],
        ));
        let engine = runtime.driver.engine().expect("the arc built an engine");
        assert!(
            engine.ctx.payload_residency("call-1").is_none(),
            "the legal half of a refused batch must not have landed either"
        );
    }

    /// The full §7.10 rule 4 arc: `read_result` → `LoadPayload` → `PayloadLoaded`, with the body
    /// entering context only once the digest proves it is the one that left.
    #[test]
    fn a_page_in_of_an_external_payload_loads_and_verifies_the_body() {
        let (mut runtime, tools) = agent_awaiting_tool_results();
        let stored = runtime.submit(&payloads_resolved(
            "in-results",
            1_700_000_003_000,
            &tools,
            vec![external_payload(
                "call-1",
                body_digest(),
                BODY.len() as u64,
                "the full report body, far la…",
            )],
        ));

        let read = runtime.submit(&provider_result(
            "in-read",
            1_700_000_004_000,
            &effect_id(stored.step_seq),
            vec![tool_call(
                "call-2",
                READ_RESULT_TOOL_NAME,
                json!({"call_id": "call-1"}),
            )],
        ));
        let load = sole_effect(&read);
        assert_eq!(load.tag(), EffectKindTag::LoadPayload);
        let EffectKind::LoadPayload(effect) = &load.effect else {
            panic!("expected a payload load");
        };
        assert_eq!(effect.handle_id.as_str(), "call-1");
        assert_eq!(
            effect.payload_ref.as_str(),
            "payload:01J8Y2QK7C4N0V",
            "the effect hands back the host's own opaque locator, unread"
        );
        let load = load.effect_id.clone();
        assert!(
            rejections(&runtime).is_empty(),
            "a reachable, externally-backed handle is a page-in, not a refusal"
        );

        let restored = runtime.submit(&resolved(
            "in-loaded",
            1_700_000_005_000,
            &load,
            EffectSuccess::PayloadLoaded(PayloadLoadedSuccess {
                handle_id: HandleId::new("call-1").unwrap(),
                payload: InlinePayload {
                    content: BODY.to_string(),
                    digest: body_digest(),
                    original_size: WireU64::new(BODY.len() as u64),
                },
            }),
        ));
        assert_eq!(
            kinds(&restored),
            vec![EffectKindTag::CallProvider],
            "the paged-in body resumes the turn that asked for it"
        );

        let engine = runtime.driver.engine().expect("the arc built an engine");
        assert_eq!(
            engine.ctx.payload_residency("call-1"),
            Some(&Residency::Resident),
            "§25.9 · the handle is the fact, and it says the body came home"
        );
        let rendered = serde_json::to_string(&engine.ctx.partitions.history.messages).unwrap();
        assert!(
            rendered.contains("clears the inline threshold"),
            "the model reads the body it asked for"
        );
        assert!(observation_kinds(&runtime).contains(&"payload_residency_changed"));
    }

    /// The kernel never saw the body, so the digest is the only evidence there is. A mismatch is a
    /// protocol violation with zero mutation, not a degraded read.
    #[test]
    fn a_paged_in_body_that_is_not_the_one_that_left_is_refused() {
        let (mut runtime, load) = agent_awaiting_payload_load();
        // Same length, and self-consistently declared — only the digest can tell the difference,
        // which is exactly the property the contract rests on.
        let substitute = BODY.replace("margin", "MARGIN");
        assert_eq!(substitute.len(), BODY.len());
        let fault = runtime.reject(&resolved(
            "in-loaded",
            1_700_000_005_000,
            &load,
            EffectSuccess::PayloadLoaded(PayloadLoadedSuccess {
                handle_id: HandleId::new("call-1").unwrap(),
                payload: InlinePayload {
                    content: substitute.to_string(),
                    digest: super::super::record::canonical_digest(substitute.as_bytes()),
                    original_size: WireU64::new(substitute.len() as u64),
                },
            }),
        ));
        assert_eq!(fault.code, KernelFaultCode::UnexpectedEffectOutcome);
        assert!(fault.message.contains("digests to"), "{}", fault.message);

        let engine = runtime.driver.engine().expect("the arc built an engine");
        assert!(
            matches!(
                engine.ctx.payload_residency("call-1"),
                Some(Residency::External { .. })
            ),
            "a refused restore leaves the handle exactly where it was"
        );
    }

    /// A loaded payload that does not agree with itself never gets as far as the digest: the size
    /// it declares and the bytes it carries are the same claim stated twice.
    #[test]
    fn a_paged_in_body_that_contradicts_its_own_size_is_refused() {
        let (mut runtime, load) = agent_awaiting_payload_load();
        let fault = runtime.reject(&resolved(
            "in-loaded",
            1_700_000_005_000,
            &load,
            EffectSuccess::PayloadLoaded(PayloadLoadedSuccess {
                handle_id: HandleId::new("call-1").unwrap(),
                payload: InlinePayload {
                    content: BODY.to_string(),
                    digest: body_digest(),
                    original_size: WireU64::new(BODY.len() as u64 + 1),
                },
            }),
        ));
        assert_eq!(fault.code, KernelFaultCode::UnexpectedEffectOutcome);
        assert!(fault.message.contains("and carries"), "{}", fault.message);
    }

    /// The outcome must name the handle its effect addressed — the same rule `verify_page_out_receipt`
    /// applies on the way out.
    #[test]
    fn a_paged_in_body_for_another_handle_is_refused() {
        let (mut runtime, load) = agent_awaiting_payload_load();
        let fault = runtime.reject(&resolved(
            "in-loaded",
            1_700_000_005_000,
            &load,
            EffectSuccess::PayloadLoaded(PayloadLoadedSuccess {
                handle_id: HandleId::new("call-9").unwrap(),
                payload: InlinePayload {
                    content: BODY.to_string(),
                    digest: body_digest(),
                    original_size: WireU64::new(BODY.len() as u64),
                },
            }),
        ));
        assert_eq!(fault.code, KernelFaultCode::UnexpectedEffectOutcome);
        assert!(fault.message.contains("names handle"), "{}", fault.message);
    }

    /// DEC-5 · a body the host cannot produce leaves the operation where it was. The read is
    /// abandoned once, the loop continues, and the kernel does not re-issue the same load.
    #[test]
    fn a_payload_the_host_cannot_produce_abandons_the_read() {
        let (mut runtime, load) = agent_awaiting_payload_load();
        let resumed = runtime.submit(&failed(
            "in-load-failed",
            1_700_000_005_000,
            &load,
            HostEffectFailureKind::StorageUnavailable,
            "the blob store is offline",
        ));
        assert_eq!(
            kinds(&resumed),
            vec![EffectKindTag::CallProvider],
            "one page-in failure does not kill a live run"
        );
        assert!(observation_kinds(&runtime).contains(&"payload_load_failed"));
        let engine = runtime.driver.engine().expect("the arc built an engine");
        assert!(
            matches!(
                engine.ctx.payload_residency("call-1"),
                Some(Residency::External { .. })
            ),
            "the reference survives the failed read: the body is still out there"
        );
    }

    /// An address whose body core still holds is refused with the reason. There is no locator to
    /// hand back, and inventing one is the confusion the closed union removes.
    #[test]
    fn a_page_in_of_a_resident_handle_is_refused() {
        let (mut runtime, tools) = agent_awaiting_tool_results();
        let inlined = runtime.submit(&tools_resolved(
            "in-results",
            1_700_000_003_000,
            &tools,
            &[("call-1", "three sources found", false)],
        ));
        runtime.submit(&provider_result(
            "in-read",
            1_700_000_004_000,
            &effect_id(inlined.step_seq),
            vec![tool_call(
                "call-2",
                READ_RESULT_TOOL_NAME,
                json!({"call_id": "call-1"}),
            )],
        ));
        let rejected = rejections(&runtime);
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0].0, "read_result");
        assert!(
            rejected[0].2.contains("nothing to page in"),
            "got {:?}",
            rejected[0].2
        );
        assert_eq!(
            runtime.pending_effect_kinds(),
            vec![EffectKindTag::CallProvider],
            "a body core already holds publishes no load effect"
        );
    }

    /// B19 · the other half of the axis. A page-out archive becomes a `PagedOut` handle — a
    /// *different* state from `External`, reached from `Resident` — and the model can read it back
    /// through the same `LoadPayload` effect.
    #[test]
    fn a_page_out_archive_becomes_a_readable_paged_out_handle() {
        let (mut runtime, archive) = agent_awaiting_page_out();
        let EffectKind::ArchivePageOut(published) = &runtime
            .tx
            .pending_effects()
            .find(|effect| effect.effect_id == archive)
            .expect("the archive is pending")
            .effect
        else {
            panic!("expected a page-out archive");
        };
        let handle_id = published.handle_id.clone();
        let digest = published.payload.digest.clone();
        let content = published.payload.content.clone();
        let original_size = published.payload.original_size;

        let archived = runtime.submit(&resolved(
            "in-archived",
            1_700_000_003_000,
            &archive,
            EffectSuccess::PageOutArchived(super::super::effect::PageOutArchivedSuccess {
                receipt: ArchiveReceipt {
                    handle_id: handle_id.clone(),
                    payload_ref: PayloadRef::new("payload:archive-1").unwrap(),
                    digest: digest.clone(),
                    original_size,
                },
            }),
        ));
        let engine = runtime.driver.engine().expect("the arc built an engine");
        assert_eq!(
            engine.ctx.payload_residency(handle_id.as_str()),
            Some(&Residency::PagedOut {
                payload_ref: "payload:archive-1".to_string(),
                digest: digest.as_str().to_string(),
            }),
            "an archived body is paged out, never external — it *was* resident"
        );
        assert!(observation_kinds(&runtime).contains(&"payload_residency_changed"));

        // and the same read_result path addresses it
        let read = runtime.submit(&provider_result(
            "in-read",
            1_700_000_004_000,
            &effect_id(archived.step_seq),
            vec![tool_call(
                "call-7",
                READ_RESULT_TOOL_NAME,
                json!({ "call_id": handle_id.as_str() }),
            )],
        ));
        let load = sole_effect(&read);
        assert_eq!(load.tag(), EffectKindTag::LoadPayload);
        let load = load.effect_id.clone();

        runtime.submit(&resolved(
            "in-loaded",
            1_700_000_005_000,
            &load,
            EffectSuccess::PayloadLoaded(PayloadLoadedSuccess {
                handle_id: handle_id.clone(),
                payload: InlinePayload {
                    content: content.clone(),
                    digest,
                    original_size,
                },
            }),
        ));
        let engine = runtime.driver.engine().expect("the arc built an engine");
        assert_eq!(
            engine.ctx.payload_residency(handle_id.as_str()),
            Some(&Residency::Resident),
            "the archived history came home through the same effect the external body uses"
        );
    }

    /// An agent holding one external payload, with a page-in of it pending.
    fn agent_awaiting_payload_load() -> (Runtime, EffectId) {
        let (mut runtime, tools) = agent_awaiting_tool_results();
        let stored = runtime.submit(&payloads_resolved(
            "in-results",
            1_700_000_003_000,
            &tools,
            vec![external_payload(
                "call-1",
                body_digest(),
                BODY.len() as u64,
                "the full report body, far la…",
            )],
        ));
        let read = runtime.submit(&provider_result(
            "in-read",
            1_700_000_004_000,
            &effect_id(stored.step_seq),
            vec![tool_call(
                "call-2",
                READ_RESULT_TOOL_NAME,
                json!({"call_id": "call-1"}),
            )],
        ));
        (runtime, sole_effect(&read).effect_id.clone())
    }

    // -----------------------------------------------------------------------------------------
    // fixture: removed-large-result-spool-effect
    // -----------------------------------------------------------------------------------------

    /// §7.10 rules 5 and 6 / §25.10 · the body does not cross core, in either direction.
    ///
    /// The scan is over everything the arc made durable or published: no record, no effect and no
    /// observation carries the persisted body, and no effect kind exists through which the kernel
    /// could hand a body back out to be persisted. The full output enters only through a verified
    /// host-owned payload reference.
    #[test]
    fn no_canonical_record_effect_or_observation_carries_an_external_body() {
        let (mut runtime, tools) = agent_awaiting_tool_results();
        runtime.submit(&payloads_resolved(
            "in-results",
            1_700_000_003_000,
            &tools,
            vec![external_payload(
                "call-1",
                body_digest(),
                BODY.len() as u64,
                "the full report body, far la…",
            )],
        ));

        // The record's canonical input is a base64 envelope in JSON, so scanning the record bytes
        // alone would pass for free. Every accepted input is decoded back out and scanned as text.
        let mut surfaces: Vec<(String, String)> = Vec::new();
        for record in &runtime.journal {
            surfaces.push((
                "record".to_string(),
                String::from_utf8_lossy(record.record_bytes().as_slice()).into_owned(),
            ));
            surfaces.push((
                "accepted input".to_string(),
                serde_json::to_string(&record.normalized_input().expect("the record decodes"))
                    .unwrap(),
            ));
        }
        for effect in runtime.tx.pending_effects() {
            surfaces.push(("effect".to_string(), serde_json::to_string(effect).unwrap()));
        }
        for observation in runtime.observations() {
            surfaces.push((
                "observation".to_string(),
                serde_json::to_string(observation).unwrap(),
            ));
        }
        assert!(surfaces.len() >= 4, "the arc produced nothing to scan");
        for (surface, text) in &surfaces {
            assert!(
                !text.contains("clears the inline threshold"),
                "the canonical {surface} carries the externalised body: {text}"
            );
            assert!(
                !text.contains("spool"),
                "the canonical {surface} still speaks of spooling: {text}"
            );
        }

        // and no effect kind can express handing a body back out to be written
        for tag in EffectKindTag::ALL {
            assert!(
                !tag.as_str().contains("spool"),
                "{} would be the round trip §7.10 deletes",
                tag.as_str()
            );
        }
    }

    // -----------------------------------------------------------------------------------------
    // fixture: removed-session-log-payload-lookup
    // -----------------------------------------------------------------------------------------

    /// §7.10 rules 4 and 7 · a page-in addresses the handle table and nothing else.
    ///
    /// The historical `read_result` resolved by scanning a spool directory and then falling back to
    /// a linear walk of the SessionLog, so any path-shaped string was a readable address and a body
    /// that had left the table was still reachable. Here the two refusals are total: an address the
    /// caller does not hold, and a locator-shaped string that is not an address at all. Neither
    /// produces an effect, so there is no lookup to fall back *to*.
    #[test]
    fn a_page_in_cannot_address_anything_outside_the_handle_table() {
        let (mut runtime, tools) = agent_awaiting_tool_results();
        let stored = runtime.submit(&payloads_resolved(
            "in-results",
            1_700_000_003_000,
            &tools,
            vec![external_payload(
                "call-1",
                body_digest(),
                BODY.len() as u64,
                "the full report body, far la…",
            )],
        ));
        // the locator the host chose is *not* an address: only the handle is
        runtime.submit(&provider_result(
            "in-read",
            1_700_000_004_000,
            &effect_id(stored.step_seq),
            vec![tool_call(
                "call-2",
                READ_RESULT_TOOL_NAME,
                json!({"call_id": "payload:01J8Y2QK7C4N0V"}),
            )],
        ));
        let rejected = rejections(&runtime);
        assert_eq!(rejected.len(), 1);
        assert!(
            rejected[0].2.contains("not reachable"),
            "got {:?}",
            rejected[0].2
        );
        assert_eq!(
            runtime.pending_effect_kinds(),
            vec![EffectKindTag::CallProvider],
            "no effect is published for an address the table does not hold"
        );
    }

    // -----------------------------------------------------------------------------------------
    // determinism: the record chain replays to the same steps
    // -----------------------------------------------------------------------------------------

    #[test]
    fn the_journal_replays_to_byte_identical_records() {
        let runtime = drive_workflow_root_to_terminal();
        verify_record_chain(&runtime.journal).expect("the chain links up");

        let mut replay = CanonicalOperationDriver::new();
        let rebuilt: KernelTransaction<PlannedStep, InMemoryRecordIndex> =
            KernelTransaction::rebuild_from_records(
                &runtime.journal,
                ConfigDefaults::default(),
                InMemoryRecordIndex::new(),
                |context| replay.fold(context),
            )
            .expect("a deterministic driver rebuilds its own journal");

        assert_eq!(rebuilt.lifecycle(), OperationLifecycle::Completed);
        assert_eq!(rebuilt.terminal(), runtime.tx.terminal());
        assert_eq!(replay.root_kind(), runtime.driver.root_kind());
        assert_eq!(replay.focus(), runtime.driver.focus());
    }

    /// A journal that contains syscall transitions rebuilds to the same steps — the caller a
    /// request was attributed to, the causation it spent and the graph it grew are all kernel state
    /// derived from the records, not host state a resume has to reassemble (§10.3).
    #[test]
    fn a_journal_with_syscall_transitions_replays_to_byte_identical_records() {
        let mut runtime = workflow_root_awaiting_first_child();
        runtime.submit(&child_done_with(
            "in-done-1",
            1_700_000_003_000,
            "wf-node0",
            "wf-node0:attempt:1",
            vec![
                SyscallRequest::AppendWorkflowNodes(
                    super::super::syscall::AppendWorkflowNodesRequest {
                        nodes: vec![wire_node("verify", "verify", &[])],
                    },
                ),
                SyscallRequest::AppendWorkflowNodes(
                    super::super::syscall::AppendWorkflowNodesRequest {
                        nodes: vec![wire_node("orphan", "orphan", &["nowhere"])],
                    },
                ),
            ],
        ));
        verify_record_chain(&runtime.journal).expect("the chain links up");

        let mut replay = CanonicalOperationDriver::new();
        let rebuilt: KernelTransaction<PlannedStep, InMemoryRecordIndex> =
            KernelTransaction::rebuild_from_records(
                &runtime.journal,
                ConfigDefaults::default(),
                InMemoryRecordIndex::new(),
                |context| replay.fold(context),
            )
            .expect("a deterministic driver rebuilds its own syscall journal");

        assert_eq!(rebuilt.lifecycle(), runtime.tx.lifecycle());
        assert_eq!(replay.focus(), runtime.driver.focus());
        assert_eq!(
            replay.engine().unwrap().workflow_node_count(),
            runtime.driver.engine().unwrap().workflow_node_count(),
            "the appended node is a kernel fact the replay reproduces"
        );
        assert_eq!(
            replay.attempts, runtime.driver.attempts,
            "live attempts rebuild identically, so authority after a rebuild is the same"
        );
    }

    #[test]
    fn a_plan_that_never_commits_fails_closed_instead_of_drifting() {
        let mut runtime = Runtime::new();
        runtime.submit(&configure());

        // plan a start, then abandon it: the semantic kernel already moved under that plan
        let preparation = runtime.prepare(&agent_start("in-start", 1_700_000_001_000));
        let token = preparation.token().unwrap().clone();
        runtime.tx.abort(&token).expect("abort before the append");

        let fault = runtime
            .driver
            .plan(&PlanContext {
                input: &runtime.journal[0].normalized_input().unwrap(),
                step_seq: WireU64::new(1),
                previous_head: None,
                config: runtime.tx.config().unwrap(),
                resolving: None,
            })
            .expect_err("a discarded plan poisons the driver");
        assert_eq!(fault.code, KernelFaultCode::TransactionConflict);
        assert!(runtime.driver.poison().is_some(), "the driver is poisoned");
    }

    // -----------------------------------------------------------------------------------------
    // §7.9 · unified effect resolution (Task 12)
    //
    // Three slices, in the order the spec suggests: provider/tool, workflow/control,
    // memory/page-out. Each effect kind gets success, failure and replay.
    // -----------------------------------------------------------------------------------------

    use crate::runtime::kernel::wire::effect::{
        ApprovalSuccess, ArchiveReceipt, EffectFailed, HostEffectFailure, HostEffectFailureKind,
        InlinePayload, InlineToolResult, MemoryPersistReceipt, MemoryPersistedSuccess,
        MemoryQueriedSuccess, MemoryRecall, MilestoneCheckResult as WireMilestoneResult,
        MilestoneEvaluatedSuccess, PageOutArchivedSuccess, PayloadLoadedSuccess,
        ProviderContextOverflow, ProviderStopReason, TaskAlreadyFinished, TaskPreemptOutcome,
        TaskPreemptStatus, TaskPreempted, TasksPreemptedSuccess, ToolResult as WireToolResult,
        ToolsSuccess,
    };
    use crate::runtime::kernel::wire::scalar::{CallId, HandleId};
    use crate::runtime::kernel::wire::syscall::MemoryKind as SyscallMemoryKind;
    use crate::runtime::kernel::wire::{Digest, MemoryRecordRef, PayloadRef};

    /// The support set `syscall_config` declares, plus whatever an arc additionally needs. Adding
    /// rather than replacing keeps §7.3's cross-field rules satisfied (a declared tool catalog
    /// implies `execute_tools` + `load_payload`).
    fn support_with(extra: impl IntoIterator<Item = EffectKindTag>) -> HostEffectSupport {
        HostEffectSupport::new(
            [
                EffectKindTag::CallProvider,
                EffectKindTag::ExecuteTools,
                EffectKindTag::LoadPayload,
                EffectKindTag::SpawnTasks,
                EffectKindTag::PreemptTasks,
                EffectKindTag::PersistMemory,
                EffectKindTag::QueryMemory,
            ]
            .into_iter()
            .chain(extra),
        )
    }

    fn resolved(id: &str, at: u64, effect: &EffectId, result: EffectSuccess) -> WireEnvelope {
        envelope(
            id,
            at,
            KernelInput::ResolveEffect(ResolveEffect {
                effect_id: effect.clone(),
                outcome: EffectOutcome::Succeeded(EffectSucceeded { result }),
            }),
        )
    }

    fn failed(
        id: &str,
        at: u64,
        effect: &EffectId,
        kind: HostEffectFailureKind,
        message: &str,
    ) -> WireEnvelope {
        envelope(
            id,
            at,
            KernelInput::ResolveEffect(ResolveEffect {
                effect_id: effect.clone(),
                outcome: EffectOutcome::Failed(EffectFailed {
                    failure: HostEffectFailure {
                        kind,
                        message: message.to_string(),
                        retryable: None,
                    },
                }),
            }),
        )
    }

    /// A provider turn that finished with plain text and no tool calls.
    fn provider_answer(id: &str, at: u64, effect: &EffectId, text: &str) -> WireEnvelope {
        resolved(
            id,
            at,
            effect,
            EffectSuccess::Provider(super::super::effect::ProviderSuccess {
                outcome: ProviderOutcome::Completed(ProviderCompleted {
                    message: ProviderMessage {
                        role: MessageRole::Assistant,
                        content: text.to_string(),
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                        tokens: None,
                    },
                    observed_input_tokens: None,
                    observed_output_tokens: None,
                    stop_reason: Some(ProviderStopReason::EndTurn),
                }),
            }),
        )
    }

    fn provider_overflow(id: &str, at: u64, effect: &EffectId) -> WireEnvelope {
        resolved(
            id,
            at,
            effect,
            EffectSuccess::Provider(super::super::effect::ProviderSuccess {
                outcome: ProviderOutcome::ContextOverflow(ProviderContextOverflow {
                    observed_input_tokens: Some(999_999),
                }),
            }),
        )
    }

    fn tools_resolved(
        id: &str,
        at: u64,
        effect: &EffectId,
        results: &[(&str, &str, bool)],
    ) -> WireEnvelope {
        resolved(
            id,
            at,
            effect,
            EffectSuccess::Tools(ToolsSuccess {
                results: results
                    .iter()
                    .map(|(call_id, output, is_error)| {
                        WireToolResultPayload::Inline(InlineToolResult {
                            call_id: CallId::new(*call_id).unwrap(),
                            result: WireToolResult {
                                output: (*output).to_string(),
                                durable_content: None,
                                is_error: *is_error,
                                disposition: ToolResultDisposition::Recoverable,
                                tokens: None,
                            },
                        })
                    })
                    .collect(),
            }),
        )
    }

    fn kinds(committed: &CommittedTransition<PlannedStep>) -> Vec<EffectKindTag> {
        committed
            .published_effects()
            .iter()
            .map(|effect| effect.tag())
            .collect()
    }

    /// Every observation the last committed transition recorded, by variant name.
    fn observation_kinds(runtime: &Runtime) -> Vec<&'static str> {
        runtime
            .observations()
            .iter()
            .map(observation_label)
            .collect()
    }

    fn observation_label(observation: &KernelObservation) -> &'static str {
        match observation {
            KernelObservation::MemoryWritten { .. } => "memory_written",
            KernelObservation::MemoryWriteFailed { .. } => "memory_write_failed",
            KernelObservation::MemoryQueried { .. } => "memory_queried",
            KernelObservation::MemoryQueryFailed { .. } => "memory_query_failed",
            KernelObservation::PageOutArchived { .. } => "page_out_archived",
            KernelObservation::PageOutArchiveFailed { .. } => "page_out_archive_failed",
            KernelObservation::ApprovalResolutionFailed { .. } => "approval_resolution_failed",
            KernelObservation::AgentPreemptFailed { .. } => "agent_preempt_failed",
            KernelObservation::ControlRequestRejected { .. } => "control_request_rejected",
            KernelObservation::Compressed { .. } => "compressed",
            KernelObservation::WorkflowBatchSpawned { .. } => "workflow_batch_spawned",
            KernelObservation::WorkflowCompleted { .. } => "workflow_completed",
            KernelObservation::MilestoneAdvanced { .. } => "milestone_advanced",
            KernelObservation::MilestoneBlocked { .. } => "milestone_blocked",
            KernelObservation::Resumed { .. } => "resumed",
            KernelObservation::Suspended { .. } => "suspended",
            KernelObservation::SignalDeliveryDisposed { .. } => "signal_delivery_disposed",
            KernelObservation::SignalDisplaced { .. } => "signal_displaced",
            KernelObservation::SignalExpired { .. } => "signal_expired",
            KernelObservation::SignalsPending { .. } => "signals_pending",
            KernelObservation::OperationCancelled { .. } => "operation_cancelled",
            KernelObservation::LivePolicyChanged { .. } => "live_policy_changed",
            KernelObservation::CapabilityChanged { .. } => "capability_changed",
            KernelObservation::AgentPreempted { .. } => "agent_preempted",
            KernelObservation::PayloadResidencyChanged { .. } => "payload_residency_changed",
            KernelObservation::PayloadLoadFailed { .. } => "payload_load_failed",
            _ => "other",
        }
    }

    /// The `(disposition, signal_id, delivery_id, attempt)` of every delivery the last transition
    /// disposed of.
    fn dispositions(runtime: &Runtime) -> Vec<(String, String, String, u32)> {
        runtime
            .observations()
            .iter()
            .filter_map(|observation| match observation {
                KernelObservation::SignalDeliveryDisposed {
                    disposition,
                    signal_id,
                    delivery_id,
                    attempt,
                    ..
                } => Some((
                    disposition.clone(),
                    signal_id.clone(),
                    delivery_id.clone(),
                    *attempt,
                )),
                _ => None,
            })
            .collect()
    }

    /// The rendered history of the operation, as text — what the model will read next turn.
    fn history_text(runtime: &Runtime) -> Vec<String> {
        runtime
            .driver
            .engine()
            .map(|engine| {
                engine
                    .ctx
                    .partitions
                    .history
                    .messages
                    .iter()
                    .map(|message| format!("{:?}:{}", message.role, message_text(message)))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn message_text(message: &Message) -> String {
        match &message.content {
            Content::Text(text) => text.clone(),
            Content::Parts(parts) => parts
                .iter()
                .map(|part| match part {
                    crate::types::message::ContentPart::ToolResult { output, .. } => output.clone(),
                    other => format!("{other:?}"),
                })
                .collect::<Vec<_>>()
                .join(" "),
        }
    }

    // ----- slice 1 · provider / tool -----------------------------------------------------------

    #[test]
    fn a_provider_turn_with_host_tool_calls_publishes_execute_tools() {
        let (mut runtime, provider) = agent_awaiting_provider();
        let dispatched = runtime.submit(&provider_result(
            "in-search",
            1_700_000_002_000,
            &provider,
            vec![tool_call("call-1", "search", json!({"q": "sources"}))],
        ));
        assert_eq!(kinds(&dispatched), vec![EffectKindTag::ExecuteTools]);
        let EffectKind::ExecuteTools(execute) = &sole_effect(&dispatched).effect else {
            panic!("expected a tool batch");
        };
        assert_eq!(execute.calls.len(), 1);
        assert_eq!(execute.calls[0].name, "search");
        assert_eq!(execute.calls[0].call_id.as_str(), "call-1");

        // and the results resolve that effect and re-ask the provider — the ordinary turn cycle
        let resumed = runtime.submit(&tools_resolved(
            "in-results",
            1_700_000_003_000,
            &effect_id(dispatched.step_seq),
            &[("call-1", "three sources found", false)],
        ));
        assert_eq!(kinds(&resumed), vec![EffectKindTag::CallProvider]);
        assert!(
            history_text(&runtime)
                .iter()
                .any(|line| line.contains("three sources found")),
            "the tool output is what the next turn reads"
        );
    }

    #[test]
    fn a_provider_answer_with_no_tool_calls_commits_the_agent_terminal() {
        let (mut runtime, provider) = agent_awaiting_provider();
        let finished = runtime.submit(&provider_answer(
            "in-final",
            1_700_000_002_000,
            &provider,
            "the brief is written",
        ));
        let terminal = finished.terminal().expect("a final answer terminates");
        let KernelTerminal::Agent(agent) = terminal else {
            panic!("expected an agent terminal, got {terminal:?}");
        };
        assert_eq!(agent.result.termination, WireTermination::Completed);
        assert_eq!(
            agent.result.final_message.as_ref().unwrap().content,
            "the brief is written"
        );
        assert!(
            finished.published_effects().is_empty(),
            "§7.12 · a terminal step publishes no effect"
        );
    }

    // fixture: budget-usage-reported-once-at-terminal
    #[test]
    fn the_usage_report_rides_the_terminal_and_only_the_terminal() {
        let (mut runtime, provider) = agent_awaiting_provider();
        let finished = runtime.submit(&provider_answer(
            "in-final",
            1_700_000_002_000,
            &provider,
            "done",
        ));
        let KernelTerminal::Agent(agent) = finished.terminal().unwrap() else {
            panic!("expected an agent terminal");
        };
        let reported = agent.usage.clone();

        // Replaying the very input that committed the terminal answers with the same record and
        // does not mint a second report.
        let replayed = runtime.prepare(&provider_answer(
            "in-final",
            1_700_000_002_000,
            &provider,
            "done",
        ));
        let RecordPreparation::Replayed(replay) = replayed else {
            panic!("an exact replay is a replay, not a new step");
        };
        let Some(committed_step) = &replay.committed_step else {
            panic!("a replay above the checkpoint floor carries its step");
        };
        let StepDisposition::Terminal(terminal) = &committed_step.disposition else {
            panic!("the replayed step is the terminal one");
        };
        let KernelTerminal::Agent(replayed_agent) = &terminal.terminal else {
            panic!("expected an agent terminal");
        };
        assert_eq!(replayed_agent.usage, reported, "one report, one terminal");
        assert_eq!(replay.step_seq, finished.step_seq);
    }

    // fixture: agent-syscall-caller-is-derived (§5k · the syscall-only continuation)
    #[test]
    fn a_syscall_only_turn_continues_with_another_provider_call() {
        let (mut runtime, provider) = agent_awaiting_provider();
        let continued = runtime.submit(&provider_result(
            "in-plan",
            1_700_000_002_000,
            &provider,
            vec![
                tool_call("call-1", "skill", json!({"name": "debug"})),
                tool_call(
                    "call-2",
                    "update_plan",
                    json!({"progress": "sources listed"}),
                ),
            ],
        ));

        assert_eq!(
            kinds(&continued),
            vec![EffectKindTag::CallProvider],
            "a pure control-plane batch publishes no effect of its own, so the kernel continues \
             the turn itself rather than leaving the operation with nothing outstanding"
        );
        let history = history_text(&runtime);
        assert!(
            history.iter().any(|line| line.contains("skill activated")),
            "every syscall the kernel executed is answered so the transcript pairs: {history:?}"
        );
        assert!(
            history.iter().any(|line| line.contains("plan updated")),
            "{history:?}"
        );
        // and the assistant turn the model emitted is in history verbatim, calls included
        assert!(
            runtime
                .driver
                .engine()
                .unwrap()
                .ctx
                .partitions
                .history
                .messages
                .iter()
                .any(|message| message.tool_calls.len() == 2),
            "the model reads back the turn it actually emitted"
        );
    }

    #[test]
    fn a_syscall_batch_that_published_an_effect_waits_instead_of_re_asking() {
        let (mut runtime, provider) = agent_awaiting_provider();
        let queried = runtime.submit(&provider_result(
            "in-memory",
            1_700_000_002_000,
            &provider,
            vec![tool_call(
                "call-1",
                crate::context::manager::MEMORY_TOOL_NAME,
                json!({"query": "prior briefs"}),
            )],
        ));
        assert_eq!(
            kinds(&queried),
            vec![EffectKindTag::QueryMemory],
            "the turn resumes when the recall it asked for resolves; a provider call now would \
             race the very facts it was issued to read"
        );
    }

    #[test]
    fn a_mixed_batch_adjudicates_the_syscall_and_dispatches_the_tool() {
        let (mut runtime, provider) = agent_awaiting_provider();
        let mixed = runtime.submit(&provider_result(
            "in-mixed",
            1_700_000_002_000,
            &provider,
            vec![
                tool_call("call-1", "skill", json!({"name": "debug"})),
                tool_call("call-2", "search", json!({"q": "x"})),
            ],
        ));
        assert_eq!(kinds(&mixed), vec![EffectKindTag::ExecuteTools]);
        let EffectKind::ExecuteTools(execute) = &sole_effect(&mixed).effect else {
            panic!("expected a tool batch");
        };
        assert_eq!(
            execute
                .calls
                .iter()
                .map(|call| call.name.as_str())
                .collect::<Vec<_>>(),
            vec!["search"],
            "a P1 syscall is never dispatched to a host executor"
        );
        assert!(
            history_text(&runtime)
                .iter()
                .any(|line| line.contains("skill activated")),
            "the syscall half still closes its own transcript pair"
        );
    }

    // fixture: fault-effect-resolution-fails-closed
    #[test]
    fn a_context_overflow_compacts_and_re_asks_without_reading_vendor_text() {
        let mut runtime = Runtime::new();
        runtime.submit(&syscall_config());
        let started = runtime.submit(&agent_start_with_history("in-start", 1_700_000_001_000, 14));
        let provider = sole_effect(&started).effect_id.clone();

        let recovered = runtime.submit(&provider_overflow(
            "in-overflow",
            1_700_000_002_000,
            &provider,
        ));
        assert_eq!(
            kinds(&recovered),
            vec![EffectKindTag::CallProvider],
            "an overflow is a semantic outcome the kernel recovers from, not a transport failure"
        );
        assert!(
            observation_kinds(&runtime).contains(&"compressed"),
            "the recovery ladder ran: {:?}",
            observation_kinds(&runtime)
        );
    }

    #[test]
    fn canonical_genesis_installs_the_entropy_watch_on_the_engine() {
        let mut runtime = Runtime::new();
        runtime.submit(&syscall_config_with(|config| {
            config.execution_policy = Some(ExecutionPolicy {
                max_turns: Some(12),
                entropy_watch: Some(EntropyWatchPolicy {
                    enabled: Some(true),
                    threshold_ppm: Some(Ppm::new(100_000).unwrap()),
                    hysteresis_ppm: Some(Ppm::ZERO),
                    cooldown_turns: Some(0),
                    notify_model: Some(true),
                }),
                ..ExecutionPolicy::default()
            });
        }));
        let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));
        let acted = runtime.submit(&provider_result(
            "in-acted",
            1_700_000_002_000,
            &effect_id(started.step_seq),
            vec![tool_call("call-1", "search", json!({ "q": "same" }))],
        ));
        let resolved = runtime.submit(&tools_resolved(
            "in-results",
            1_700_000_003_000,
            &effect_id(acted.step_seq),
            &[("call-1", "failed", true)],
        ));

        assert!(
            resolved
                .step
                .observations
                .iter()
                .any(|observation| matches!(observation, KernelObservation::EntropyAlert { .. })),
            "the resolved canonical execution policy must arm the semantic engine's entropy watch"
        );
    }

    /// **Task 14 · core does not parse a raw vendor error string.**
    ///
    /// The legacy path classified overflow by substring-matching provider prose
    /// (`state_machine/eviction.rs::is_prompt_too_long`, §22.8). On the canonical face that
    /// reading has no entry point: the same words are ordinary content in a completion, ordinary
    /// diagnostics in a failure message, and neither reaches a recovery decision. Only the typed
    /// `ContextOverflow` outcome does.
    #[test]
    fn vendor_error_prose_is_content_and_never_a_recovery_decision() {
        const VENDOR_PROSE: &str = "HTTP 413: prompt is too long — context_length_exceeded, \
                                    maximum context length is 128000 tokens";

        // (a) as the model's own words: an ordinary completed turn
        let mut runtime = Runtime::new();
        runtime.submit(&syscall_config());
        let started = runtime.submit(&agent_start_with_history("in-start", 1_700_000_001_000, 14));
        let answered = runtime.submit(&provider_answer(
            "in-prose",
            1_700_000_002_000,
            &sole_effect(&started).effect_id.clone(),
            VENDOR_PROSE,
        ));
        assert!(
            answered.terminal().is_some() || kinds(&answered) == vec![EffectKindTag::CallProvider],
            "the words are content, not a classification"
        );
        assert!(
            !observation_kinds(&runtime).contains(&"compressed"),
            "no recovery ladder ran: {:?}",
            observation_kinds(&runtime)
        );

        // (b) as a host failure message: a failed terminal, not an overflow recovery
        let (mut runtime, provider) = agent_awaiting_provider();
        let ended = runtime.submit(&failed(
            "in-prose-failure",
            1_700_000_002_000,
            &provider,
            HostEffectFailureKind::TransportExhausted,
            VENDOR_PROSE,
        ));
        let Some(KernelTerminal::Failed(failure)) = ended.terminal() else {
            panic!("expected a failed terminal, got {:?}", ended.terminal());
        };
        assert_eq!(failure.failure.code, KernelFailureCode::HostEffectFailed);
        assert!(
            !observation_kinds(&runtime).contains(&"compressed"),
            "a failure message is diagnostics; it never selects the recovery ladder: {:?}",
            observation_kinds(&runtime)
        );
    }

    /// **Task 14 · no vendor vocabulary survives anywhere on the canonical face.**
    ///
    /// Every closed vocabulary the wire publishes, scanned against the words hosts historically
    /// forwarded verbatim. This is the regression guard for "the host maps, core never learns":
    /// adding a `rate_limited` failure class or a `stop` stop-reason would break it here rather
    /// than three releases later when something starts branching on it.
    #[test]
    fn no_canonical_vocabulary_contains_a_vendor_word() {
        const VENDOR_WORDS: [&str; 16] = [
            "rate_limit",
            "429",
            "503",
            "413",
            "overloaded",
            // `storage_unavailable` is the deliberate near-miss: a *storage* classification, not a
            // vendor's "service unavailable" — which folds into `transport_exhausted` once the
            // host's own ladder is spent. So the banned word is the vendor's compound, not the
            // bare adjective.
            "service_unavailable",
            "context_length",
            "max_context",
            "too_long",
            "finish_reason",
            "openai",
            "anthropic",
            "gemini",
            "deepseek",
            "qwen",
            "minimax",
        ];

        let mut vocabulary: Vec<&'static str> = Vec::new();
        vocabulary.extend(EffectKindTag::ALL.iter().map(|tag| tag.as_str()));
        vocabulary.extend(
            super::super::effect::EffectSuccessTag::ALL
                .iter()
                .map(|tag| tag.as_str()),
        );
        vocabulary.extend(HostEffectFailureKind::ALL.iter().map(|kind| kind.as_str()));
        vocabulary.extend(ProviderStopReason::ALL.iter().map(|r| r.as_str()));
        vocabulary.extend(ToolResultDisposition::ALL.iter().map(|d| d.as_str()));
        assert!(vocabulary.len() >= 34, "the scan lost a vocabulary");

        for word in vocabulary {
            for vendor in VENDOR_WORDS {
                assert!(
                    !word.contains(vendor),
                    "{word:?} carries the vendor word {vendor:?}; the canonical face is the \
                     host's mapping *target*, never its passthrough"
                );
            }
        }

        // `storage_unavailable` is the one near-miss and it is deliberate: it is a *storage*
        // classification, not a vendor's "service unavailable" — which folds into
        // `transport_exhausted` after the host's own ladder is spent.
        assert!(
            HostEffectFailureKind::ALL
                .iter()
                .any(|kind| kind.as_str() == "storage_unavailable")
        );
        assert!(
            !HostEffectFailureKind::ALL
                .iter()
                .any(|kind| kind.as_str().contains("service"))
        );
    }

    #[test]
    fn the_overflow_ladder_is_bounded_and_ends_in_an_honest_terminal() {
        let mut runtime = Runtime::new();
        runtime.submit(&syscall_config_with(|config| {
            config.recovery_policy = Some(crate::runtime::kernel::wire::command::RecoveryPolicy {
                provider_recovery_attempts: Some(1),
                ..Default::default()
            });
        }));
        let started = runtime.submit(&agent_start_with_history("in-start", 1_700_000_001_000, 14));
        let mut provider = sole_effect(&started).effect_id.clone();

        let first = runtime.submit(&provider_overflow("in-of-1", 1_700_000_002_000, &provider));
        assert_eq!(kinds(&first), vec![EffectKindTag::CallProvider]);
        provider = sole_effect(&first).effect_id.clone();

        let exhausted = runtime.submit(&provider_overflow("in-of-2", 1_700_000_003_000, &provider));
        let KernelTerminal::Agent(agent) = exhausted.terminal().expect("the ladder is bounded")
        else {
            panic!("expected an agent terminal");
        };
        assert_eq!(agent.result.termination, WireTermination::ContextOverflow);
    }

    // fixture: removed-kernel-auto-redispatch
    #[test]
    fn a_provider_failure_commits_a_terminal_and_never_re_asks() {
        let (mut runtime, provider) = agent_awaiting_provider();
        let ended = runtime.submit(&failed(
            "in-dead",
            1_700_000_002_000,
            &provider,
            HostEffectFailureKind::TransportExhausted,
            "the vendor returned 503 five times",
        ));
        assert!(
            ended.published_effects().is_empty(),
            "DEC-5 · the kernel makes one policy decision and does not re-emit the same intent"
        );
        let KernelTerminal::Failed(failure) = ended.terminal().expect("a terminal was committed")
        else {
            panic!("expected a failed terminal, got {:?}", ended.terminal());
        };
        assert_eq!(failure.failure.code, KernelFailureCode::HostEffectFailed);
        assert!(
            failure.failure.message.contains("transport_exhausted"),
            "the classification decides; the prose is only what an operator reads: {}",
            failure.failure.message
        );
    }

    // fixture: removed-kernel-auto-redispatch
    #[test]
    fn a_tool_batch_failure_answers_every_dispatched_call_and_never_re_runs_it() {
        let (mut runtime, provider) = agent_awaiting_provider();
        let dispatched = runtime.submit(&provider_result(
            "in-search",
            1_700_000_002_000,
            &provider,
            vec![
                tool_call("call-1", "search", json!({"q": "a"})),
                tool_call("call-2", "search", json!({"q": "b"})),
            ],
        ));
        let tools = effect_id(dispatched.step_seq);

        let answered = runtime.submit(&failed(
            "in-exec-failed",
            1_700_000_003_000,
            &tools,
            HostEffectFailureKind::PermissionDenied,
            "the executor is not allowed to run tools in this sandbox",
        ));
        assert_eq!(
            kinds(&answered),
            vec![EffectKindTag::CallProvider],
            "the batch is abandoned and the model is asked again — the kernel never re-dispatches \
             the same batch"
        );
        let history = history_text(&runtime);
        let refusals = history
            .iter()
            .filter(|line| line.contains("permission_denied"))
            .count();
        assert_eq!(
            refusals, 2,
            "every dispatched call still gets a result: {history:?}"
        );
    }

    // ----- §7.10 · tool result disposition (Task 14) --------------------------------------------

    /// A batch of two calls, dispatched and awaiting results.
    fn agent_dispatching_two_tools() -> (Runtime, EffectId) {
        let (mut runtime, provider) = agent_awaiting_provider();
        let dispatched = runtime.submit(&provider_result(
            "in-search",
            1_700_000_002_000,
            &provider,
            vec![
                tool_call("call-1", "search", json!({"q": "a"})),
                tool_call("call-2", "search", json!({"q": "b"})),
            ],
        ));
        assert_eq!(kinds(&dispatched), vec![EffectKindTag::ExecuteTools]);
        let tools = effect_id(dispatched.step_seq);
        (runtime, tools)
    }

    fn tool_batch(
        id: &str,
        at: u64,
        effect: &EffectId,
        results: &[(&str, &str, bool, ToolResultDisposition)],
    ) -> WireEnvelope {
        resolved(
            id,
            at,
            effect,
            EffectSuccess::Tools(ToolsSuccess {
                results: results
                    .iter()
                    .map(|(call_id, output, is_error, disposition)| {
                        WireToolResultPayload::Inline(InlineToolResult {
                            call_id: CallId::new(*call_id).unwrap(),
                            result: WireToolResult {
                                output: (*output).to_string(),
                                durable_content: None,
                                is_error: *is_error,
                                disposition: *disposition,
                                tokens: None,
                            },
                        })
                    })
                    .collect(),
            }),
        )
    }

    #[test]
    fn a_fatal_result_closes_out_the_calls_its_batch_never_answered() {
        // `fatal` = the executor stopped. The unanswered call is not pending — it will never be
        // answered — so leaving it alone would produce an assistant turn whose tool_call has no
        // matching tool_result, which is malformed on every vendor wire.
        let (mut runtime, tools) = agent_dispatching_two_tools();
        let settled = runtime.submit(&tool_batch(
            "in-fatal",
            1_700_000_003_000,
            &tools,
            &[(
                "call-1",
                "disk corrupt, aborting",
                true,
                ToolResultDisposition::Fatal,
            )],
        ));
        assert_eq!(
            kinds(&settled),
            vec![EffectKindTag::CallProvider],
            "the turn still completes and the model is asked again — a fatal result is model \
             feedback, not a rollback (v0.2.42)"
        );

        let history = history_text(&runtime);
        assert!(
            history.iter().any(|line| line.contains("disk corrupt")),
            "the failure the host reported stays visible: {history:?}"
        );
        // The pairing invariant is the point: both dispatched calls end the turn with a result.
        let answered = tool_result_call_ids(&runtime);
        assert_eq!(
            answered,
            vec!["call-1".to_string(), "call-2".to_string()],
            "the call the batch never answered is closed out"
        );
        assert!(
            history
                .iter()
                .any(|line| line.contains("not executed") && line.contains("fatally")),
            "the close-out says why the call did not run: {history:?}"
        );
    }

    /// Every `call_id` that has a committed tool result in history, in order.
    fn tool_result_call_ids(runtime: &Runtime) -> Vec<String> {
        runtime
            .driver
            .engine()
            .map(|engine| {
                engine
                    .ctx
                    .partitions
                    .history
                    .messages
                    .iter()
                    .flat_map(|message| match &message.content {
                        Content::Parts(parts) => parts
                            .iter()
                            .filter_map(|part| match part {
                                crate::types::message::ContentPart::ToolResult {
                                    call_id, ..
                                } => Some(call_id.to_string()),
                                _ => None,
                            })
                            .collect::<Vec<_>>(),
                        Content::Text(_) => Vec::new(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn an_ordinary_batch_closes_out_nothing() {
        // The close-out is fatal-only. A short *recoverable* batch says nothing about the calls it
        // omitted, so the kernel must not invent results for them.
        let (mut runtime, tools) = agent_dispatching_two_tools();
        runtime.submit(&tool_batch(
            "in-partial",
            1_700_000_003_000,
            &tools,
            &[(
                "call-1",
                "no matches",
                false,
                ToolResultDisposition::Recoverable,
            )],
        ));
        let history = history_text(&runtime);
        assert!(
            !history.iter().any(|line| line.contains("not executed")),
            "a recoverable batch closes nothing out: {history:?}"
        );
    }

    /// §7.10 rule 9 · the close-out is **total over residency**.
    ///
    /// A tool that fails after producing a huge diagnostic body is the common shape, not a rare
    /// one. If the fatality scan read only the inline arm, whether a fatal failure stopped the
    /// batch would depend on how many bytes its traceback happened to be.
    #[test]
    fn an_externalised_fatal_stops_the_batch_exactly_as_an_inline_one_does() {
        let (mut runtime, tools) = agent_dispatching_two_tools();
        let settled = runtime.submit(&resolved(
            "in-fatal-external",
            1_700_000_003_000,
            &tools,
            EffectSuccess::Tools(ToolsSuccess {
                results: vec![external_payload_with(
                    "call-1",
                    Digest::new(
                        "sha256:3b1f4a7c9e2d05186a4c7f0b9d3e8c25714f6a0b8c5d2e9f1a3b6c8d0e2f4a61",
                    )
                    .unwrap(),
                    524_288,
                    "Traceback (most recent call last):",
                    true,
                    ToolResultDisposition::Fatal,
                )],
            }),
        ));
        assert_eq!(kinds(&settled), vec![EffectKindTag::CallProvider]);
        assert_eq!(
            tool_result_call_ids(&runtime),
            vec!["call-1".to_string(), "call-2".to_string()],
            "an externalised fatal closes the batch out just like an inline one"
        );
        let history = history_text(&runtime);
        assert!(
            history
                .iter()
                .any(|line| line.contains("not executed") && line.contains("fatally")),
            "{history:?}"
        );

        // …and the failure itself stays visible as an error, which the old shape could not express
        // at all: an externalised failure was indistinguishable from an externalised success.
        assert!(
            runtime
                .driver
                .engine()
                .expect("engine")
                .ctx
                .partitions
                .history
                .messages
                .iter()
                .any(
                    |message| matches!(&message.content, Content::Parts(parts) if parts
                    .iter()
                    .any(|part| matches!(
                        part,
                        crate::types::message::ContentPart::ToolResult {
                            call_id,
                            durable_content: None,
                            is_error: true,
                            ..
                        } if call_id.as_str() == "call-1"
                    )))
                ),
            "the externalised failure is committed as an error result"
        );
    }

    #[test]
    fn a_fatal_result_does_not_synthesise_an_answer_the_host_already_gave() {
        // The close-out is keyed on *unanswered* calls, so a complete fatal batch adds nothing —
        // otherwise a call would get two results.
        let (mut runtime, tools) = agent_dispatching_two_tools();
        runtime.submit(&tool_batch(
            "in-fatal-complete",
            1_700_000_003_000,
            &tools,
            &[
                ("call-1", "boom", true, ToolResultDisposition::Fatal),
                (
                    "call-2",
                    "ok anyway",
                    false,
                    ToolResultDisposition::Recoverable,
                ),
            ],
        ));
        let history = history_text(&runtime);
        assert!(
            !history.iter().any(|line| line.contains("not executed")),
            "every call was answered by the host: {history:?}"
        );
        assert_eq!(
            history
                .iter()
                .filter(|line| line.contains("ok anyway"))
                .count(),
            1,
            "no call gets two results: {history:?}"
        );
    }

    // ----- §7.9 · DEC-5 differential (Task 14) --------------------------------------------------

    /// **Recovery exhaustion differential.** The kernel's decision on a failure is chosen by the
    /// effect kind *it* published, never by the failure kind the host reports and never by
    /// `retryable`. The strongest statement of that is byte equality: for every one of the ten
    /// effect kinds, all six failure classes × three `retryable` values must plan the same step.
    ///
    /// If any of those ever became an input, this is the test that breaks — which is the point,
    /// because "the kernel does not retry" is otherwise only a comment.
    #[test]
    fn a_host_failure_plans_the_same_step_whatever_the_host_advises() {
        for kind in HostEffectFailureKind::ALL {
            let mut planned: Option<Value> = None;
            for retryable in [None, Some(true), Some(false)] {
                let (mut runtime, tools) = agent_dispatching_two_tools();
                let settled = runtime.submit(&envelope(
                    "in-failed",
                    1_700_000_003_000,
                    KernelInput::ResolveEffect(ResolveEffect {
                        effect_id: tools.clone(),
                        outcome: EffectOutcome::Failed(EffectFailed {
                            failure: HostEffectFailure {
                                kind,
                                // the message is fixed: only the advice varies
                                message: "the executor could not run".to_string(),
                                retryable,
                            },
                        }),
                    }),
                ));
                let step = serde_json::to_value(&settled.step).unwrap();
                match &planned {
                    None => planned = Some(step),
                    Some(first) => assert_eq!(
                        first, &step,
                        "{kind:?} with retryable={retryable:?} planned a different step; \
                         `retryable` is advice and DEC-5 leaves it no branch to select"
                    ),
                }
            }
        }
    }

    #[test]
    fn the_recovery_decision_reads_the_effect_kind_the_kernel_published() {
        // The other half of the same claim: the *kind* the host reports does not select the
        // decision either. Six wildly different failure classes on the same effect all produce the
        // one decision that effect kind gets — here, "answer every call and ask the model again".
        for kind in HostEffectFailureKind::ALL {
            let (mut runtime, tools) = agent_dispatching_two_tools();
            let settled = runtime.submit(&failed(
                "in-failed",
                1_700_000_003_000,
                &tools,
                kind,
                "the executor could not run",
            ));
            assert_eq!(
                kinds(&settled),
                vec![EffectKindTag::CallProvider],
                "{kind:?} must take the same decision as every other failure class"
            );
            assert!(
                runtime.pending_effect_kinds() == vec![EffectKindTag::CallProvider],
                "{kind:?}: the failed batch is never re-issued (DEC-5)"
            );
        }
    }

    // fixture: fault-effect-resolution-fails-closed
    #[test]
    fn duplicate_and_conflicting_resolutions_are_settled_before_the_driver_sees_them() {
        let (mut runtime, provider) = agent_awaiting_provider();
        let first = runtime.submit(&provider_result(
            "in-skill",
            1_700_000_002_000,
            &provider,
            vec![tool_call("call-1", "skill", json!({"name": "debug"}))],
        ));

        // the same resolution under a fresh input id is a replay of the existing record (DEC-1)
        let replay = runtime.prepare(&provider_result(
            "in-skill-again",
            1_700_000_002_500,
            &provider,
            vec![tool_call("call-1", "skill", json!({"name": "debug"}))],
        ));
        let RecordPreparation::Replayed(replayed) = replay else {
            panic!("a semantically identical redelivery is a replay");
        };
        assert_eq!(replayed.step_seq, first.step_seq);

        // a *different* payload for the same effect is a conflict
        let conflict = runtime.reject(&provider_result(
            "in-skill-conflict",
            1_700_000_002_600,
            &provider,
            vec![tool_call("call-9", "skill", json!({"name": "debug"}))],
        ));
        assert_eq!(conflict.code, KernelFaultCode::UnexpectedEffectOutcome);

        // an effect nobody is waiting on
        let unknown = EffectId::new("op-driver-1:step:77:effect:0").unwrap();
        let stray = runtime.reject(&provider_answer(
            "in-stray",
            1_700_000_002_700,
            &unknown,
            "hello",
        ));
        assert_eq!(stray.code, KernelFaultCode::UnexpectedEffectOutcome);

        // and a result of the wrong *kind* for the pending effect
        let pending = effect_id(first.step_seq);
        let mismatched = runtime.reject(&tools_resolved(
            "in-mismatch",
            1_700_000_002_800,
            &pending,
            &[("call-1", "x", false)],
        ));
        assert_eq!(mismatched.code, KernelFaultCode::UnexpectedEffectOutcome);
    }

    // ----- slice 2 · workflow / control ---------------------------------------------------------

    #[test]
    fn spc_008_01_wire_node_metadata_requested_capabilities_threads_into_the_core_spec() {
        use crate::types::capability::{
            ActionSet, Capability, CapabilityId, CapabilityKind, ConstraintSet, Principal,
            ResourceSelector,
        };

        let capability = Capability {
            id: CapabilityId("cap-1".into()),
            kind: CapabilityKind::Tool,
            resource: ResourceSelector("/repo/src/**".into()),
            actions: ActionSet(["read".into()].into_iter().collect()),
            constraints: ConstraintSet::default(),
            lease: None,
            delegatable: true,
            issuer: Principal("root".into()),
        };
        let metadata = json!({ "requested_capabilities": [capability.clone()] });

        let core = build_core_spec(&WireSpec {
            name: "cap-thread".to_string(),
            nodes: vec![WireNode {
                node_id: NodeId::new("solo").unwrap(),
                task: LogicalTask::new("do work"),
                depends_on: Vec::new(),
                run_spec: Some(LogicalAgentSpec {
                    metadata: crate::runtime::kernel::wire::BoundedJson::new(metadata).unwrap(),
                    ..LogicalAgentSpec::new("do work")
                }),
            }],
        })
        .expect("well-formed requested_capabilities builds");

        assert_eq!(core.nodes[0].requested_capabilities, vec![capability]);
    }

    #[test]
    fn spc_008_02_wire_node_metadata_requested_budget_threads_into_the_core_spec() {
        use crate::scheduler::budget_grant::ResourceBudget;

        let budget = ResourceBudget {
            tokens: Some(1_000),
            ..ResourceBudget::default()
        };
        let metadata = json!({ "requested_budget": budget });

        let core = build_core_spec(&WireSpec {
            name: "budget-thread".to_string(),
            nodes: vec![WireNode {
                node_id: NodeId::new("solo").unwrap(),
                task: LogicalTask::new("do work"),
                depends_on: Vec::new(),
                run_spec: Some(LogicalAgentSpec {
                    metadata: crate::runtime::kernel::wire::BoundedJson::new(metadata).unwrap(),
                    ..LogicalAgentSpec::new("do work")
                }),
            }],
        })
        .expect("well-formed requested_budget builds");

        assert_eq!(core.nodes[0].requested_budget, Some(budget));
    }

    #[test]
    fn spc_016_06_wire_node_scheduling_factors_thread_into_the_core_spec() {
        let factors = crate::orchestration::task_graph::SchedulingFactors {
            deadline_urgency: 3,
            process_priority: 2,
            resource_pressure: 1,
            budget_pressure: 4,
        };
        let metadata = json!({ "scheduling_factors": factors });

        let core = build_core_spec(&WireSpec {
            name: "scheduler-factors".to_string(),
            nodes: vec![WireNode {
                node_id: NodeId::new("solo").unwrap(),
                task: LogicalTask::new("do work"),
                depends_on: Vec::new(),
                run_spec: Some(LogicalAgentSpec {
                    metadata: crate::runtime::kernel::wire::BoundedJson::new(metadata).unwrap(),
                    ..LogicalAgentSpec::new("do work")
                }),
            }],
        })
        .expect("well-formed scheduling factors build");

        assert_eq!(core.nodes[0].scheduling_factors, factors);
    }

    #[test]
    fn spc_016_06_malformed_wire_node_scheduling_factors_fail_closed() {
        let metadata = json!({ "scheduling_factors": { "deadline_urgency": "urgent" } });
        let error = build_core_spec(&WireSpec {
            name: "scheduler-factors-malformed".to_string(),
            nodes: vec![WireNode {
                node_id: NodeId::new("solo").unwrap(),
                task: LogicalTask::new("do work"),
                depends_on: Vec::new(),
                run_spec: Some(LogicalAgentSpec {
                    metadata: crate::runtime::kernel::wire::BoundedJson::new(metadata).unwrap(),
                    ..LogicalAgentSpec::new("do work")
                }),
            }],
        })
        .expect_err("malformed scheduling factors must not silently become zeros");
        assert_eq!(error.code, KernelFaultCode::InvalidConfig);
    }

    #[test]
    fn spc_008_02_malformed_requested_budget_metadata_fails_closed() {
        let metadata = json!({ "requested_budget": {"tokens": "not a number"} });

        let error = build_core_spec(&WireSpec {
            name: "budget-malformed".to_string(),
            nodes: vec![WireNode {
                node_id: NodeId::new("solo").unwrap(),
                task: LogicalTask::new("do work"),
                depends_on: Vec::new(),
                run_spec: Some(LogicalAgentSpec {
                    metadata: crate::runtime::kernel::wire::BoundedJson::new(metadata).unwrap(),
                    ..LogicalAgentSpec::new("do work")
                }),
            }],
        })
        .expect_err("malformed requested_budget must fail closed, not silently drop");
        assert_eq!(error.code, KernelFaultCode::InvalidConfig);
    }

    #[test]
    fn spc_008_01_malformed_requested_capabilities_metadata_fails_closed() {
        // A security-relevant declaration that fails to parse must reject the whole spec, not
        // silently degrade into "no capability requested" (which would make the attenuation check
        // a no-op instead of denying).
        let metadata = json!({ "requested_capabilities": [{"not": "a capability"}] });

        let error = build_core_spec(&WireSpec {
            name: "cap-malformed".to_string(),
            nodes: vec![WireNode {
                node_id: NodeId::new("solo").unwrap(),
                task: LogicalTask::new("do work"),
                depends_on: Vec::new(),
                run_spec: Some(LogicalAgentSpec {
                    metadata: crate::runtime::kernel::wire::BoundedJson::new(metadata).unwrap(),
                    ..LogicalAgentSpec::new("do work")
                }),
            }],
        })
        .expect_err("malformed requested_capabilities must fail closed, not silently drop");
        assert_eq!(error.code, KernelFaultCode::InvalidConfig);
    }

    // fixture: removed-kernel-auto-redispatch
    #[test]
    fn a_spawn_failure_fails_the_whole_batch_and_never_re_launches_it() {
        let mut runtime = Runtime::new();
        runtime.submit(&configure());
        let started = runtime.submit(&workflow_start(
            "in-start",
            1_700_000_001_000,
            two_node_spec(),
        ));
        let spawn = effect_id(started.step_seq);

        let after = runtime.submit(&failed(
            "in-launch-failed",
            1_700_000_002_000,
            &spawn,
            HostEffectFailureKind::ResourceExhausted,
            "no worker slots",
        ));
        assert!(
            !after
                .published_effects()
                .iter()
                .any(|effect| effect.tag() == EffectKindTag::SpawnTasks),
            "DEC-5 · the same launch intent is never re-emitted"
        );
        let KernelTerminal::Workflow(workflow) = after
            .terminal()
            .expect("a DAG whose only ready node could not start drains")
        else {
            panic!("expected a workflow terminal, got {:?}", after.terminal());
        };
        assert_eq!(workflow.outcome.status, WorkflowStatus::Failed);
        assert!(
            runtime.driver.attempts.is_empty(),
            "a launch that never happened leaves no live attempt to complete later"
        );
    }

    #[test]
    fn an_approval_resolution_dispatches_exactly_what_was_approved() {
        let (mut runtime, provider) = agent_awaiting_approval();
        let requested = runtime.submit(&provider_result(
            "in-gated",
            1_700_000_002_000,
            &provider,
            vec![
                tool_call("call-1", "search", json!({"q": "a"})),
                tool_call("call-2", "search", json!({"q": "b"})),
            ],
        ));
        assert_eq!(kinds(&requested), vec![EffectKindTag::RequestApproval]);
        let approval = effect_id(requested.step_seq);
        assert!(matches!(
            runtime
                .driver
                .engine()
                .unwrap()
                .task_table()
                .get(ROOT_TASK_ID)
                .unwrap()
                .wait_set
                .as_ref()
                .unwrap()
                .conditions
                .as_slice(),
            [WaitCondition::Approval(_)]
        ));

        let resumed = runtime.submit(&resolved(
            "in-approved",
            1_700_000_003_000,
            &approval,
            EffectSuccess::Approval(ApprovalSuccess {
                approved_call_ids: vec![CallId::new("call-1").unwrap()],
                denied_call_ids: vec![CallId::new("call-2").unwrap()],
            }),
        ));
        assert_eq!(kinds(&resumed), vec![EffectKindTag::ExecuteTools]);
        let EffectKind::ExecuteTools(execute) = &sole_effect(&resumed).effect else {
            panic!("expected a tool batch");
        };
        assert_eq!(
            execute
                .calls
                .iter()
                .map(|call| call.call_id.as_str())
                .collect::<Vec<_>>(),
            vec!["call-1"],
            "only the approved call reaches a host"
        );
        assert!(
            runtime
                .driver
                .engine()
                .unwrap()
                .task_table()
                .get(ROOT_TASK_ID)
                .unwrap()
                .wait_set
                .is_none(),
            "approval success consumes the durable approval wait"
        );
    }

    // fixture: removed-kernel-auto-redispatch
    #[test]
    fn an_approval_failure_denies_every_gated_call_and_never_re_asks() {
        let (mut runtime, provider) = agent_awaiting_approval();
        let requested = runtime.submit(&provider_result(
            "in-gated",
            1_700_000_002_000,
            &provider,
            vec![tool_call("call-1", "search", json!({"q": "a"}))],
        ));
        let approval = effect_id(requested.step_seq);

        let after = runtime.submit(&failed(
            "in-approval-failed",
            1_700_000_003_000,
            &approval,
            HostEffectFailureKind::StorageUnavailable,
            "the approval queue is down",
        ));
        assert!(
            !after
                .published_effects()
                .iter()
                .any(|effect| effect.tag() == EffectKindTag::RequestApproval),
            "DEC-5 · `retry_approval` is deleted on this wire"
        );
        assert_eq!(kinds(&after), vec![EffectKindTag::CallProvider]);
        assert!(
            observation_kinds(&runtime).contains(&"approval_resolution_failed"),
            "the failure is a typed audit fact: {:?}",
            observation_kinds(&runtime)
        );
        assert!(
            history_text(&runtime)
                .iter()
                .any(|line| line.contains("permission denied")),
            "fail closed: an approval that never arrived approves nothing"
        );
    }

    #[test]
    fn a_preempt_resolution_settles_every_attempt_it_names() {
        let (mut runtime, spawn_effect) = workflow_with_live_child();
        let attempts = vec![
            TaskPreemptOutcome {
                task_id: TaskId::new("wf-node0").unwrap(),
                attempt_id: WireAttemptId::new("wf-node0:attempt:1").unwrap(),
                outcome: TaskPreemptStatus::Preempted(TaskPreempted {}),
            },
            TaskPreemptOutcome {
                task_id: TaskId::new("wf-node1").unwrap(),
                attempt_id: WireAttemptId::new("wf-node1:attempt:1").unwrap(),
                outcome: TaskPreemptStatus::AlreadyFinished(TaskAlreadyFinished {}),
            },
        ];
        // The preempt effect has no canonical producer yet (its only trigger is the signal /
        // cancel path). Publishing it through the driver's own mint is what makes the *resolution*
        // contract — the half Task 12 owns — reachable.
        let carrier = syscall_carrier("in-preempt-effect", 1_700_000_004_000);
        let published = runtime.submit_planned(&carrier, |driver, context| {
            let mut index = 0;
            let effect = driver.mint_effect(
                context,
                EffectKind::PreemptTasks(PreemptTasksEffect {
                    attempts: vec![TaskAttemptRef {
                        task_id: TaskId::new("wf-node0").unwrap(),
                        attempt_id: WireAttemptId::new("wf-node0:attempt:1").unwrap(),
                    }],
                    reason: "cancelled".to_string(),
                }),
                &mut index,
            );
            Ok(PlannedStep {
                root_kind: Some(RootKind::Workflow),
                focus: driver.focus().cloned(),
                observations: Vec::new(),
                disposition: StepDisposition::Effects(EffectsDisposition {
                    effects: vec![effect],
                }),
            })
        });
        let preempt = effect_id(published.step_seq);
        assert!(runtime.driver.attempts.contains_key("wf-node0"));

        runtime.submit(&resolved(
            "in-preempted",
            1_700_000_005_000,
            &preempt,
            EffectSuccess::TasksPreempted(TasksPreemptedSuccess { attempts }),
        ));
        assert!(
            !runtime.driver.attempts.contains_key("wf-node0"),
            "a preempted attempt is spent, so a later completion naming it is a stale causation"
        );
        let _ = spawn_effect;
    }

    // fixture: removed-kernel-auto-redispatch
    #[test]
    fn a_preempt_failure_is_an_audit_fact_and_not_a_second_preemption() {
        let (mut runtime, _) = workflow_with_live_child();
        let carrier = syscall_carrier("in-preempt-effect", 1_700_000_004_000);
        let published = runtime.submit_planned(&carrier, |driver, context| {
            let mut index = 0;
            let effect = driver.mint_effect(
                context,
                EffectKind::PreemptTasks(PreemptTasksEffect {
                    attempts: vec![TaskAttemptRef {
                        task_id: TaskId::new("wf-node0").unwrap(),
                        attempt_id: WireAttemptId::new("wf-node0:attempt:1").unwrap(),
                    }],
                    reason: "cancelled".to_string(),
                }),
                &mut index,
            );
            Ok(PlannedStep {
                root_kind: Some(RootKind::Workflow),
                focus: driver.focus().cloned(),
                observations: Vec::new(),
                disposition: StepDisposition::Effects(EffectsDisposition {
                    effects: vec![effect],
                }),
            })
        });
        let preempt = effect_id(published.step_seq);

        let after = runtime.submit(&failed(
            "in-preempt-failed",
            1_700_000_005_000,
            &preempt,
            HostEffectFailureKind::Unknown,
            "the supervisor did not answer",
        ));
        assert!(
            after.published_effects().is_empty(),
            "DEC-5 · `retry_preempt` is deleted on this wire"
        );
        assert!(
            observation_kinds(&runtime).contains(&"agent_preempt_failed"),
            "{:?}",
            observation_kinds(&runtime)
        );
    }

    #[test]
    fn a_milestone_verdict_advances_the_contract_it_belongs_to() {
        let (mut runtime, provider) = agent_awaiting_milestone_check();
        let requested = runtime.submit(&provider_answer(
            "in-claim",
            1_700_000_002_000,
            &provider,
            "phase one is done",
        ));
        assert_eq!(kinds(&requested), vec![EffectKindTag::EvaluateMilestone]);
        let milestone = effect_id(requested.step_seq);

        let blocked = runtime.submit(&resolved(
            "in-verdict",
            1_700_000_003_000,
            &milestone,
            EffectSuccess::MilestoneEvaluated(MilestoneEvaluatedSuccess {
                result: WireMilestoneResult {
                    phase_id: "collect".to_string(),
                    passed: false,
                    failed_criteria: vec!["no sources cited".to_string()],
                    score: None,
                    notes: String::new(),
                },
            }),
        ));
        assert_eq!(kinds(&blocked), vec![EffectKindTag::CallProvider]);
        assert!(
            observation_kinds(&runtime).contains(&"milestone_blocked"),
            "{:?}",
            observation_kinds(&runtime)
        );
    }

    /// **`EvaluateMilestone`'s canonical producer, end to end** (Task 12 SPEC-ISSUE-4).
    ///
    /// Contract declared in `verification_contracts` → referenced by the root run spec → loaded as
    /// the engine's phase cascade → phase 0 published as an `EvaluateMilestone` → a passing verdict
    /// mounts that phase's `unlocks` and moves the cascade to phase 1. Before Task 14 the middle
    /// three links did not exist on the wire at all: the effect, its resolution and its failure
    /// path were reachable only by reaching into the engine from outside.
    #[test]
    fn a_declared_contract_drives_the_whole_milestone_cascade() {
        let (mut runtime, provider) = agent_awaiting_milestone_check();

        // link 3: the cascade is installed, so the first turn's completion asks for phase 0
        let requested = runtime.submit(&provider_answer(
            "in-claim",
            1_700_000_002_000,
            &provider,
            "sources collected",
        ));
        assert_eq!(kinds(&requested), vec![EffectKindTag::EvaluateMilestone]);
        let EffectKind::EvaluateMilestone(evaluate) = &sole_effect(&requested).effect else {
            panic!("expected a milestone request");
        };
        assert_eq!(
            evaluate.request.phase_id, "collect",
            "the request names the phase the declared cascade is on"
        );
        assert_eq!(
            evaluate.request.contract_id, "brief-quality-v1",
            "a phase id is unique only inside its contract, so the request carries the pair the \
             host looks its verifier up by"
        );
        // and nothing else: criteria, evidence and verifier are host-owned (§5.2)
        let request = serde_json::to_value(&evaluate.request).unwrap();
        assert_eq!(
            request.as_object().unwrap().keys().collect::<Vec<_>>(),
            vec!["contract_id", "phase_id"],
            "{request}"
        );

        // link 4: a pass mounts the phase's unlocks and advances to phase 1
        let advanced = runtime.submit(&resolved(
            "in-verdict",
            1_700_000_003_000,
            &effect_id(requested.step_seq),
            EffectSuccess::MilestoneEvaluated(MilestoneEvaluatedSuccess {
                result: WireMilestoneResult {
                    phase_id: "collect".to_string(),
                    passed: true,
                    failed_criteria: Vec::new(),
                    score: None,
                    notes: String::new(),
                },
            }),
        ));
        assert_eq!(kinds(&advanced), vec![EffectKindTag::CallProvider]);
        let advance = runtime
            .observations()
            .iter()
            .find_map(|observation| match observation {
                KernelObservation::MilestoneAdvanced {
                    phase_id,
                    capabilities_unlocked,
                    ..
                } => Some((phase_id.clone(), capabilities_unlocked.clone())),
                _ => None,
            })
            .unwrap_or_else(|| panic!("{:?}", observation_kinds(&runtime)));
        assert_eq!(advance.0, "collect");
        assert_eq!(
            advance.1,
            vec!["Tool:search".to_string()],
            "the phase's declared unlocks are the ones mounted"
        );
        assert_eq!(
            runtime
                .driver
                .engine()
                .expect("engine")
                .current_milestone_phase_id(),
            Some("write"),
            "the cascade advanced to the next declared phase"
        );

        // link 5: phase 1 unlocks a *skill*, proving the projection covers both directories
        let requested_2 = runtime.submit(&provider_answer(
            "in-claim-2",
            1_700_000_004_000,
            &effect_id(advanced.step_seq),
            "brief written",
        ));
        assert_eq!(kinds(&requested_2), vec![EffectKindTag::EvaluateMilestone]);
        runtime.submit(&resolved(
            "in-verdict-2",
            1_700_000_005_000,
            &effect_id(requested_2.step_seq),
            EffectSuccess::MilestoneEvaluated(MilestoneEvaluatedSuccess {
                result: WireMilestoneResult {
                    phase_id: "write".to_string(),
                    passed: true,
                    failed_criteria: Vec::new(),
                    score: None,
                    notes: String::new(),
                },
            }),
        ));
        let unlocked_by_phase_two =
            runtime
                .observations()
                .iter()
                .find_map(|observation| match observation {
                    KernelObservation::MilestoneAdvanced {
                        capabilities_unlocked,
                        ..
                    } => Some(capabilities_unlocked.clone()),
                    _ => None,
                });
        assert_eq!(unlocked_by_phase_two, Some(vec!["Skill:debug".to_string()]));
    }

    #[test]
    fn a_run_spec_naming_an_undeclared_contract_is_refused_before_anything_moves() {
        // A reference that resolves to nothing is a gate the run believes it has: the agent would
        // start with no cascade, never publish an `EvaluateMilestone`, and finish having "passed"
        // a contract that was never evaluated.
        let mut runtime = Runtime::new();
        runtime.submit(&syscall_config_with(|config| {
            config.host_effect_support = support_with([EffectKindTag::EvaluateMilestone]);
            config.verification_contracts = vec![brief_contract()];
        }));
        let head_before = runtime.tx.head();

        let fault = runtime.reject(&agent_start_under_contract(
            "in-start",
            1_700_000_001_000,
            "brief-quality-v2",
        ));
        assert_eq!(fault.code, KernelFaultCode::InvalidConfig);
        assert!(
            fault.message.contains("brief-quality-v2"),
            "{}",
            fault.message
        );
        assert_eq!(runtime.tx.head(), head_before, "nothing moved");
        assert!(
            runtime.driver.root_kind().is_none(),
            "the operation is still free to start with a spec that resolves"
        );

        // …and the same spec with the declared id starts normally
        runtime.submit(&agent_start_under_contract(
            "in-start-ok",
            1_700_000_001_500,
            "brief-quality-v1",
        ));
        assert_eq!(runtime.driver.root_kind(), Some(RootKind::Agent));
    }

    #[test]
    fn a_workflow_node_may_not_name_an_undeclared_contract_either() {
        // Same rule wherever a `LogicalAgentSpec` enters: a root, a DAG node, an authored append.
        let mut runtime = Runtime::new();
        runtime.submit(&syscall_config_with(|config| {
            config.host_effect_support = support_with([EffectKindTag::EvaluateMilestone]);
            config.verification_contracts = vec![brief_contract()];
        }));
        let mut spec = two_node_spec();
        spec.nodes[1].run_spec = Some(LogicalAgentSpec {
            verification_contract_id: Some("no-such-contract".to_string()),
            ..LogicalAgentSpec::new("write the brief")
        });
        let fault = runtime.reject(&workflow_start("in-start", 1_700_000_001_000, spec));
        assert_eq!(fault.code, KernelFaultCode::InvalidConfig);
        assert!(
            fault.message.contains("no-such-contract"),
            "{}",
            fault.message
        );
    }

    // fixture: removed-kernel-auto-redispatch
    #[test]
    fn a_milestone_that_could_not_be_evaluated_terminates_instead_of_advancing() {
        let (mut runtime, provider) = agent_awaiting_milestone_check();
        let requested = runtime.submit(&provider_answer(
            "in-claim",
            1_700_000_002_000,
            &provider,
            "phase one is done",
        ));
        let milestone = effect_id(requested.step_seq);

        let ended = runtime.submit(&failed(
            "in-verifier-down",
            1_700_000_003_000,
            &milestone,
            HostEffectFailureKind::StorageUnavailable,
            "the verifier could not be reached",
        ));
        assert!(ended.published_effects().is_empty());
        let KernelTerminal::Failed(failure) = ended.terminal().expect("a terminal was committed")
        else {
            panic!("expected a failed terminal, got {:?}", ended.terminal());
        };
        assert_eq!(failure.failure.code, KernelFailureCode::HostEffectFailed);
        assert!(
            failure.failure.message.contains("evaluate_milestone"),
            "{}",
            failure.failure.message
        );
    }

    // ----- slice 3 · memory / page-out ----------------------------------------------------------

    /// §22.13 · a receipt is a locator, never a rewrite of the record the kernel authored.
    #[test]
    fn a_memory_receipt_cannot_restate_what_the_kernel_authored() {
        let (mut runtime, effect) = agent_awaiting_memory_write();
        let settled = runtime.submit(&resolved(
            "in-persisted",
            1_700_000_003_000,
            &effect,
            EffectSuccess::MemoryPersisted(MemoryPersistedSuccess {
                receipt: MemoryPersistReceipt {
                    binding_id: MemoryBindingId::new("some-other-binding").unwrap(),
                    record_ref: MemoryRecordRef::new("rec-7").unwrap(),
                    digest: Digest::new("sha256:".to_string() + &"0".repeat(64)).unwrap(),
                },
            }),
        ));
        assert!(
            settled.published_effects().is_empty(),
            "a persisted record is a fact, not a new obligation"
        );
        let written = runtime
            .observations()
            .iter()
            .find_map(|observation| match observation {
                KernelObservation::MemoryWritten {
                    record_id,
                    scope,
                    name,
                    memory_kind,
                    size_bytes,
                    ..
                } => Some((
                    record_id.clone(),
                    scope.clone(),
                    name.clone(),
                    *memory_kind,
                    *size_bytes,
                )),
                _ => None,
            })
            .expect("the resolution records the write");
        assert_eq!(written.0, "rec-7", "the host contributes its own locator");
        assert_eq!(
            written.1.namespace, "mem-binding-1",
            "and nothing else: the binding is the one the operation holds, not the one echoed back"
        );
        assert_eq!(written.2, "brief-style");
        assert_eq!(written.3, crate::mm::memory::MemoryKind::Project);
        assert_eq!(written.4, "prefers numbered sections".len() as u32);
    }

    #[test]
    fn a_memory_write_failure_names_the_intent_the_kernel_authored() {
        let (mut runtime, effect) = agent_awaiting_memory_write();
        let settled = runtime.submit(&failed(
            "in-persist-failed",
            1_700_000_003_000,
            &effect,
            HostEffectFailureKind::StorageUnavailable,
            "the memory store is offline",
        ));
        assert!(settled.published_effects().is_empty());
        assert!(
            observation_kinds(&runtime).contains(&"memory_write_failed"),
            "{:?}",
            observation_kinds(&runtime)
        );
    }

    #[test]
    fn a_memory_recall_enters_context_before_the_turn_resumes() {
        let (mut runtime, provider) = agent_awaiting_provider();
        let queried = runtime.submit(&provider_result(
            "in-memory",
            1_700_000_002_000,
            &provider,
            vec![tool_call(
                "call-1",
                crate::context::manager::MEMORY_TOOL_NAME,
                json!({"query": "prior briefs", "top_k": 9}),
            )],
        ));
        let query_effect = sole_effect(&queried);
        let EffectKind::QueryMemory(query) = &query_effect.effect else {
            panic!("expected a memory query");
        };
        assert_eq!(
            query.requested_k, 4,
            "retrieval width is the operation's policy, clamped — a model cannot widen it"
        );
        let effect = query_effect.effect_id.clone();

        let resumed = runtime.submit(&resolved(
            "in-recalls",
            1_700_000_003_000,
            &effect,
            EffectSuccess::MemoryQueried(MemoryQueriedSuccess {
                recalls: vec![MemoryRecall {
                    record_ref: MemoryRecordRef::new("rec-1").unwrap(),
                    name: "brief-style".to_string(),
                    kind: SyscallMemoryKind::Project,
                    content: "prefers numbered sections".to_string(),
                    score: None,
                }],
            }),
        ));
        assert_eq!(kinds(&resumed), vec![EffectKindTag::CallProvider]);
        assert!(
            history_text(&runtime)
                .iter()
                .any(|line| line.contains("prefers numbered sections")),
            "the recall is in the context the resumed turn renders"
        );
        assert!(
            observation_kinds(&runtime).contains(&"memory_queried"),
            "{:?}",
            observation_kinds(&runtime)
        );
    }

    #[test]
    fn a_memory_query_failure_resumes_the_turn_without_recalls() {
        let (mut runtime, provider) = agent_awaiting_provider();
        let queried = runtime.submit(&provider_result(
            "in-memory",
            1_700_000_002_000,
            &provider,
            vec![tool_call(
                "call-1",
                crate::context::manager::MEMORY_TOOL_NAME,
                json!({"query": "prior briefs"}),
            )],
        ));
        let effect = effect_id(queried.step_seq);

        let resumed = runtime.submit(&failed(
            "in-query-failed",
            1_700_000_003_000,
            &effect,
            HostEffectFailureKind::StorageUnavailable,
            "the memory store is offline",
        ));
        assert_eq!(
            kinds(&resumed),
            vec![EffectKindTag::CallProvider],
            "a store that could not answer is not a reason to stall the run"
        );
        assert!(
            observation_kinds(&runtime).contains(&"memory_query_failed"),
            "{:?}",
            observation_kinds(&runtime)
        );
    }

    #[test]
    fn a_page_out_archive_holds_the_continuation_until_the_host_commits_it() {
        let (mut runtime, archive) = agent_awaiting_page_out();
        let EffectKind::ArchivePageOut(published) = &runtime
            .tx
            .pending_effects()
            .find(|effect| effect.effect_id == archive)
            .expect("the archive is pending")
            .effect
            .clone()
        else {
            panic!("expected a page-out effect");
        };

        let resumed = runtime.submit(&resolved(
            "in-archived",
            1_700_000_004_000,
            &archive,
            EffectSuccess::PageOutArchived(PageOutArchivedSuccess {
                receipt: ArchiveReceipt {
                    handle_id: published.handle_id.clone(),
                    payload_ref: PayloadRef::new("blob-1").unwrap(),
                    digest: published.payload.digest.clone(),
                    original_size: published.payload.original_size,
                },
            }),
        ));
        assert_eq!(
            kinds(&resumed),
            vec![EffectKindTag::CallProvider],
            "the provider retry the compaction deferred is released by the archive's commit"
        );
        assert!(
            observation_kinds(&runtime).contains(&"page_out_archived"),
            "{:?}",
            observation_kinds(&runtime)
        );
    }

    #[test]
    fn an_archive_receipt_for_another_body_is_refused() {
        let (mut runtime, archive) = agent_awaiting_page_out();
        let fault = runtime.reject(&resolved(
            "in-wrong-archive",
            1_700_000_004_000,
            &archive,
            EffectSuccess::PageOutArchived(PageOutArchivedSuccess {
                receipt: ArchiveReceipt {
                    handle_id: HandleId::new("some-other-handle").unwrap(),
                    payload_ref: PayloadRef::new("blob-1").unwrap(),
                    digest: Digest::new("sha256:".to_string() + &"1".repeat(64)).unwrap(),
                    original_size: WireU64::new(1),
                },
            }),
        ));
        assert_eq!(fault.code, KernelFaultCode::UnexpectedEffectOutcome);
    }

    // fixture: removed-kernel-auto-redispatch
    #[test]
    fn a_failed_archive_is_abandoned_and_the_run_stays_live() {
        let (mut runtime, archive) = agent_awaiting_page_out();
        let resumed = runtime.submit(&failed(
            "in-archive-failed",
            1_700_000_004_000,
            &archive,
            HostEffectFailureKind::StorageUnavailable,
            "the blob store is offline",
        ));
        assert_eq!(
            kinds(&resumed),
            vec![EffectKindTag::CallProvider],
            "DEC-5 · the archive is abandoned once; the compaction it belongs to already happened, \
             so the run continues degraded rather than dying on a best-effort durability effect"
        );
        assert!(
            observation_kinds(&runtime).contains(&"page_out_archive_failed"),
            "{:?}",
            observation_kinds(&runtime)
        );
    }

    /// A load outcome that answers no pending `LoadPayload` effect fails closed: since Task 13 the
    /// producer is the P1 `PageIn { handle_id }` syscall, and a resolution is only reducible
    /// against the effect it actually answers.
    #[test]
    fn a_payload_load_outcome_is_refused_with_its_reason() {
        let (mut runtime, provider) = agent_awaiting_provider();
        let fault = runtime.reject(&resolved(
            "in-loaded",
            1_700_000_002_000,
            &provider,
            EffectSuccess::PayloadLoaded(PayloadLoadedSuccess {
                handle_id: HandleId::new("call-1").unwrap(),
                payload: InlinePayload {
                    content: "body".to_string(),
                    digest: Digest::new("sha256:".to_string() + &"2".repeat(64)).unwrap(),
                    original_size: WireU64::new(4),
                },
            }),
        ));
        // the transaction refuses it first: a provider effect does not accept a payload result
        assert_eq!(fault.code, KernelFaultCode::UnexpectedEffectOutcome);
    }

    // ----- fixtures the failure vocabulary itself has to satisfy --------------------------------

    // fixture: removed-cancellation-as-effect-failure
    #[test]
    fn the_failure_vocabulary_cannot_express_a_cancellation() {
        for kind in HostEffectFailureKind::ALL {
            let label = kind.as_str();
            assert!(
                !label.contains("cancel"),
                "cancellation is a control-plane fact and only reaches the kernel through \
                 HostControl::Cancel; {label} would give one pending effect two meanings"
            );
        }
        assert_eq!(HostEffectFailureKind::ALL.len(), 6);
    }

    // ----- arcs the tests above start from ------------------------------------------------------

    fn agent_start_with_history(id: &str, at: u64, messages: usize) -> WireEnvelope {
        envelope(
            id,
            at,
            KernelInput::StartOperation(StartOperation {
                entry: RootEntry::Agent(RootAgentEntry {
                    task: LogicalTask::new("write the research brief"),
                    run_spec: None,
                }),
                initial_context: InitialContext {
                    messages: (0..messages)
                        .map(|index| super::super::root::LogicalMessage {
                            role: if index % 2 == 0 {
                                MessageRole::User
                            } else {
                                MessageRole::Assistant
                            },
                            content: format!(
                                "turn {index}: a long enough body that compaction has something \
                                 to reclaim when the prompt stops fitting"
                            ),
                            tokens: Some(64),
                            tool_call_id: None,
                        })
                        .collect(),
                    ..InitialContext::default()
                },
            }),
        )
    }

    /// An agent whose governance policy gates `search` behind an approval.
    fn agent_awaiting_approval() -> (Runtime, EffectId) {
        use crate::runtime::kernel::wire::command::{GovernancePolicy, PolicyAction, PolicyRule};

        let mut runtime = Runtime::new();
        runtime.submit(&syscall_config_with(|config| {
            config.host_effect_support = support_with([EffectKindTag::RequestApproval]);
            config.governance_policy = Some(GovernancePolicy {
                rules: vec![PolicyRule {
                    tool_pattern: "search".to_string(),
                    action: PolicyAction::AskUser,
                }],
                ..GovernancePolicy::default()
            });
        }));
        let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));
        (runtime, sole_effect(&started).effect_id.clone())
    }

    /// The two-phase skeleton the milestone arcs run on: `collect` unlocks the `search` tool,
    /// `write` unlocks the `debug` skill — one of each capability directory, so the projection is
    /// exercised in both directions.
    fn brief_contract() -> WireVerificationContract {
        WireVerificationContract {
            contract_id: "brief-quality-v1".to_string(),
            phases: vec![
                WireMilestonePhase {
                    phase_id: "collect".to_string(),
                    unlocks: vec!["search".to_string()],
                },
                WireMilestonePhase {
                    phase_id: "write".to_string(),
                    unlocks: vec!["debug".to_string()],
                },
            ],
        }
    }

    fn agent_start_under_contract(id: &str, at: u64, contract_id: &str) -> WireEnvelope {
        envelope(
            id,
            at,
            KernelInput::StartOperation(StartOperation {
                entry: RootEntry::Agent(RootAgentEntry {
                    task: LogicalTask::new("write the research brief"),
                    run_spec: Some(LogicalAgentSpec {
                        verification_contract_id: Some(contract_id.to_string()),
                        ..LogicalAgentSpec::new("write the research brief")
                    }),
                }),
                initial_context: InitialContext::default(),
            }),
        )
    }

    /// An agent carrying a two-phase milestone contract, declared the canonical way: the
    /// operation's `verification_contracts` catalog holds the skeleton and the root run spec
    /// references it by id (Task 14 · closes Task 12's SPEC-ISSUE-4, where nothing on the wire
    /// could make the kernel ask for a verdict).
    fn agent_awaiting_milestone_check() -> (Runtime, EffectId) {
        let mut runtime = Runtime::new();
        runtime.submit(&syscall_config_with(|config| {
            config.host_effect_support = support_with([EffectKindTag::EvaluateMilestone]);
            config.verification_contracts = vec![brief_contract()];
        }));
        let started = runtime.submit(&agent_start_under_contract(
            "in-start",
            1_700_000_001_000,
            "brief-quality-v1",
        ));
        (runtime, sole_effect(&started).effect_id.clone())
    }

    /// An agent that has published a `PersistMemory` effect through the child→parent request path.
    fn agent_awaiting_memory_write() -> (Runtime, EffectId) {
        use crate::runtime::kernel::wire::syscall::{
            MemoryWriteProposal, RequestMemoryWriteRequest,
        };

        let mut runtime = Runtime::new();
        runtime.submit(&syscall_config());
        let started = runtime.submit(&workflow_start(
            "in-start",
            1_700_000_001_000,
            two_node_spec(),
        ));
        runtime.submit(&spawned(
            "in-ack",
            1_700_000_002_000,
            &effect_id(started.step_seq),
            &["wf-node0"],
        ));
        let completed = runtime.submit(&child_done_with(
            "in-done",
            1_700_000_002_500,
            "wf-node0",
            "wf-node0:attempt:1",
            vec![SyscallRequest::RequestMemoryWrite(
                RequestMemoryWriteRequest {
                    proposal: MemoryWriteProposal {
                        name: "brief-style".to_string(),
                        kind: SyscallMemoryKind::Project,
                        content: "prefers numbered sections".to_string(),
                        description: String::new(),
                        evidence_refs: Vec::new(),
                    },
                },
            )],
        ));
        let effect = completed
            .published_effects()
            .iter()
            .find(|effect| effect.tag() == EffectKindTag::PersistMemory)
            .expect("the child's request published a memory write")
            .effect_id
            .clone();
        (runtime, effect)
    }

    /// An agent whose context overflowed hard enough to compact, so a page-out archive is pending
    /// and the provider retry it deferred is waiting behind it.
    fn agent_awaiting_page_out() -> (Runtime, EffectId) {
        let mut runtime = Runtime::new();
        runtime.submit(&syscall_config_with(|config| {
            config.host_effect_support = support_with([EffectKindTag::ArchivePageOut]);
        }));
        let started = runtime.submit(&agent_start_with_history("in-start", 1_700_000_001_000, 14));
        let provider = sole_effect(&started).effect_id.clone();
        let compacted = runtime.submit(&provider_overflow(
            "in-overflow",
            1_700_000_002_000,
            &provider,
        ));
        let archive = compacted
            .published_effects()
            .iter()
            .find(|effect| effect.tag() == EffectKindTag::ArchivePageOut)
            .expect("the compaction externalised its archive")
            .effect_id
            .clone();
        (runtime, archive)
    }

    /// A workflow root with its first node launched and acknowledged, so a live attempt exists.
    fn workflow_with_live_child() -> (Runtime, EffectId) {
        let mut runtime = Runtime::new();
        runtime.submit(&syscall_config());
        let started = runtime.submit(&workflow_start(
            "in-start",
            1_700_000_001_000,
            two_node_spec(),
        ));
        let spawn = effect_id(started.step_seq);
        runtime.submit(&spawned("in-ack", 1_700_000_002_000, &spawn, &["wf-node0"]));
        (runtime, spawn)
    }

    // -----------------------------------------------------------------------------------------
    // §7.5 · host control plane, and §7.7 · signal delivery (Task 12b)
    // -----------------------------------------------------------------------------------------

    fn control(id: &str, at: u64, command: HostCommand) -> WireEnvelope {
        envelope(
            id,
            at,
            KernelInput::HostControl(super::super::envelope::HostControl { command }),
        )
    }

    fn cancel_with(id: &str, at: u64, reason: CancellationReason) -> WireEnvelope {
        control(
            id,
            at,
            HostCommand::Cancel(CancelCommand {
                reason,
                pending_call_ids: Vec::new(),
            }),
        )
    }

    fn cancel(id: &str, at: u64) -> WireEnvelope {
        cancel_with(id, at, CancellationReason::User)
    }

    /// One signal delivery. `delivery` and `signal` are separate arguments precisely because they
    /// are separate identities (§7.7).
    fn signal_delivery(
        id: &str,
        at: u64,
        delivery: &str,
        attempt: u32,
        signal: LogicalSignal,
    ) -> WireEnvelope {
        envelope(
            id,
            at,
            KernelInput::DeliverExternalEvent(DeliverExternalEvent {
                event: ExternalEvent::DeliverSignal(DeliverSignal {
                    delivery_id: DeliveryId::new(delivery).unwrap(),
                    attempt,
                    signal,
                }),
            }),
        )
    }

    fn logical_signal(id: &str, urgency: SignalUrgency) -> LogicalSignal {
        LogicalSignal {
            urgency: Some(urgency),
            ..LogicalSignal::new(SignalId::new(id).unwrap())
        }
    }

    /// A configuration with a signal policy, so queue capacity and TTL are the operation's own
    /// facts rather than a compile-time default.
    fn signal_config(queue_max: u32, ttl_ms: Option<u64>) -> WireEnvelope {
        syscall_config_with(|config| {
            config.signal_policy = Some(super::super::command::SignalPolicy {
                queue_max,
                ttl_ms: ttl_ms.map(WireU64::new),
                deadline_escalation: None,
            });
        })
    }

    #[test]
    fn signal_and_timer_waits_wake_only_through_the_canonical_envelope_clock_and_event() {
        use crate::scheduler::tcb::{
            LogicalDeadline, SignalFilter, WaitCondition, WaitMode, WaitSet,
        };

        let mut signal_runtime = workflow_root_awaiting_first_child();
        signal_runtime
            .driver
            .engine_mut()
            .unwrap()
            .task_table_mut()
            .register_wait_set(
                "wf-node0",
                WaitSet {
                    mode: WaitMode::Any,
                    conditions: vec![WaitCondition::Signal(SignalFilter("sig-wake".into()))],
                },
            );
        let mut wake_signal = logical_signal("sig-wake", SignalUrgency::Normal);
        wake_signal.target = SignalTarget::Task(super::super::event::TaskTarget {
            task_id: TaskId::new("wf-node0").unwrap(),
        });
        signal_runtime.submit(&signal_delivery(
            "in-signal-wake",
            1_700_000_002_000,
            "delivery-signal-wake",
            1,
            wake_signal,
        ));
        assert!(
            signal_runtime
                .driver
                .engine()
                .unwrap()
                .task_table()
                .get("wf-node0")
                .unwrap()
                .wait_set
                .is_none()
        );

        let (mut timer_runtime, _) = agent_awaiting_provider();
        timer_runtime
            .driver
            .engine_mut()
            .unwrap()
            .task_table_mut()
            .register_wait_set(
                ROOT_TASK_ID,
                WaitSet {
                    mode: WaitMode::Any,
                    conditions: vec![WaitCondition::Timer(LogicalDeadline(1_700_000_003_000))],
                },
            );
        timer_runtime.submit(&control(
            "in-before-deadline",
            1_700_000_002_000,
            HostCommand::UpdateTask(UpdateTaskCommand {
                update: WireTaskUpdate::default(),
            }),
        ));
        assert!(
            timer_runtime
                .driver
                .engine()
                .unwrap()
                .task_table()
                .get(ROOT_TASK_ID)
                .unwrap()
                .wait_set
                .is_some(),
            "an earlier accepted envelope cannot wake the timer"
        );
        timer_runtime.submit(&control(
            "in-at-deadline",
            1_700_000_003_000,
            HostCommand::UpdateTask(UpdateTaskCommand {
                update: WireTaskUpdate::default(),
            }),
        ));
        assert!(
            timer_runtime
                .driver
                .engine()
                .unwrap()
                .task_table()
                .get(ROOT_TASK_ID)
                .unwrap()
                .wait_set
                .is_none(),
            "the journal-owned observed_at_ms is the timer producer"
        );
    }

    #[test]
    fn unsupported_channel_and_resource_external_events_fail_closed_at_decode() {
        let base = serde_json::to_value(signal_delivery(
            "in-unsupported",
            1_700_000_002_000,
            "delivery-unsupported",
            1,
            logical_signal("sig", SignalUrgency::Normal),
        ))
        .unwrap();
        for kind in ["channel_ready", "resource_released"] {
            let mut forged = base.clone();
            forged["input"]["event"] = json!({"kind": kind});
            assert!(
                serde_json::from_value::<WireEnvelope>(forged).is_err(),
                "unsupported ExternalEvent {kind:?} must not decode as a no-op"
            );
        }
    }

    // fixture: cancel-flows-only-through-host-control
    #[test]
    fn cancellation_enters_only_through_the_control_plane_and_never_as_a_failure() {
        // an effect failure on the very call a cancel would abandon is a *failed* terminal, and no
        // wire shape lets it become a cancellation
        let (mut runtime, provider) = agent_awaiting_provider();
        let failed_out = runtime.submit(&failed(
            "in-transport-dead",
            1_700_000_002_000,
            &provider,
            HostEffectFailureKind::TransportExhausted,
            "the vendor gave up",
        ));
        assert!(
            matches!(failed_out.terminal(), Some(KernelTerminal::Failed(_))),
            "a host effect failure is never a cancellation (§14.3)"
        );

        // the control plane is the path that produces a cancellation
        let (mut runtime, _) = agent_awaiting_provider();
        let cancelled = runtime.submit(&cancel_with(
            "in-cancel",
            1_700_000_002_000,
            CancellationReason::Deadline,
        ));
        let Some(KernelTerminal::Cancelled(terminal)) = cancelled.terminal() else {
            panic!(
                "expected a cancelled terminal, got {:?}",
                cancelled.terminal()
            );
        };
        assert_eq!(
            terminal.reason,
            CancellationReason::Deadline,
            "the reason is the host's, not the loop's internal user-abort"
        );
        assert!(
            observation_kinds(&runtime).contains(&"operation_cancelled"),
            "{:?}",
            observation_kinds(&runtime)
        );

        // identity comes from the envelope alone: a cancel that repeats it does not decode, and a
        // cancel addressed at another operation never reaches the driver
        assert!(
            serde_json::from_value::<CancelCommand>(
                json!({ "reason": "user", "operation_id": "op-driver-1" })
            )
            .is_err(),
            "the envelope owns the operation id (§7.5)"
        );
        let (mut runtime, _) = agent_awaiting_provider();
        let mut foreign = cancel("in-foreign", 1_700_000_002_000);
        foreign.operation_id = OperationId::new("op-someone-else").unwrap();
        assert_eq!(
            runtime.reject(&foreign).code,
            KernelFaultCode::OperationMismatch
        );
    }

    // fixture: cancel-order-is-downstream-first
    #[test]
    fn cancellation_settles_every_downstream_wait_before_it_commits_the_root_terminal() {
        let (mut runtime, _) = workflow_with_live_child();
        assert!(
            runtime.driver.attempts.contains_key("wf-node0"),
            "the arc starts with a live child attempt"
        );

        let cancelled = runtime.submit(&cancel("in-cancel", 1_700_000_003_000));

        // downstream first, inside this one transition
        assert_eq!(
            runtime
                .driver
                .engine()
                .and_then(|engine| engine.task_lifecycle("wf-node0"))
                .map(TaskLifecycle::is_terminal),
            Some(true),
            "a running child is settled by the same step that cancels its parent"
        );
        assert!(
            runtime.driver.attempts.is_empty(),
            "a settled attempt is spent, so nothing downstream can be resumed by a late completion"
        );
        assert_eq!(
            runtime.pending_effect_kinds(),
            Vec::<EffectKindTag>::new(),
            "§11.1 · the cancelling step leaves nothing waiting on the host"
        );

        // and the root terminal is what that same step committed
        assert!(matches!(
            cancelled.terminal(),
            Some(KernelTerminal::Cancelled(_))
        ));
        assert!(
            cancelled.published_effects().is_empty(),
            "§7.12 · effects or a terminal, never both"
        );

        // the child's late completion cannot revive the operation
        assert_eq!(
            runtime
                .reject(&child_done(
                    "in-late-done",
                    1_700_000_004_000,
                    "wf-node0",
                    "finished anyway"
                ))
                .code,
            KernelFaultCode::InvalidLifecycle,
        );
    }

    // fixture: cancel-is-idempotent
    #[test]
    fn a_second_cancellation_commits_no_second_terminal() {
        let (mut runtime, _) = agent_awaiting_provider();
        let first = runtime.submit(&cancel("in-cancel", 1_700_000_002_000));
        let head = runtime.tx.head().map(|head| head.step_seq);

        // a re-issued cancellation under a fresh input id replays the record that already
        // committed the terminal (§18.3) — no second record, no second terminal, no new effect
        let replay = runtime.prepare(&cancel("in-cancel-again", 1_700_000_003_000));
        assert_eq!(replay.step_seq(), Some(first.step_seq));
        assert!(
            replay.record().unwrap().record_digest() == first.record.record_digest(),
            "the replay names the existing record"
        );
        assert_eq!(
            runtime.tx.head().map(|head| head.step_seq),
            head,
            "a replay moves no head"
        );

        // a cancellation that says something *different* is a conflict, not an overwrite
        assert_eq!(
            runtime
                .reject(&cancel_with(
                    "in-cancel-other",
                    1_700_000_003_000,
                    CancellationReason::LeaseLost
                ))
                .code,
            KernelFaultCode::DuplicateInputConflict,
        );
    }

    // fixture: cancel-terminal-rejects-all-state-changing-input
    #[test]
    fn a_terminal_refuses_every_state_changing_input_including_signal_delivery() {
        let (mut runtime, provider) = agent_awaiting_provider();
        runtime.submit(&cancel("in-cancel", 1_700_000_002_000));

        let head = runtime.tx.head().map(|head| head.step_seq);
        let queue_before = signal_queue_depth(&runtime);
        let journal_len = runtime.journal.len();

        for envelope in [
            signal_delivery(
                "in-late-signal",
                1_700_000_003_000,
                "delivery-late",
                1,
                logical_signal("sig-late", SignalUrgency::Critical),
            ),
            child_done("in-late-child", 1_700_000_003_000, "wf-node0", "done"),
            provider_answer("in-late-answer", 1_700_000_003_000, &provider, "too late"),
            agent_start("in-restart", 1_700_000_003_000),
            control(
                "in-late-compact",
                1_700_000_003_000,
                HostCommand::ForceCompact(super::super::command::ForceCompactCommand {}),
            ),
            control(
                "in-late-task",
                1_700_000_003_000,
                HostCommand::UpdateTask(UpdateTaskCommand {
                    update: WireTaskUpdate {
                        progress: Some("still going".to_string()),
                        ..WireTaskUpdate::default()
                    },
                }),
            ),
        ] {
            let input_id = envelope.input_id.clone();
            assert_eq!(
                runtime.reject(&envelope).code,
                KernelFaultCode::InvalidLifecycle,
                "{input_id} must be refused after the terminal",
            );
        }

        assert_eq!(
            runtime.tx.head().map(|head| head.step_seq),
            head,
            "no step sequence advanced"
        );
        assert_eq!(runtime.journal.len(), journal_len, "nothing was journaled");
        assert_eq!(
            signal_queue_depth(&runtime),
            queue_before,
            "a refused signal never reaches the queue"
        );
        assert!(
            runtime.driver.poison().is_none(),
            "a typed rejection is not a driver failure"
        );
    }

    fn signal_queue_depth(runtime: &Runtime) -> usize {
        runtime
            .driver
            .engine()
            .map(LoopStateMachine::signal_queue_depth)
            .unwrap_or(0)
    }

    // fixture: signal-delivery-identity-is-distinct
    #[test]
    fn delivery_identity_and_signal_identity_are_two_separate_facts() {
        let mut runtime = Runtime::new();
        runtime.submit(&signal_config(8, None));
        runtime.submit(&agent_start("in-start", 1_700_000_001_000));

        let first = signal_delivery(
            "in-sig-1",
            1_700_000_002_000,
            "delivery-a",
            1,
            LogicalSignal {
                dedupe_key: Some("nightly".to_string()),
                ..logical_signal("sig-nightly", SignalUrgency::Normal)
            },
        );
        runtime.submit(&first);
        assert_eq!(
            dispositions(&runtime),
            vec![(
                "queue".to_string(),
                "sig-nightly".to_string(),
                "delivery-a".to_string(),
                1
            )],
            "the audit fact names the caller's own signal id, never a minted one"
        );

        // the same delivery, retried: the envelope's idempotency key answers it as a replay, so the
        // signal is not disposed of twice
        let depth = signal_queue_depth(&runtime);
        let replay = runtime.prepare(&first);
        assert!(matches!(
            replay,
            super::super::fault::KernelPreparation::Replayed(_)
        ));
        assert_eq!(signal_queue_depth(&runtime), depth);

        // a *new* delivery of the same business signal is a distinct delivery attempt, and the
        // business dedupe key is what stops it becoming a second queued signal
        runtime.submit(&signal_delivery(
            "in-sig-2",
            1_700_000_003_000,
            "delivery-b",
            2,
            LogicalSignal {
                dedupe_key: Some("nightly".to_string()),
                ..logical_signal("sig-nightly", SignalUrgency::Normal)
            },
        ));
        assert_eq!(
            dispositions(&runtime),
            vec![(
                "ignore".to_string(),
                "sig-nightly".to_string(),
                "delivery-b".to_string(),
                2
            )],
            "a redelivery is recognisable as the same signal and a different delivery"
        );
        assert_eq!(
            signal_queue_depth(&runtime),
            depth,
            "and it does not queue a second copy"
        );

        // a delivery that cannot say which attempt it is fails closed
        assert_eq!(
            runtime
                .reject(&signal_delivery(
                    "in-sig-0",
                    1_700_000_004_000,
                    "delivery-c",
                    0,
                    logical_signal("sig-other", SignalUrgency::Normal),
                ))
                .code,
            KernelFaultCode::MalformedEnvelope,
        );
    }

    // fixture: signal-admission-uses-accepted-time
    #[test]
    fn signal_ttl_is_measured_from_the_accepted_envelope_time() {
        let mut runtime = Runtime::new();
        runtime.submit(&signal_config(8, Some(60_000)));
        runtime.submit(&agent_start("in-start", 1_700_000_001_000));

        // a source timestamp far older than the TTL — if admission read it, this signal would be
        // born expired
        runtime.submit(&signal_delivery(
            "in-sig-1",
            1_700_000_002_000,
            "delivery-a",
            1,
            LogicalSignal {
                source_timestamp_ms: Some(WireU64::new(1_600_000_000_000)),
                ..logical_signal("sig-stale-source", SignalUrgency::Normal)
            },
        ));
        assert_eq!(
            dispositions(&runtime)
                .iter()
                .map(|(disposition, ..)| disposition.clone())
                .collect::<Vec<_>>(),
            vec!["queue".to_string()],
            "admission uses the accepted envelope time; the source timestamp is metadata"
        );
        assert_eq!(signal_queue_depth(&runtime), 1);

        // and expiry is measured on the same clock: the next accepted input is far enough past the
        // *accepted* time of the first signal to expire it
        runtime.submit(&signal_delivery(
            "in-sig-2",
            1_700_000_200_000,
            "delivery-b",
            1,
            logical_signal("sig-fresh", SignalUrgency::Normal),
        ));
        assert!(
            observation_kinds(&runtime).contains(&"signal_expired"),
            "{:?}",
            observation_kinds(&runtime)
        );
        assert_eq!(
            signal_queue_depth(&runtime),
            1,
            "the stale signal left, the fresh one stayed"
        );
    }

    // fixture: signal-target-is-operation-or-task
    #[test]
    fn a_signal_addresses_the_operation_or_one_of_its_own_tasks() {
        let (mut runtime, _) = workflow_with_live_child();

        // the operation itself
        runtime.submit(&signal_delivery(
            "in-sig-op",
            1_700_000_003_000,
            "delivery-a",
            1,
            logical_signal("sig-op", SignalUrgency::Normal),
        ));
        assert_eq!(dispositions(&runtime).len(), 1);

        // one of its own logical tasks
        runtime.submit(&signal_delivery(
            "in-sig-task",
            1_700_000_004_000,
            "delivery-b",
            1,
            LogicalSignal {
                target: SignalTarget::Task(super::super::event::TaskTarget {
                    task_id: TaskId::new("wf-node0").unwrap(),
                }),
                ..logical_signal("sig-task", SignalUrgency::Normal)
            },
        ));
        assert_eq!(dispositions(&runtime).len(), 1);

        // a task this operation does not have
        assert_eq!(
            runtime
                .reject(&signal_delivery(
                    "in-sig-ghost",
                    1_700_000_005_000,
                    "delivery-c",
                    1,
                    LogicalSignal {
                        target: SignalTarget::Task(super::super::event::TaskTarget {
                            task_id: TaskId::new("ghost").unwrap(),
                        }),
                        ..logical_signal("sig-ghost", SignalUrgency::Normal)
                    },
                ))
                .code,
            KernelFaultCode::InvalidAuthority,
        );

        // a host session is not an address, and the wire has no slot for one
        assert!(
            serde_json::from_value::<SignalTarget>(
                json!({ "kind": "task", "task_id": "wf-node0", "session_id": "sess-1" })
            )
            .is_err(),
            "host session identity does not enter the event (§7.7)"
        );
    }

    // fixture: signal-target-is-operation-or-task
    #[test]
    fn a_full_signal_queue_drops_by_policy_and_leaves_an_audit_fact() {
        let mut runtime = Runtime::new();
        runtime.submit(&signal_config(1, None));
        runtime.submit(&agent_start("in-start", 1_700_000_001_000));

        runtime.submit(&signal_delivery(
            "in-sig-1",
            1_700_000_002_000,
            "delivery-a",
            1,
            logical_signal("sig-1", SignalUrgency::Normal),
        ));
        runtime.submit(&signal_delivery(
            "in-sig-2",
            1_700_000_003_000,
            "delivery-b",
            1,
            logical_signal("sig-2", SignalUrgency::Normal),
        ));
        assert_eq!(
            dispositions(&runtime)
                .iter()
                .map(|(disposition, ..)| disposition.clone())
                .collect::<Vec<_>>(),
            vec!["dropped".to_string()],
            "the configured capacity decides, and the loss is an audit fact"
        );
        assert_eq!(signal_queue_depth(&runtime), 1);
    }

    // fixture: signal-disposition-is-a-fact
    #[test]
    fn a_signal_disposition_is_a_fact_and_only_a_preemption_asks_the_host_for_anything() {
        // an ordinary signal: an audit fact and nothing to execute
        let mut runtime = Runtime::new();
        runtime.submit(&signal_config(8, None));
        runtime.submit(&agent_start("in-start", 1_700_000_001_000));
        let queued = runtime.submit(&signal_delivery(
            "in-sig-normal",
            1_700_000_002_000,
            "delivery-a",
            1,
            logical_signal("sig-normal", SignalUrgency::Normal),
        ));
        assert!(
            queued.published_effects().is_empty(),
            "queueing is a fact; it asks the host for nothing"
        );
        assert!(observation_kinds(&runtime).contains(&"signal_delivery_disposed"));

        // an urgent one that must stop running children *is* a host action, published as an effect
        let (mut runtime, _) = workflow_with_live_child();
        let interrupted = runtime.submit(&signal_delivery(
            "in-sig-critical",
            1_700_000_003_000,
            "delivery-b",
            1,
            logical_signal("sig-critical", SignalUrgency::Critical),
        ));
        assert_eq!(kinds(&interrupted), vec![EffectKindTag::PreemptTasks]);
        assert!(
            !observation_kinds(&runtime).contains(&"agent_preempted"),
            "the preemption is requested here, not committed: {:?}",
            observation_kinds(&runtime)
        );

        // only the host's resolution commits it
        let preempt = effect_id(interrupted.step_seq);
        runtime.submit(&resolved(
            "in-preempted",
            1_700_000_004_000,
            &preempt,
            EffectSuccess::TasksPreempted(super::super::effect::TasksPreemptedSuccess {
                attempts: vec![super::super::effect::TaskPreemptOutcome {
                    task_id: TaskId::new("wf-node0").unwrap(),
                    attempt_id: WireAttemptId::new("wf-node0:attempt:1").unwrap(),
                    outcome: super::super::effect::TaskPreemptStatus::Preempted(
                        super::super::effect::TaskPreempted {},
                    ),
                }],
            }),
        ));
        assert!(
            observation_kinds(&runtime).contains(&"agent_preempted"),
            "{:?}",
            observation_kinds(&runtime)
        );
    }

    #[test]
    fn an_operation_that_can_be_interrupted_is_one_that_can_stop_its_children() {
        // DEC-8 · the driver's own pre-check for `preempt_tasks` support before a critical signal
        // routes can never fire, and this is why: an operation that declares spawn capacity is
        // refused at genesis unless it also declares that it can stop what it started. The
        // guarantee lives at config time, so the signal path cannot plan an effect the host would
        // have to refuse.
        let mut runtime = Runtime::new();
        let fault = runtime.reject(&syscall_config_with(|config| {
            config.host_effect_support = HostEffectSupport::new([
                EffectKindTag::CallProvider,
                EffectKindTag::ExecuteTools,
                EffectKindTag::LoadPayload,
                EffectKindTag::SpawnTasks,
                EffectKindTag::PersistMemory,
                EffectKindTag::QueryMemory,
            ]);
        }));
        assert_eq!(fault.code, KernelFaultCode::InvalidConfig);
        assert!(fault.message.contains("preempt_tasks"), "{fault:?}");
    }

    // ----- §7.7 · escalate_after_ms (Task 14, adjudication §5n item 1) --------------------------

    /// A signal config with deadline escalation switched on.
    fn escalating_signal_config(queue_max: u32) -> WireEnvelope {
        syscall_config_with(|config| {
            config.signal_policy = Some(super::super::command::SignalPolicy {
                queue_max,
                ttl_ms: None,
                deadline_escalation: Some(true),
            });
        })
    }

    fn signal_escalating_after(id: &str, urgency: SignalUrgency, after_ms: u64) -> LogicalSignal {
        LogicalSignal {
            escalate_after_ms: Some(WireU64::new(after_ms)),
            ..logical_signal(id, urgency)
        }
    }

    #[test]
    fn a_due_escalation_raises_urgency_one_tier_and_is_anchored_to_the_accepted_time() {
        // A `low` signal is only observed. The same signal with a due `escalate_after_ms` is
        // `normal`, which queues for the next turn boundary — one tier, exactly.
        let mut runtime = Runtime::new();
        runtime.submit(&escalating_signal_config(8));
        runtime.submit(&agent_start("in-start", 1_700_000_001_000));
        runtime.submit(&signal_delivery(
            "in-sig-due",
            1_700_000_002_000,
            "delivery-a",
            1,
            signal_escalating_after("sig-due", SignalUrgency::Low, 0),
        ));
        assert_eq!(
            dispositions(&runtime)
                .iter()
                .map(|(disposition, ..)| disposition.clone())
                .collect::<Vec<_>>(),
            vec!["queue".to_string()],
            "a due deadline escalated low → normal"
        );

        // Not yet due: the same signal, same accepted time, a deadline in the future. This is what
        // "anchored to the envelope's accepted time" buys — the kernel needs no clock of its own to
        // tell the two apart.
        let mut runtime = Runtime::new();
        runtime.submit(&escalating_signal_config(8));
        runtime.submit(&agent_start("in-start", 1_700_000_001_000));
        runtime.submit(&signal_delivery(
            "in-sig-waiting",
            1_700_000_002_000,
            "delivery-a",
            1,
            signal_escalating_after("sig-waiting", SignalUrgency::Low, 60_000),
        ));
        assert_eq!(
            dispositions(&runtime)
                .iter()
                .map(|(disposition, ..)| disposition.clone())
                .collect::<Vec<_>>(),
            vec!["observe".to_string()],
            "a deadline that has not come due changes nothing"
        );
    }

    #[test]
    fn escalation_is_inert_unless_the_operation_asked_for_it() {
        // The field is a request; `signal_policy.deadline_escalation` is the operation's consent.
        // Without it the same bytes decode to the same signal and route the same way.
        let mut runtime = Runtime::new();
        runtime.submit(&signal_config(8, None));
        runtime.submit(&agent_start("in-start", 1_700_000_001_000));
        runtime.submit(&signal_delivery(
            "in-sig",
            1_700_000_002_000,
            "delivery-a",
            1,
            signal_escalating_after("sig-inert", SignalUrgency::Low, 0),
        ));
        assert_eq!(
            dispositions(&runtime)
                .iter()
                .map(|(disposition, ..)| disposition.clone())
                .collect::<Vec<_>>(),
            vec!["observe".to_string()],
            "escalation without the policy is inert"
        );
    }

    #[test]
    fn an_escalation_that_reaches_critical_takes_the_whole_interrupt_arc() {
        // The full arc: a `high` signal that waited long enough becomes `critical`, and critical
        // while children are running is the one disposition that asks the host for something. This
        // is also what the driver's pre-admission `effective_urgency` check has to agree with — it
        // reads the escalated value precisely so a delivery that will publish a `PreemptTasks` is
        // adjudicated against DEC-8 before the router moves.
        let mut runtime = Runtime::new();
        runtime.submit(&escalating_signal_config(8));
        let started = runtime.submit(&workflow_start(
            "in-start",
            1_700_000_001_000,
            two_node_spec(),
        ));
        runtime.submit(&spawned(
            "in-ack",
            1_700_000_002_000,
            &effect_id(started.step_seq),
            &["wf-node0"],
        ));

        let interrupted = runtime.submit(&signal_delivery(
            "in-sig-escalated",
            1_700_000_003_000,
            "delivery-a",
            1,
            signal_escalating_after("sig-escalated", SignalUrgency::High, 0),
        ));
        assert_eq!(
            kinds(&interrupted),
            vec![EffectKindTag::PreemptTasks],
            "high + due deadline = critical, and critical while busy preempts"
        );

        // …and without the escalation the same signal is only a soft interrupt: nothing published.
        let mut runtime = Runtime::new();
        runtime.submit(&escalating_signal_config(8));
        let started = runtime.submit(&workflow_start(
            "in-start",
            1_700_000_001_000,
            two_node_spec(),
        ));
        runtime.submit(&spawned(
            "in-ack",
            1_700_000_002_000,
            &effect_id(started.step_seq),
            &["wf-node0"],
        ));
        let soft = runtime.submit(&signal_delivery(
            "in-sig-plain",
            1_700_000_003_000,
            "delivery-a",
            1,
            logical_signal("sig-plain", SignalUrgency::High),
        ));
        assert!(
            soft.published_effects().is_empty(),
            "an unescalated high signal waits for the next boundary"
        );
    }

    #[test]
    fn a_signal_carries_a_duration_not_a_deadline() {
        // DEC-2 · an absolute instant would be a second host clock on the wire, and the same
        // intent would decode to a different deadline on every redelivery. Only the duration
        // spelling exists.
        let signal = signal_escalating_after("sig", SignalUrgency::Normal, 30_000);
        let value = serde_json::to_value(&signal).unwrap();
        assert_eq!(value["escalate_after_ms"], json!("30000"));
        for banned in ["deadline_ms", "escalate_at_ms", "expires_at_ms", "now_ms"] {
            assert!(
                value.get(banned).is_none(),
                "a signal must not carry {banned}"
            );
            let mut with_instant = value.clone();
            with_instant
                .as_object_mut()
                .unwrap()
                .insert(banned.to_string(), json!("1700000000000"));
            assert!(
                serde_json::from_value::<LogicalSignal>(with_instant).is_err(),
                "{banned} must not decode"
            );
        }

        // and the field is optional: a signal that never escalates is the common case
        let plain = serde_json::to_value(logical_signal("sig", SignalUrgency::Normal)).unwrap();
        assert!(plain.get("escalate_after_ms").is_none());
    }

    #[test]
    fn an_urgent_signal_never_publishes_a_second_provider_request() {
        // DEC-3 · a provider call is already pending, so the interrupt is admitted to the attention
        // partition and read at the next turn boundary rather than re-asking now.
        let (mut runtime, provider) = agent_awaiting_provider();
        let interrupted = runtime.submit(&signal_delivery(
            "in-sig-critical",
            1_700_000_002_000,
            "delivery-a",
            1,
            logical_signal("sig-critical", SignalUrgency::Critical),
        ));
        assert!(
            interrupted.published_effects().is_empty(),
            "a second provider call would be refused by §15.3, so none is planned"
        );
        assert_eq!(
            dispositions(&runtime)
                .iter()
                .map(|(disposition, ..)| disposition.clone())
                .collect::<Vec<_>>(),
            vec!["interrupt".to_string()],
            "the disposition reports what actually happened"
        );

        // and the pending call still resolves normally, carrying the interrupt into that turn
        let answered = runtime.submit(&provider_result(
            "in-answer",
            1_700_000_003_000,
            &provider,
            vec![tool_call("call-1", "search", json!({"q": "now what"}))],
        ));
        assert_eq!(kinds(&answered), vec![EffectKindTag::ExecuteTools]);
    }

    // -----------------------------------------------------------------------------------------
    // §7.5 · the remaining host commands
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_host_task_update_and_a_model_task_update_share_a_payload_but_not_an_authority() {
        let (mut runtime, provider) = agent_awaiting_provider();
        let committed = runtime.submit(&control(
            "in-host-plan",
            1_700_000_002_000,
            HostCommand::UpdateTask(UpdateTaskCommand {
                update: WireTaskUpdate {
                    plan: Some(vec!["collect".to_string(), "write".to_string()]),
                    progress: Some("host set the plan".to_string()),
                    ..WireTaskUpdate::default()
                },
            }),
        ));
        assert!(
            committed.published_effects().is_empty() && committed.terminal().is_none(),
            "a control command changes kernel state and publishes nothing"
        );
        assert_eq!(
            runtime
                .driver
                .engine()
                .map(|engine| engine.ctx.partitions.task_state.progress.clone()),
            Some("host set the plan".to_string()),
        );

        // the model reaches the same mutation through the P1 syscall path, which *is* gated —
        // it must be a call the turn advertised, attributed to a derived caller
        let acted = runtime.submit(&provider_result(
            "in-model-plan",
            1_700_000_003_000,
            &provider,
            vec![tool_call(
                "call-1",
                "update_plan",
                json!({ "progress": "model set the plan" }),
            )],
        ));
        assert_eq!(kinds(&acted), vec![EffectKindTag::CallProvider]);
        assert_eq!(
            runtime
                .driver
                .engine()
                .map(|engine| engine.ctx.partitions.task_state.progress.clone()),
            Some("model set the plan".to_string()),
        );
    }

    #[test]
    fn seeded_and_mutated_knowledge_enter_the_same_partition() {
        let (mut runtime, _) = agent_awaiting_provider();
        runtime.submit(&control(
            "in-seed",
            1_700_000_002_000,
            HostCommand::SeedKnowledge(SeedKnowledgeCommand {
                entries: vec![super::super::root::KnowledgeEntry {
                    content: "the house style forbids bullet lists".to_string(),
                    key: Some("style".to_string()),
                    tokens: Some(9),
                    pinned: true,
                }],
            }),
        ));
        assert!(
            knowledge_text(&runtime)
                .iter()
                .any(|text| text.contains("house style")),
            "{:?}",
            knowledge_text(&runtime)
        );

        // the keyed removal half is boundary-deferred, so it is accepted here and swept later
        let committed = runtime.submit(&control(
            "in-knowledge",
            1_700_000_003_000,
            HostCommand::ApplyKnowledgeMutation(ApplyKnowledgeMutationCommand {
                mutation: super::super::command::KnowledgeMutation {
                    upsert: vec![super::super::root::KnowledgeEntry {
                        content: "cite at least three sources".to_string(),
                        key: Some("sources".to_string()),
                        tokens: Some(6),
                        pinned: false,
                    }],
                    remove: vec!["style".to_string(), "never-seen".to_string()],
                },
            }),
        ));
        assert!(committed.published_effects().is_empty());
        assert!(
            knowledge_text(&runtime)
                .iter()
                .any(|text| text.contains("three sources")),
            "{:?}",
            knowledge_text(&runtime)
        );
    }

    fn knowledge_text(runtime: &Runtime) -> Vec<String> {
        runtime
            .driver
            .engine()
            .map(|engine| {
                engine
                    .ctx
                    .partitions
                    .knowledge
                    .messages()
                    .map(message_text)
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn a_capability_patch_mounts_and_unmounts_in_one_step() {
        use super::super::root::{CapabilityGrant, CapabilityKind as WireCapabilityKind};

        let (mut runtime, _) = agent_awaiting_provider();
        runtime.submit(&control(
            "in-mount",
            1_700_000_002_000,
            HostCommand::ApplyCapabilityPatch(ApplyCapabilityPatchCommand {
                patch: super::super::command::CapabilityPatch {
                    mount: vec![CapabilityGrant {
                        kind: WireCapabilityKind::McpServer,
                        id: "github".to_string(),
                        description: Some("issue and PR access".to_string()),
                    }],
                    unmount: Vec::new(),
                },
            }),
        ));
        assert!(observation_kinds(&runtime).contains(&"capability_changed"));
        assert!(
            runtime
                .driver
                .engine()
                .unwrap()
                .ctx
                .capabilities
                .capabilities()
                .iter()
                .any(|capability| capability.id == "github")
        );

        // withdrawing something already absent errs open, so a retry is safe
        runtime.submit(&control(
            "in-unmount",
            1_700_000_003_000,
            HostCommand::ApplyCapabilityPatch(ApplyCapabilityPatchCommand {
                patch: super::super::command::CapabilityPatch {
                    mount: Vec::new(),
                    unmount: vec![
                        super::super::root::CapabilityRef {
                            kind: WireCapabilityKind::McpServer,
                            id: "github".to_string(),
                        },
                        super::super::root::CapabilityRef {
                            kind: WireCapabilityKind::McpServer,
                            id: "never-mounted".to_string(),
                        },
                    ],
                },
            }),
        ));
        assert!(
            !runtime
                .driver
                .engine()
                .unwrap()
                .ctx
                .capabilities
                .capabilities()
                .iter()
                .any(|capability| capability.id == "github")
        );
    }

    #[test]
    fn a_skill_swap_is_atomic_and_refuses_a_name_outside_the_catalog() {
        let (mut runtime, _) = agent_awaiting_provider();
        let before = runtime.driver.engine().unwrap().ctx.active_skills.len();

        // one undeclared name refuses the whole swap — nothing is half-applied
        assert_eq!(
            runtime
                .reject(&control(
                    "in-bad-skill",
                    1_700_000_002_000,
                    HostCommand::ApplySkillActivation(ApplySkillActivationCommand {
                        activate: vec![
                            super::super::command::SkillActivation {
                                name: "debug".to_string(),
                                lease_turns: None,
                            },
                            super::super::command::SkillActivation {
                                name: "invented".to_string(),
                                lease_turns: None,
                            },
                        ],
                        deactivate: Vec::new(),
                    }),
                ))
                .code,
            KernelFaultCode::InvalidConfig,
        );
        assert_eq!(
            runtime.driver.engine().unwrap().ctx.active_skills.len(),
            before,
            "a refused swap left nothing behind"
        );

        runtime.submit(&control(
            "in-skill",
            1_700_000_002_000,
            HostCommand::ApplySkillActivation(ApplySkillActivationCommand {
                activate: vec![super::super::command::SkillActivation {
                    name: "debug".to_string(),
                    lease_turns: Some(2),
                }],
                deactivate: Vec::new(),
            }),
        ));
        assert!(
            runtime
                .driver
                .engine()
                .unwrap()
                .ctx
                .active_skills
                .iter()
                .any(|(skill, _)| skill == "debug")
        );
    }

    #[test]
    fn skill_capability_grants_must_attenuate_root_authority_before_activation() {
        use crate::types::capability::{
            ActionSet, Capability, CapabilityId, CapabilityKind, ConstraintSet, Principal,
            ResourceSelector,
        };

        let root_grant = Capability {
            id: CapabilityId("root-read-src".into()),
            kind: CapabilityKind::Tool,
            resource: ResourceSelector("/repo/src/**".into()),
            actions: ActionSet(["read".into()].into_iter().collect()),
            constraints: ConstraintSet::default(),
            lease: None,
            delegatable: true,
            issuer: Principal("root".into()),
        };
        let overbroad_skill_grant = Capability {
            id: CapabilityId("read-repo".into()),
            kind: CapabilityKind::Tool,
            resource: ResourceSelector("/repo/**".into()),
            actions: ActionSet(["read".into()].into_iter().collect()),
            constraints: ConstraintSet::default(),
            lease: None,
            delegatable: false,
            issuer: Principal("skill:debug".into()),
        };

        let mut runtime = Runtime::new();
        runtime.submit(&syscall_config_with(|config| {
            config.skill_catalog[0].capability_grants = vec![overbroad_skill_grant];
        }));
        runtime.submit(&agent_start_with_capabilities(
            "in-start",
            1_700_000_001_000,
            vec![root_grant],
        ));
        let before = runtime.driver.engine().unwrap().ctx.active_skills.clone();

        let fault = runtime.reject(&control(
            "in-overbroad-skill",
            1_700_000_002_000,
            HostCommand::ApplySkillActivation(ApplySkillActivationCommand {
                activate: vec![super::super::command::SkillActivation {
                    name: "debug".to_string(),
                    lease_turns: None,
                }],
                deactivate: Vec::new(),
            }),
        ));
        assert_eq!(fault.code, KernelFaultCode::InvalidAuthority);
        assert_eq!(runtime.driver.engine().unwrap().ctx.active_skills, before);
    }

    #[test]
    fn model_skill_activation_rejects_overbroad_grants_as_an_audit_fact() {
        use crate::types::capability::{
            ActionSet, Capability, CapabilityId, CapabilityKind, ConstraintSet, Principal,
            ResourceSelector,
        };

        let root_grant = Capability {
            id: CapabilityId("root-read-src".into()),
            kind: CapabilityKind::Tool,
            resource: ResourceSelector("/repo/src/**".into()),
            actions: ActionSet(["read".into()].into_iter().collect()),
            constraints: ConstraintSet::default(),
            lease: None,
            delegatable: true,
            issuer: Principal("root".into()),
        };
        let overbroad_skill_grant = Capability {
            id: CapabilityId("read-repo".into()),
            kind: CapabilityKind::Tool,
            resource: ResourceSelector("/repo/**".into()),
            actions: ActionSet(["read".into()].into_iter().collect()),
            constraints: ConstraintSet::default(),
            lease: None,
            delegatable: false,
            issuer: Principal("skill:debug".into()),
        };

        let mut runtime = Runtime::new();
        runtime.submit(&syscall_config_with(|config| {
            config.skill_catalog[0].capability_grants = vec![overbroad_skill_grant];
        }));
        let started = runtime.submit(&agent_start_with_capabilities(
            "in-start",
            1_700_000_001_000,
            vec![root_grant],
        ));
        let provider = sole_effect(&started).effect_id.clone();

        runtime.submit(&provider_result(
            "in-overbroad-skill",
            1_700_000_002_000,
            &provider,
            vec![tool_call("call-1", "skill", json!({"name": "debug"}))],
        ));

        let rejected = rejections(&runtime);
        assert_eq!(rejected.len(), 1);
        assert_eq!(rejected[0].0, "skill");
        assert!(rejected[0].2.contains("would widen"));
        assert!(
            runtime
                .driver
                .engine()
                .unwrap()
                .ctx
                .active_skills
                .is_empty()
        );
        assert_eq!(
            runtime.pending_effect_kinds(),
            vec![EffectKindTag::CallProvider]
        );
    }

    #[test]
    fn skill_capability_grants_are_effective_only_for_a_legal_active_skill() {
        use crate::types::capability::{
            ActionSet, Capability, CapabilityId, CapabilityKind, ConstraintSet, Principal,
            ResourceSelector,
        };

        let root_grant = Capability {
            id: CapabilityId("root-read-src".into()),
            kind: CapabilityKind::Tool,
            resource: ResourceSelector("/repo/src/**".into()),
            actions: ActionSet(["read".into(), "write".into()].into_iter().collect()),
            constraints: ConstraintSet::default(),
            lease: None,
            delegatable: true,
            issuer: Principal("root".into()),
        };
        let narrowed_skill_grant = Capability {
            id: CapabilityId("read-utils".into()),
            kind: CapabilityKind::Tool,
            resource: ResourceSelector("/repo/src/utils/**".into()),
            actions: ActionSet(["read".into()].into_iter().collect()),
            constraints: ConstraintSet::default(),
            lease: None,
            delegatable: false,
            issuer: Principal("skill:debug".into()),
        };

        let mut runtime = Runtime::new();
        runtime.submit(&syscall_config_with(|config| {
            config.skill_catalog[0].capability_grants = vec![narrowed_skill_grant.clone()];
        }));
        runtime.submit(&agent_start_with_capabilities(
            "in-start",
            1_700_000_001_000,
            vec![root_grant],
        ));
        runtime.submit(&control(
            "in-narrowed-skill",
            1_700_000_002_000,
            HostCommand::ApplySkillActivation(ApplySkillActivationCommand {
                activate: vec![super::super::command::SkillActivation {
                    name: "debug".to_string(),
                    lease_turns: None,
                }],
                deactivate: Vec::new(),
            }),
        ));
        assert_eq!(
            runtime
                .driver
                .engine()
                .unwrap()
                .ctx
                .active_skill_capabilities(),
            vec![narrowed_skill_grant]
        );

        runtime.submit(&control(
            "in-deactivate-skill",
            1_700_000_003_000,
            HostCommand::ApplySkillActivation(ApplySkillActivationCommand {
                activate: Vec::new(),
                deactivate: vec!["debug".to_string()],
            }),
        ));
        assert!(
            runtime
                .driver
                .engine()
                .unwrap()
                .ctx
                .active_skill_capabilities()
                .is_empty()
        );
    }

    #[test]
    fn a_live_policy_patch_is_revision_guarded_and_takes_effect() {
        let mut runtime = Runtime::new();
        runtime.submit(&signal_config(8, None));
        runtime.submit(&agent_start("in-start", 1_700_000_001_000));

        // a stale revision changes nothing at all
        let stale = control(
            "in-stale-policy",
            1_700_000_002_000,
            HostCommand::ApplyPolicyPatch(ApplyPolicyPatchCommand {
                expected_revision: WireU64::new(7),
                patch: super::super::command::LivePolicyPatch::ReplaceSignalPolicy(
                    super::super::command::ReplaceSignalPolicy {
                        policy: super::super::command::SignalPolicy {
                            queue_max: 1,
                            ttl_ms: None,
                            deadline_escalation: None,
                        },
                    },
                ),
            }),
        );
        let fault = runtime.reject(&stale);
        assert_eq!(fault.code, KernelFaultCode::InvalidConfig);
        assert!(fault.message.contains("revision mismatch"), "{fault:?}");
        assert_eq!(
            runtime
                .driver
                .policy
                .as_ref()
                .map(LivePolicyState::revision),
            Some(WireU64::ZERO),
        );

        // a widening quota is refused rather than clamped
        let widening = control(
            "in-widen",
            1_700_000_002_000,
            HostCommand::ApplyPolicyPatch(ApplyPolicyPatchCommand {
                expected_revision: WireU64::ZERO,
                patch: super::super::command::LivePolicyPatch::TightenResourceQuota(
                    super::super::command::TightenResourceQuota {
                        max_workflow_nodes: Some(999),
                        ..Default::default()
                    },
                ),
            }),
        );
        assert!(
            runtime
                .reject(&widening)
                .message
                .contains("may only tighten")
        );

        // a well-formed patch applies, advances the revision, and reaches the running engine
        runtime.submit(&control(
            "in-policy",
            1_700_000_002_000,
            HostCommand::ApplyPolicyPatch(ApplyPolicyPatchCommand {
                expected_revision: WireU64::ZERO,
                patch: super::super::command::LivePolicyPatch::ReplaceSignalPolicy(
                    super::super::command::ReplaceSignalPolicy {
                        policy: super::super::command::SignalPolicy {
                            queue_max: 1,
                            ttl_ms: None,
                            deadline_escalation: None,
                        },
                    },
                ),
            }),
        ));
        assert_eq!(
            runtime
                .driver
                .policy
                .as_ref()
                .map(LivePolicyState::revision),
            Some(WireU64::new(1)),
        );
        assert!(
            observation_kinds(&runtime).contains(&"live_policy_changed"),
            "{:?}",
            observation_kinds(&runtime)
        );

        // the new capacity is the one the router enforces from here on
        runtime.submit(&signal_delivery(
            "in-sig-1",
            1_700_000_003_000,
            "delivery-a",
            1,
            logical_signal("sig-1", SignalUrgency::Normal),
        ));
        runtime.submit(&signal_delivery(
            "in-sig-2",
            1_700_000_004_000,
            "delivery-b",
            1,
            logical_signal("sig-2", SignalUrgency::Normal),
        ));
        assert_eq!(
            dispositions(&runtime)
                .iter()
                .map(|(disposition, ..)| disposition.clone())
                .collect::<Vec<_>>(),
            vec!["dropped".to_string()],
            "the patched queue_max is what the next admission decision reads"
        );
    }

    #[test]
    fn an_absolute_deadline_becomes_the_wall_budget_axis() {
        let (mut runtime, provider) = agent_awaiting_provider();
        // the operation's clock started at the root start
        runtime.submit(&control(
            "in-deadline",
            1_700_000_002_000,
            HostCommand::UpdateDeadline(UpdateDeadlineCommand {
                deadline_ms: Some(WireU64::new(1_700_000_001_500)),
            }),
        ));

        // the axis is read at the one funnel that issues a provider request, so the deadline is
        // seen the next time the loop asks a question rather than mid-flight
        let acted = runtime.submit(&provider_result(
            "in-acted",
            1_700_000_003_000,
            &provider,
            vec![tool_call("call-1", "search", json!({ "q": "sources" }))],
        ));
        let results = runtime.submit(&tools_resolved(
            "in-results",
            1_700_000_004_000,
            &effect_id(acted.step_seq),
            &[("call-1", "three sources", false)],
        ));
        assert_eq!(
            kinds(&results),
            vec![EffectKindTag::CallProvider],
            "an exhausted budget still buys exactly one bounded final turn"
        );

        let ended = runtime.submit(&provider_answer(
            "in-answer",
            1_700_000_005_000,
            &effect_id(results.step_seq),
            "half a thought",
        ));
        let Some(KernelTerminal::Agent(agent)) = ended.terminal() else {
            panic!("expected the deadline to end the operation, got {ended:?}");
        };
        assert_eq!(agent.result.termination, WireTermination::Deadline);
    }

    #[test]
    fn a_forced_compaction_publishes_the_archive_it_produced() {
        let mut runtime = Runtime::new();
        runtime.submit(&syscall_config_with(|config| {
            config.host_effect_support = support_with([EffectKindTag::ArchivePageOut]);
        }));
        runtime.submit(&agent_start_with_history("in-start", 1_700_000_001_000, 14));

        let compacted = runtime.submit(&control(
            "in-compact",
            1_700_000_002_000,
            HostCommand::ForceCompact(super::super::command::ForceCompactCommand {}),
        ));
        assert_eq!(kinds(&compacted), vec![EffectKindTag::ArchivePageOut]);
        assert!(observation_kinds(&runtime).contains(&"compressed"));
        assert!(
            compacted
                .step
                .observations
                .iter()
                .any(|observation| matches!(observation, KernelObservation::Compressed { .. })),
            "the committed planned step is the host publication channel for observations"
        );
    }

    // -----------------------------------------------------------------------------------------
    // lifecycle goldens
    // -----------------------------------------------------------------------------------------

    fn fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/kernel-wire")
    }

    fn golden(name: &str, produced: &Value) -> Value {
        let path = fixture_dir().join(name);
        if std::env::var("BLESS_KERNEL_RECORD_FIXTURES").as_deref() == Ok("1") {
            let mut text = serde_json::to_string_pretty(produced).unwrap();
            text.push('\n');
            fs::write(&path, text).unwrap_or_else(|e| panic!("cannot bless {name}: {e}"));
            return produced.clone();
        }
        let raw = fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!("missing golden {name} ({e}); re-bless with BLESS_KERNEL_RECORD_FIXTURES=1")
        });
        serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{name} is not JSON: {e}"))
    }

    fn link(envelope: &WireEnvelope, committed: &CommittedTransition<PlannedStep>) -> Value {
        json!({
            "envelope": serde_json::to_value(envelope).unwrap(),
            "step": serde_json::to_value(&committed.step).unwrap(),
            "record": serde_json::to_value(&committed.record).unwrap(),
        })
    }

    #[test]
    fn golden_lifecycle_agent_root() {
        let mut runtime = Runtime::new();
        let configure_envelope = configure();
        let start_envelope = agent_start("in-start", 1_700_000_001_000);
        let genesis = runtime.submit(&configure_envelope);
        let started = runtime.submit(&start_envelope);

        let produced = json!({
            "description":
                "Configure → atomic agent root start (spec 6, 7.4). Two accepted inputs reach the \
                 first provider call: the genesis record freezes the resolved configuration, and \
                 the start record's step carries the immutable root kind, the initial execution \
                 focus and the one CallProvider effect the transition published.",
            "genesis_digest": genesis.record.record_digest().as_str(),
            "head_digest": started.record.record_digest().as_str(),
            "links": [
                link(&configure_envelope, &genesis),
                link(&start_envelope, &started),
            ],
        });

        let expected = golden("golden_lifecycle_agent_root.json", &produced);
        assert_eq!(produced, expected, "the agent root lifecycle drifted");
        assert_eq!(expected["links"][1]["step"]["root_kind"], json!("agent"));
        assert_eq!(
            expected["links"][1]["step"]["disposition"]["effects"][0]["effect"]["kind"],
            json!("call_provider"),
        );
    }

    #[test]
    fn golden_lifecycle_agent_full_turn() {
        let mut runtime = Runtime::new();
        let configure_envelope = syscall_config();
        let start_envelope = agent_start("in-start", 1_700_000_001_000);
        let genesis = runtime.submit(&configure_envelope);
        let started = runtime.submit(&start_envelope);

        let acted_envelope = provider_result(
            "in-acted",
            1_700_000_002_000,
            &effect_id(started.step_seq),
            vec![tool_call("call-1", "search", json!({"q": "sources"}))],
        );
        let acted = runtime.submit(&acted_envelope);
        let results_envelope = tools_resolved(
            "in-results",
            1_700_000_003_000,
            &effect_id(acted.step_seq),
            &[("call-1", "three sources found", false)],
        );
        let results = runtime.submit(&results_envelope);
        let answer_envelope = provider_answer(
            "in-answer",
            1_700_000_004_000,
            &effect_id(results.step_seq),
            "the brief cites three sources",
        );
        let answered = runtime.submit(&answer_envelope);

        let produced = json!({
            "description":
                "One whole agent turn cycle on the canonical wire (spec 7.9 · Task 12): configure → \
                 atomic agent start → provider result carrying a tool call → tool results → final \
                 provider answer → agent terminal. Every step after the start is a single \
                 `ResolveEffect` input, and each one publishes exactly the next effect the loop is \
                 waiting on — until the last, whose disposition is the terminal itself and which \
                 publishes nothing.",
            "genesis_digest": genesis.record.record_digest().as_str(),
            "head_digest": answered.record.record_digest().as_str(),
            "links": [
                link(&configure_envelope, &genesis),
                link(&start_envelope, &started),
                link(&acted_envelope, &acted),
                link(&results_envelope, &results),
                link(&answer_envelope, &answered),
            ],
        });

        let expected = golden("golden_lifecycle_agent_full_turn.json", &produced);
        assert_eq!(produced, expected, "the agent turn cycle drifted");
        assert_eq!(
            expected["links"][2]["step"]["disposition"]["effects"][0]["effect"]["kind"],
            json!("execute_tools"),
            "a provider result carrying a host tool call publishes exactly one tool batch",
        );
        assert_eq!(
            expected["links"][3]["step"]["disposition"]["effects"][0]["effect"]["kind"],
            json!("call_provider"),
            "the tool results resume the turn with the next provider call",
        );
        assert_eq!(
            expected["links"][4]["step"]["disposition"]["terminal"]["kind"],
            json!("agent"),
        );
        assert_eq!(
            expected["links"][4]["step"]["disposition"]["effects"],
            Value::Null,
            "§7.12 · effects or a terminal, never both",
        );
    }

    #[test]
    fn golden_lifecycle_workflow_root() {
        let mut runtime = Runtime::new();
        let configure_envelope = configure();
        let start_envelope = workflow_start("in-start", 1_700_000_001_000, two_node_spec());
        let genesis = runtime.submit(&configure_envelope);
        let started = runtime.submit(&start_envelope);

        let ack_envelope = spawned(
            "in-ack-1",
            1_700_000_002_000,
            &effect_id(started.step_seq),
            &["wf-node0"],
        );
        let ack = runtime.submit(&ack_envelope);
        let done_envelope = child_done("in-done-1", 1_700_000_003_000, "wf-node0", "collected");
        let advanced = runtime.submit(&done_envelope);
        let ack2_envelope = spawned(
            "in-ack-2",
            1_700_000_004_000,
            &effect_id(advanced.step_seq),
            &["wf-node1"],
        );
        let ack2 = runtime.submit(&ack2_envelope);
        let done2_envelope = child_done("in-done-2", 1_700_000_005_000, "wf-node1", "written");
        let finished = runtime.submit(&done2_envelope);

        let produced = json!({
            "description":
                "Configure → atomic workflow root start → spawn ack → child completions → workflow \
                 terminal (spec 10.1). No LoadWorkflow, no placeholder agent run, and no host \
                 CompleteRun: the last committed step's disposition is the terminal itself.",
            "genesis_digest": genesis.record.record_digest().as_str(),
            "head_digest": finished.record.record_digest().as_str(),
            "links": [
                link(&configure_envelope, &genesis),
                link(&start_envelope, &started),
                link(&ack_envelope, &ack),
                link(&done_envelope, &advanced),
                link(&ack2_envelope, &ack2),
                link(&done2_envelope, &finished),
            ],
        });

        let expected = golden("golden_lifecycle_workflow_root.json", &produced);
        assert_eq!(produced, expected, "the workflow root lifecycle drifted");
        assert_eq!(expected["links"][1]["step"]["root_kind"], json!("workflow"));
        assert_eq!(
            expected["links"][1]["step"]["disposition"]["effects"][0]["effect"]["kind"],
            json!("spawn_tasks"),
        );
        assert_eq!(
            expected["links"][5]["step"]["disposition"]["terminal"]["kind"],
            json!("workflow"),
        );
    }

    #[test]
    fn golden_lifecycle_cancel_arc() {
        let mut runtime = Runtime::new();
        let configure_envelope = signal_config(8, None);
        let start_envelope = workflow_start("in-start", 1_700_000_001_000, two_node_spec());
        let genesis = runtime.submit(&configure_envelope);
        let started = runtime.submit(&start_envelope);

        let ack_envelope = spawned(
            "in-ack",
            1_700_000_002_000,
            &effect_id(started.step_seq),
            &["wf-node0"],
        );
        let ack = runtime.submit(&ack_envelope);

        let signal_envelope = signal_delivery(
            "in-sig",
            1_700_000_003_000,
            "delivery-a",
            1,
            logical_signal("sig-abort", SignalUrgency::Critical),
        );
        let interrupted = runtime.submit(&signal_envelope);

        let preempted_envelope = resolved(
            "in-preempted",
            1_700_000_004_000,
            &effect_id(interrupted.step_seq),
            EffectSuccess::TasksPreempted(super::super::effect::TasksPreemptedSuccess {
                attempts: vec![super::super::effect::TaskPreemptOutcome {
                    task_id: TaskId::new("wf-node0").unwrap(),
                    attempt_id: WireAttemptId::new("wf-node0:attempt:1").unwrap(),
                    outcome: super::super::effect::TaskPreemptStatus::Preempted(
                        super::super::effect::TaskPreempted {},
                    ),
                }],
            }),
        );
        let preempted = runtime.submit(&preempted_envelope);

        let cancel_envelope = cancel("in-cancel", 1_700_000_005_000);
        let cancelled = runtime.submit(&cancel_envelope);

        let produced = json!({
            "description":
                "The cancellation arc on the canonical wire (spec 7.5 / 7.7 / 11.1 · Task 12b): \
                 configure → workflow root start → spawn ack → a critical signal that preempts the \
                 running child → the host's preempt resolution → `HostControl::Cancel`, whose step \
                 is the cancelled terminal itself. Two things this chain fixes in place: the only \
                 host action a signal can cause is the preemption (its queueing/dropping/expiry are \
                 audit facts and publish nothing), and cancellation settles every downstream wait \
                 inside the same transition that commits the root terminal — §7.12 admits effects or \
                 a terminal and never both, so there is no second round trip in which the operation \
                 is neither running nor cancelled.",
            "genesis_digest": genesis.record.record_digest().as_str(),
            "head_digest": cancelled.record.record_digest().as_str(),
            "links": [
                link(&configure_envelope, &genesis),
                link(&start_envelope, &started),
                link(&ack_envelope, &ack),
                link(&signal_envelope, &interrupted),
                link(&preempted_envelope, &preempted),
                link(&cancel_envelope, &cancelled),
            ],
        });

        let expected = golden("golden_lifecycle_cancel_arc.json", &produced);
        assert_eq!(produced, expected, "the cancellation arc drifted");
        assert_eq!(
            expected["links"][3]["step"]["disposition"]["effects"][0]["effect"]["kind"],
            json!("preempt_tasks"),
            "an urgent signal's one host action is stopping the running child",
        );
        assert_eq!(
            expected["links"][5]["step"]["disposition"]["terminal"]["kind"],
            json!("cancelled"),
        );
        assert_eq!(
            expected["links"][5]["step"]["disposition"]["effects"],
            Value::Null,
            "§11.1 · the cancelling step leaves nothing waiting on the host",
        );
        assert_eq!(
            runtime.pending_effect_kinds(),
            Vec::<EffectKindTag>::new(),
            "and the transaction holds no pending effect after it",
        );
    }

    #[test]
    fn golden_lifecycle_external_payload() {
        let mut runtime = Runtime::new();
        let configure_envelope = payload_config();
        let start_envelope = agent_start("in-start", 1_700_000_001_000);
        let genesis = runtime.submit(&configure_envelope);
        let started = runtime.submit(&start_envelope);

        let acted_envelope = provider_result(
            "in-acted",
            1_700_000_002_000,
            &effect_id(started.step_seq),
            vec![tool_call("call-1", "search", json!({"q": "sources"}))],
        );
        let acted = runtime.submit(&acted_envelope);
        let results_envelope = payloads_resolved(
            "in-results",
            1_700_000_003_000,
            &effect_id(acted.step_seq),
            vec![external_payload(
                "call-1",
                body_digest(),
                BODY.len() as u64,
                "the full report body, far la…",
            )],
        );
        let results = runtime.submit(&results_envelope);
        let read_envelope = provider_result(
            "in-read",
            1_700_000_004_000,
            &effect_id(results.step_seq),
            vec![tool_call(
                "call-2",
                READ_RESULT_TOOL_NAME,
                json!({"call_id": "call-1"}),
            )],
        );
        let read = runtime.submit(&read_envelope);
        let loaded_envelope = resolved(
            "in-loaded",
            1_700_000_005_000,
            &effect_id(read.step_seq),
            EffectSuccess::PayloadLoaded(PayloadLoadedSuccess {
                handle_id: HandleId::new("call-1").unwrap(),
                payload: InlinePayload {
                    content: BODY.to_string(),
                    digest: body_digest(),
                    original_size: WireU64::new(BODY.len() as u64),
                },
            }),
        );
        let loaded = runtime.submit(&loaded_envelope);

        let produced = json!({
            "description":
                "The §7.10 external-payload arc (Task 13): configure → agent start → a provider \
                 turn calling one host tool → an **external** tool result → the model's \
                 `read_result` page-in → the loaded body. Read the records: the persisted body \
                 appears in exactly one place on this whole chain — the `payload_loaded` outcome \
                 the host sent when the kernel asked for it. It is in no effect the kernel \
                 published and in no accepted input the kernel did not ask for, which is what \
                 §25.10 means by \"large bodies do not enter the journal\". What the kernel holds \
                 instead is the reference: an opaque `payload_ref` it never interprets, a digest \
                 it checks the restored body against, and a bounded preview that is the only part \
                 to occupy context.",
            "genesis_digest": genesis.record.record_digest().as_str(),
            "head_digest": loaded.record.record_digest().as_str(),
            "links": [
                link(&configure_envelope, &genesis),
                link(&start_envelope, &started),
                link(&acted_envelope, &acted),
                link(&results_envelope, &results),
                link(&read_envelope, &read),
                link(&loaded_envelope, &loaded),
            ],
        });

        let expected = golden("golden_lifecycle_external_payload.json", &produced);
        assert_eq!(produced, expected, "the external-payload arc drifted");
        assert_eq!(
            expected["links"][3]["step"]["disposition"]["effects"][0]["effect"]["kind"],
            json!("call_provider"),
            "an external result resumes the turn like any other — the body never comes back out",
        );
        assert_eq!(
            expected["links"][4]["step"]["disposition"]["effects"][0]["effect"]["kind"],
            json!("load_payload"),
            "§7.10 rule 4 · `read_result` reduces to exactly one effect",
        );
        assert_eq!(
            expected["links"][4]["step"]["disposition"]["effects"][0]["effect"]["payload_ref"],
            json!("payload:01J8Y2QK7C4N0V"),
            "the effect hands back the host's own opaque locator, unread and unjoined",
        );

        // The body appears exactly once on the whole chain, and only where the model asked for it.
        //
        // SPEC-ISSUE: §25.10 states flatly that large bodies do not enter the journal, but §7.9's
        // `PayloadLoaded` carries `InlinePayload.content` as an accepted input, and §8.1 makes the
        // accepted input part of the durable record. So a *paged-in* body is journalled by the
        // contract's own construction. The invariant that is actually achievable — and the one this
        // golden pins — is narrower: no body enters on the **ingestion** path, and no body is ever
        // carried by an effect the kernel publishes. Either §25.10 should be scoped to those two,
        // or `PayloadLoaded` needs a shape that resolves an effect without riding the record.
        for (index, link) in expected["links"].as_array().unwrap().iter().enumerate() {
            assert_eq!(
                link["envelope"]
                    .to_string()
                    .contains("clears the inline threshold"),
                index == 5,
                "link {index} disagrees about where a large body may enter",
            );
            // A `call_provider` effect legitimately carries whatever is in context once the model
            // has paged a body in. No effect may hand the body back out to be persisted.
            for effect in link["step"]["disposition"]["effects"]
                .as_array()
                .unwrap_or(&Vec::new())
            {
                assert!(
                    !effect.to_string().contains("clears the inline threshold")
                        || effect["effect"]["kind"] == json!("call_provider"),
                    "link {index} hands the body back out in a {} effect",
                    effect["effect"]["kind"],
                );
            }
        }
        for record in &runtime.journal {
            let input = serde_json::to_string(&record.normalized_input().unwrap()).unwrap();
            assert_eq!(
                input.contains("clears the inline threshold"),
                record.step_seq().get() == 5,
                "only the page-in the model asked for may make a body durable, got step {}",
                record.step_seq()
            );
        }
    }

    // -----------------------------------------------------------------------------------------
    // §12 · logical checkpoint goldens
    // -----------------------------------------------------------------------------------------

    /// Drive one whole agent turn and stop mid-flight, so the checkpoint has something to hold:
    /// a live pending effect, a replay ledger, a task table and a live policy.
    fn agent_mid_turn() -> Runtime {
        let mut runtime = Runtime::new();
        runtime.submit(&syscall_config());
        let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));
        let acted = runtime.submit(&provider_result(
            "in-acted",
            1_700_000_002_000,
            &effect_id(started.step_seq),
            vec![tool_call("call-1", "search", json!({"q": "sources"}))],
        ));
        runtime.submit(&tools_resolved(
            "in-results",
            1_700_000_003_000,
            &effect_id(acted.step_seq),
            &[("call-1", "three sources found", false)],
        ));
        runtime
    }

    #[test]
    fn checkpoint_restore_preserves_partial_durable_wait_set_progress() {
        use crate::scheduler::tcb::{WaitCondition, WaitMode, WaitSet};
        use crate::scheduler::wait_index::WaitKey;

        let mut runtime = agent_mid_turn();
        let first = EffectId::new("wait-effect-1").unwrap();
        let second = EffectId::new("wait-effect-2").unwrap();
        let table = runtime.driver.engine_mut().unwrap().task_table_mut();
        table.register_wait_set(
            ROOT_TASK_ID,
            WaitSet {
                mode: WaitMode::All,
                conditions: vec![
                    WaitCondition::Effect(first.clone()),
                    WaitCondition::Effect(second.clone()),
                ],
            },
        );
        assert!(table.notify(&WaitKey::Effect(first)).is_empty());

        let checkpoint = runtime.checkpoint().decode().expect("checkpoint verifies");
        let mut restored = Runtime::restore_with(Some(&checkpoint), &[]);
        let restored_table = restored.driver.engine_mut().unwrap().task_table_mut();
        assert_eq!(
            restored_table
                .get(ROOT_TASK_ID)
                .unwrap()
                .wait_set
                .as_ref()
                .unwrap()
                .satisfied
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            vec![0]
        );
        assert_eq!(
            restored_table.notify(&WaitKey::Effect(second)),
            vec![crate::scheduler::tcb::TaskId::from(ROOT_TASK_ID)]
        );
        assert_eq!(
            restored_table.get(ROOT_TASK_ID).unwrap().state,
            TaskLifecycle::Ready
        );
    }

    #[test]
    fn golden_checkpoint_agent_turn() {
        let runtime = agent_mid_turn();
        let candidate = runtime.checkpoint();
        let checkpoint = candidate.decode().expect("the candidate blob verifies");

        let produced = json!({
            "description":
                "A full-state logical checkpoint taken mid-turn (spec 12.1, 12.3). base == through \
                 == the durable head, so the bounded tail is empty and a restore replays nothing \
                 before the post-checkpoint records. The logical state is partitioned four ways and \
                 the header repeats none of it: the pending `call_provider` effect, the replay \
                 ledger and the terminal slot live in `transition`, the task table in `scheduler`, \
                 the handle table in `context_vm`, the live policy and the provider-tool causation \
                 in `syscall`.",
            "through_step_seq": candidate.through_step_seq.to_string(),
            "covered_head": candidate.covered_head.as_str(),
            "state_digest": candidate.state_digest.as_str(),
            "ack_token": candidate.ack_token.as_str(),
            "checkpoint": serde_json::to_value(&checkpoint).unwrap(),
        });

        let expected = golden("golden_checkpoint_agent_turn.json", &produced);
        assert_eq!(produced, expected, "the logical checkpoint drifted");
        assert_eq!(
            expected["checkpoint"]["checkpoint_version"],
            json!(super::super::KERNEL_CHECKPOINT_VERSION)
        );
        assert_eq!(
            expected["checkpoint"]["abi_version"],
            json!(super::super::KERNEL_ABI_VERSION),
        );
        assert_eq!(
            expected["checkpoint"]["base_step_seq"], expected["checkpoint"]["through_step_seq"],
            "a full-state candidate carries no tail",
        );
        assert_eq!(expected["checkpoint"]["tail_inputs"], json!([]));
        assert_eq!(
            expected["checkpoint"]["logical_state"]["transition"]["pending_effects"][0]["effect"]["kind"],
            json!("call_provider"),
            "the effect the operation is waiting on is inside the checkpoint, not beside it",
        );
    }

    /// The incremental form of §12.1: an older logical state plus the canonical inputs that carry
    /// it forward to the covered head. This is exactly the shape a Task 16 rebase produces, and
    /// pinning it now is what stops the tail contract from being invented twice.
    #[test]
    fn golden_checkpoint_bounded_tail() {
        let mut runtime = Runtime::new();
        runtime.submit(&syscall_config());
        let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));

        // the state at the root start — the base a later rebase keeps
        let base = runtime
            .checkpoint()
            .decode()
            .expect("the base candidate verifies");

        let acted = runtime.submit(&provider_result(
            "in-acted",
            1_700_000_002_000,
            &effect_id(started.step_seq),
            vec![tool_call("call-1", "search", json!({"q": "sources"}))],
        ));
        let results = runtime.submit(&tools_resolved(
            "in-results",
            1_700_000_003_000,
            &effect_id(acted.step_seq),
            &[("call-1", "three sources found", false)],
        ));
        runtime.submit(&provider_answer(
            "in-answer",
            1_700_000_004_000,
            &effect_id(results.step_seq),
            "the brief cites three sources",
        ));

        let tail: Vec<CanonicalInput> = runtime.journal[2..]
            .iter()
            .map(|record| CanonicalInput::from_record(record).expect("a record projects"))
            .collect();
        // The transaction is the authority on what the tail still holds; harvesting from the
        // journal and asking the transaction must give the same answer, or a rebase built from
        // either source would produce a different checkpoint.
        assert_eq!(
            runtime
                .tx
                .tail_inputs()
                .into_iter()
                .filter(|entry| entry.step_seq.get() > base.through_step_seq().get())
                .collect::<Vec<_>>(),
            tail,
            "the transaction's own tail and the journal agree on (base, through]",
        );
        let rebased = runtime
            .tx
            .checkpoint_rebase(
                &CheckpointBoundary {
                    through_step_seq: base.through_step_seq(),
                    covered_head: base.covered_transaction_head_digest().clone(),
                },
                base.logical_state().clone(),
            )
            .expect("an older state plus its exact tail is a checkpoint")
            .decode()
            .expect("the rebase blob verifies");

        let produced = json!({
            "description":
                "The incremental form of a logical checkpoint (spec 12.1, 12.2): `logical_state` is \
                 the state after `base_step_seq`, and `tail_inputs` covers (base, through] exactly \
                 — no hole, no duplicate, nothing outside the range. A restore replays this tail on \
                 top of the state, then continues with the journal records after `through_step_seq`. \
                 The state digest is byte-identical to the base checkpoint's, because the state is \
                 the same state; only the tail and the header moved.",
            "base_state_digest": base.state_digest().as_str(),
            "checkpoint": serde_json::to_value(&rebased).unwrap(),
        });

        let expected = golden("golden_checkpoint_bounded_tail.json", &produced);
        assert_eq!(produced, expected, "the bounded-tail checkpoint drifted");
        assert_eq!(
            expected["checkpoint"]["state_digest"], expected["base_state_digest"],
            "a rebase carries the base state forward untouched",
        );
        assert_eq!(
            expected["checkpoint"]["base_step_seq"],
            json!("1"),
            "the base is the root start",
        );
        assert_eq!(expected["checkpoint"]["through_step_seq"], json!("4"));
        let tail = expected["checkpoint"]["tail_inputs"].as_array().unwrap();
        assert_eq!(
            tail.iter()
                .map(|entry| entry["step_seq"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["2", "3", "4"],
            "(1, 4] is exactly steps 2, 3 and 4",
        );
    }

    #[test]
    fn bless_checkpoint_v1_migration_fixture() {
        if std::env::var("BLESS_KERNEL_RECORD_FIXTURES").as_deref() != Ok("1") {
            return;
        }

        let mut runtime = Runtime::new();
        runtime.submit(&syscall_config());
        let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));
        let base = runtime
            .checkpoint()
            .decode()
            .expect("the v1 fixture base checkpoint verifies");
        let acted = runtime.submit(&provider_result(
            "in-acted",
            1_700_000_002_000,
            &effect_id(started.step_seq),
            vec![tool_call("call-1", "search", json!({"q": "sources"}))],
        ));
        runtime.submit(&tools_resolved(
            "in-results",
            1_700_000_003_000,
            &effect_id(acted.step_seq),
            &[("call-1", "three sources found", false)],
        ));
        let current = runtime
            .tx
            .checkpoint_rebase(
                &CheckpointBoundary {
                    through_step_seq: base.through_step_seq(),
                    covered_head: base.covered_transaction_head_digest().clone(),
                },
                base.logical_state().clone(),
            )
            .expect("the historical bounded-tail shape assembles")
            .decode()
            .expect("the current checkpoint verifies");
        let mut v1: serde_json::Map<String, Value> =
            serde_json::from_slice(current.checkpoint_bytes().as_slice()).unwrap();
        v1.insert("checkpoint_version".into(), json!(1));
        v1["logical_state"]["context_vm"]["messages"][0]["body"] = json!({
            "form": "structured",
            "content_json": "\"Proceed with the task described in [TASK STATE].\"",
        });
        let state_bytes = canonical_bytes(&v1["logical_state"]).unwrap();
        v1.insert(
            "state_digest".into(),
            Value::String(canonical_digest(state_bytes.as_slice()).to_string()),
        );
        let tail_bytes = canonical_bytes(&v1["tail_inputs"]).unwrap();
        v1.insert(
            "tail_digest".into(),
            Value::String(canonical_digest(tail_bytes.as_slice()).to_string()),
        );
        let mut body = v1.clone();
        body.remove("checkpoint_digest");
        v1.insert(
            "checkpoint_digest".into(),
            Value::String(canonical_digest(canonical_bytes(&body).unwrap().as_slice()).to_string()),
        );

        let fixture = json!({
            "description": "Frozen v1 bounded-tail checkpoint used to prove explicit v1-to-v2 migration and replay equivalence.",
            "checkpoint": Value::Object(v1),
            "expected": {
                "covered_head": current.covered_transaction_head_digest().as_str(),
                "tail_inputs_replayed": current.tail_inputs().len(),
            },
        });
        let path = fixture_dir().join("golden_checkpoint_v1_migration.json");
        let mut text = serde_json::to_string_pretty(&fixture).unwrap();
        text.push('\n');
        fs::write(path, text).expect("the historical v1 fixture writes");
    }

    /// §12.3 rule 1 · a candidate is a read. Appends continue, and the candidate that was handed
    /// out still describes the prefix it was taken over.
    #[test]
    fn a_candidate_neither_blocks_nor_is_invalidated_by_later_appends() {
        let mut runtime = agent_mid_turn();
        let candidate = runtime.checkpoint();
        let before = runtime.tx.head().expect("a head");
        assert_eq!(candidate.through_step_seq, before.step_seq);

        let results_effect = effect_id(before.step_seq);
        runtime.submit(&provider_answer(
            "in-answer",
            1_700_000_004_000,
            &results_effect,
            "the brief cites three sources",
        ));

        let after = runtime.tx.head().expect("a head");
        assert_ne!(after.step_seq, before.step_seq, "the journal moved on");
        assert_eq!(
            candidate.through_step_seq, before.step_seq,
            "the candidate still covers the prefix it was taken over",
        );
        candidate
            .decode()
            .expect("and it still verifies after the journal moved")
            .verify_belongs_to(&operation(), runtime.journal[0].record_digest())
            .expect("it is still this operation's checkpoint");
    }

    // -----------------------------------------------------------------------------------------
    // §12.2 · bounded-tail restore
    // -----------------------------------------------------------------------------------------

    /// Drive an operation to a fixed point through a fixed envelope sequence.
    ///
    /// One function so the two sides of a differential are *the same* sequence by construction
    /// rather than by two copies that agree today. `stop_after` is how many envelopes to submit.
    fn drive(runtime: &mut Runtime, envelopes: &[WireEnvelope]) {
        for envelope in envelopes {
            runtime.submit(envelope);
        }
    }

    /// The canonical agent turn, as a list of envelopes.
    ///
    /// Deterministic in every field a record digests: ids, clocks and payloads are literals, and
    /// the effect ids are derived from the step sequence exactly as the kernel derives them. That is
    /// what makes "byte-identical" a checkable claim rather than a hope.
    fn turn_envelopes() -> Vec<WireEnvelope> {
        vec![
            syscall_config(),
            agent_start("in-start", 1_700_000_001_000),
            provider_result(
                "in-acted",
                1_700_000_002_000,
                &effect_id(WireU64::new(1)),
                vec![tool_call("call-1", "search", json!({"q": "sources"}))],
            ),
            tools_resolved(
                "in-results",
                1_700_000_003_000,
                &effect_id(WireU64::new(2)),
                &[("call-1", "three sources found", false)],
            ),
            provider_result(
                "in-acted-2",
                1_700_000_004_000,
                &effect_id(WireU64::new(3)),
                vec![tool_call("call-2", "search", json!({"q": "more"}))],
            ),
            tools_resolved(
                "in-results-2",
                1_700_000_005_000,
                &effect_id(WireU64::new(4)),
                &[("call-2", "two more sources", false)],
            ),
            provider_answer(
                "in-answer",
                1_700_000_006_000,
                &effect_id(WireU64::new(5)),
                "the brief cites five sources",
            ),
        ]
    }

    fn digests(records: &[KernelRecord]) -> Vec<String> {
        records
            .iter()
            .map(|record| record.record_digest().to_string())
            .collect()
    }

    /// The whole observable surface of a runtime, as bytes.
    ///
    /// Everything a differential must compare and nothing that is allowed to differ: the state
    /// digest a checkpoint would take, the pending effect identities, the terminal, and the head.
    /// If a restored runtime matches an uninterrupted one on all four *and* keeps producing the same
    /// records, "behaves identically" is not a judgement call.
    fn surface(runtime: &Runtime) -> Value {
        json!({
            "head": runtime.tx.head().map(|head| json!({
                "digest": head.digest.as_str(),
                "step_seq": head.step_seq.to_string(),
            })),
            "lifecycle": format!("{:?}", runtime.tx.lifecycle()),
            "pending_effects": runtime
                .tx
                .pending_effects()
                .map(|effect| serde_json::to_value(effect).unwrap())
                .collect::<Vec<_>>(),
            "terminal": runtime.tx.terminal().map(|t| serde_json::to_value(t).unwrap()),
            "logical_state": serde_json::to_value(
                runtime
                    .tx
                    .transition_state_for_restore(
                        runtime.driver.root_kind(),
                        runtime.driver.focus().cloned(),
                    )
                    .expect("a configured runtime has a transition state"),
            )
            .unwrap(),
            "context_vm": serde_json::to_value(
                runtime.driver.project_logical_state().context_vm
            ).unwrap(),
            "scheduler": serde_json::to_value(
                runtime.driver.project_logical_state().scheduler
            ).unwrap(),
        })
    }

    /// Task 16b · a checkpoint owns the active DAG and every child process identity needed to
    /// continue it. Restoring while the first node is live must neither forget the second node nor
    /// rebuild the child under different role/isolation/inheritance.
    #[test]
    fn an_active_workflow_and_its_child_restore_to_the_same_completion() {
        let (mut uninterrupted, _) = workflow_with_live_child();
        let checkpoint = uninterrupted.checkpoint().decode().expect("verifies");
        let mut restored = Runtime::restore_with(Some(&checkpoint), &[]);

        let first_done = child_done(
            "in-done-1",
            1_700_000_003_000,
            "wf-node0",
            "sources collected",
        );
        let uninterrupted_next = uninterrupted.submit(&first_done);
        let restored_next = restored.submit(&first_done);
        assert_eq!(
            restored_next, uninterrupted_next,
            "the restored DAG schedules the same second node",
        );

        let uninterrupted_spawn = effect_id(uninterrupted_next.step_seq);
        let restored_spawn = effect_id(restored_next.step_seq);
        uninterrupted.submit(&spawned(
            "in-ack-2",
            1_700_000_004_000,
            &uninterrupted_spawn,
            &["wf-node1"],
        ));
        restored.submit(&spawned(
            "in-ack-2",
            1_700_000_004_000,
            &restored_spawn,
            &["wf-node1"],
        ));
        let second_done = child_done("in-done-2", 1_700_000_005_000, "wf-node1", "brief written");
        uninterrupted.submit(&second_done);
        restored.submit(&second_done);

        assert_eq!(
            surface(&restored),
            surface(&uninterrupted),
            "workflow state and child permission identity are reversible",
        );
    }

    /// Task 16b · queued signal source state and the router's business-dedupe memory survive a
    /// checkpoint. The queued payload must drive the same follow-up provider request, while a new
    /// delivery with the same key remains ignored on both sides.
    #[test]
    fn a_queued_signal_and_its_dedupe_key_restore_to_the_same_follow_up() {
        let mut uninterrupted = Runtime::new();
        uninterrupted.submit(&signal_config(8, None));
        let started = uninterrupted.submit(&agent_start("in-start", 1_700_000_001_000));
        uninterrupted.submit(&signal_delivery(
            "in-sig-1",
            1_700_000_002_000,
            "delivery-a",
            1,
            LogicalSignal {
                payload: super::super::scalar::BoundedJson::new(json!({
                    "job": "nightly-index"
                }))
                .unwrap(),
                dedupe_key: Some("nightly-index".to_string()),
                ..logical_signal("sig-nightly", SignalUrgency::Normal)
            },
        ));

        let checkpoint = uninterrupted.checkpoint().decode().expect("verifies");
        let mut restored = Runtime::restore_with(Some(&checkpoint), &[]);

        let duplicate = signal_delivery(
            "in-sig-2",
            1_700_000_003_000,
            "delivery-b",
            2,
            LogicalSignal {
                payload: super::super::scalar::BoundedJson::new(json!({
                    "job": "nightly-index"
                }))
                .unwrap(),
                dedupe_key: Some("nightly-index".to_string()),
                ..logical_signal("sig-nightly", SignalUrgency::Normal)
            },
        );
        assert_eq!(
            restored.submit(&duplicate),
            uninterrupted.submit(&duplicate),
            "the restored router remembers the business dedupe key",
        );

        let provider = effect_id(started.step_seq);
        let answer = provider_answer(
            "in-answer",
            1_700_000_004_000,
            &provider,
            "the first request completed",
        );
        assert_eq!(
            restored.submit(&answer),
            uninterrupted.submit(&answer),
            "the queued payload produces the same follow-up provider request",
        );
        assert_eq!(surface(&restored), surface(&uninterrupted));
    }

    #[test]
    fn caller_capability_ceiling_survives_checkpoint_restore() {
        use crate::types::capability::{
            ActionSet, Capability, CapabilityId, CapabilityKind, ConstraintSet, Lease, Principal,
            ResourceSelector,
        };

        let capability = Capability {
            id: CapabilityId("root-read".into()),
            kind: CapabilityKind::Tool,
            resource: ResourceSelector("/repo/src/**".into()),
            actions: ActionSet(["read".into()].into_iter().collect()),
            constraints: ConstraintSet::default(),
            lease: Some(Lease {
                expires_at_turn: Some(10),
            }),
            delegatable: true,
            issuer: Principal("root".into()),
        };
        let mut runtime = Runtime::new();
        runtime.submit(&configure());
        runtime.submit(&agent_start_with_capabilities(
            "in-start",
            1_700_000_001_000,
            vec![capability.clone()],
        ));

        let checkpoint = runtime.checkpoint().decode().expect("verifies");
        let restored = Runtime::restore_with(Some(&checkpoint), &[]);

        assert_eq!(
            restored
                .driver
                .engine
                .as_ref()
                .unwrap()
                .task_capabilities("root"),
            &[capability],
            "authority state must not disappear when the reverse runtime is rebuilt"
        );
    }

    #[test]
    fn hierarchical_budget_grant_and_settlement_marker_survive_checkpoint_restore() {
        use crate::scheduler::budget_grant::{ResourceBudget, debit, reserve};
        use crate::scheduler::tcb::TaskLifecycle;
        use crate::types::agent::{
            AgentIsolation, AgentRole, ContextInheritance, IsolationManifest,
        };

        let tokens = |value| ResourceBudget {
            tokens: Some(value),
            ..ResourceBudget::default()
        };
        let mut runtime = Runtime::new();
        runtime.submit(&configure());
        runtime.submit(&agent_start("in-start", 1_700_000_001_000));
        let table = runtime.driver.engine.as_mut().unwrap().task_table_mut();
        table.get_mut("root").unwrap().child_budget_remaining = Some(tokens(100));
        let manifest = IsolationManifest {
            agent_id: "child".into(),
            role: AgentRole::Implement,
            isolation: AgentIsolation::Shared,
            context_inheritance: ContextInheritance::Full,
            permitted_capability_ids: Vec::new(),
            requested_capabilities: Vec::new(),
            requested_budget: Some(tokens(60)),
        };
        table
            .spawn_child(
                "root",
                &manifest,
                SchedulerBudget::default(),
                TaskLifecycle::Running,
            )
            .unwrap();
        let grant = reserve("root".into(), "child".into(), &tokens(100), &tokens(60)).unwrap();
        table.get_mut("root").unwrap().child_budget_remaining =
            Some(debit(&tokens(100), &tokens(60)));
        table.attach_child_budget_grant("child", grant);

        let checkpoint = runtime.checkpoint().decode().expect("verifies");
        let mut restored = Runtime::restore_with(Some(&checkpoint), &[]);
        let restored_table = restored.driver.engine.as_mut().unwrap().task_table_mut();
        assert_eq!(
            restored_table
                .get("child")
                .unwrap()
                .budget_grant
                .as_ref()
                .unwrap()
                .reserved,
            tokens(60)
        );
        assert_eq!(
            restored_table.get("child").unwrap().child_budget_remaining,
            Some(tokens(60))
        );

        restored_table.return_child_budget("child");
        restored_table.return_child_budget("child");
        assert_eq!(
            restored_table.get("root").unwrap().child_budget_remaining,
            Some(tokens(100)),
            "restored grant settles exactly once"
        );
        assert!(
            restored_table
                .get("child")
                .unwrap()
                .budget_grant
                .as_ref()
                .unwrap()
                .settled
        );
    }

    #[test]
    fn spc_019_09_cross_task_object_read_requires_capability_and_registry_restores() {
        use crate::mm::handle::{Handle, HandleKind, ObjectDescriptor};
        use crate::scheduler::tcb::TaskLifecycle;
        use crate::types::agent::{
            AgentIsolation, AgentRole, ContextInheritance, IsolationManifest,
        };
        use crate::types::capability::{
            ActionSet, Capability, CapabilityId, CapabilityKind, ConstraintSet, Principal,
            ResourceSelector,
        };

        fn start_with_object(capabilities: Vec<Capability>) -> (Runtime, EffectId) {
            let mut runtime = Runtime::new();
            runtime.submit(&syscall_config());
            let started = runtime.submit(&agent_start_with_capabilities(
                "in-start",
                1_700_000_001_000,
                capabilities,
            ));
            let provider = sole_effect(&started).effect_id.clone();
            let handle = Handle::resident_for(77, HandleKind::ToolResult, 1, "shared-77");
            let descriptor = ObjectDescriptor::from_handle("owner-a".into(), &handle, 1);
            let engine = runtime.driver.engine.as_mut().unwrap();
            engine.ctx.handles.insert(handle);
            engine
                .task_table_mut()
                .spawn_child(
                    "root",
                    &IsolationManifest {
                        agent_id: "owner-a".into(),
                        role: AgentRole::Implement,
                        isolation: AgentIsolation::Shared,
                        context_inheritance: ContextInheritance::Full,
                        permitted_capability_ids: Vec::new(),
                        requested_capabilities: Vec::new(),
                        requested_budget: None,
                    },
                    SchedulerBudget::default(),
                    TaskLifecycle::Running,
                )
                .unwrap();
            engine
                .task_table_mut()
                .register_object("owner-a", descriptor)
                .unwrap();
            (runtime, provider)
        }

        let (mut denied, denied_provider) = start_with_object(Vec::new());
        denied.submit(&provider_result(
            "in-read-denied",
            1_700_000_002_000,
            &denied_provider,
            vec![tool_call(
                "call-read",
                "read_object",
                json!({"object_id": 77}),
            )],
        ));
        assert_eq!(rejections(&denied)[0].0, "read_object");

        let capability = Capability {
            id: CapabilityId("read-owner-a-77".into()),
            kind: CapabilityKind::Tool,
            resource: ResourceSelector("object:owner-a/77".into()),
            actions: ActionSet(["read".into()].into_iter().collect()),
            constraints: ConstraintSet::default(),
            lease: None,
            delegatable: false,
            issuer: Principal("owner-a".into()),
        };
        let (mut allowed, allowed_provider) = start_with_object(vec![capability]);
        allowed.submit(&provider_result(
            "in-read-allowed",
            1_700_000_002_000,
            &allowed_provider,
            vec![tool_call(
                "call-read",
                "read_object",
                json!({"object_id": 77}),
            )],
        ));
        assert!(rejections(&allowed).is_empty());

        let checkpoint = allowed.checkpoint().decode().expect("verifies");
        let restored = Runtime::restore_with(Some(&checkpoint), &[]);
        assert_eq!(surface(&restored), surface(&allowed));
        assert_eq!(
            restored
                .driver
                .engine
                .as_ref()
                .unwrap()
                .task_table()
                .object(77)
                .unwrap()
                .owner
                .as_str(),
            "owner-a"
        );
    }

    /// Task 16b · a completed child remains the same process fact after restore: its role,
    /// isolation, context inheritance, capability ceiling and join result are not inferred anew.
    #[test]
    fn a_subagent_process_and_join_result_restore_without_permission_drift() {
        let mut spec = two_node_spec();
        spec.nodes[0].run_spec = Some(LogicalAgentSpec {
            role: Some(WireRole::Verify),
            isolation: Some(WireIsolation::ReadOnly),
            ..LogicalAgentSpec::new("verify the collected sources")
        });

        let mut uninterrupted = Runtime::new();
        uninterrupted.submit(&syscall_config());
        let started = uninterrupted.submit(&workflow_start("in-start", 1_700_000_001_000, spec));
        uninterrupted.submit(&spawned(
            "in-ack-1",
            1_700_000_002_000,
            &effect_id(started.step_seq),
            &["wf-node0"],
        ));
        let completed = uninterrupted.submit(&child_done(
            "in-done-1",
            1_700_000_003_000,
            "wf-node0",
            "sources verified",
        ));

        let checkpoint = uninterrupted.checkpoint().decode().expect("verifies");
        let mut restored = Runtime::restore_with(Some(&checkpoint), &[]);
        let original_task = uninterrupted
            .driver
            .project_logical_state()
            .scheduler
            .tasks
            .into_iter()
            .find(|task| task.task_id.as_str() == "wf-node0")
            .expect("the completed child remains projected");
        let restored_task = restored
            .driver
            .project_logical_state()
            .scheduler
            .tasks
            .into_iter()
            .find(|task| task.task_id.as_str() == "wf-node0")
            .expect("the restored child remains projected");
        assert_eq!(restored_task, original_task);
        let process = restored_task.process.expect("it is still a child process");
        assert_eq!(process.role, "verify");
        assert_eq!(process.isolation, "read_only");
        assert_eq!(process.context_inheritance, "none");
        assert!(
            process.join_result.is_some(),
            "the join result is source state"
        );

        let next_spawn = effect_id(completed.step_seq);
        let ack = spawned("in-ack-2", 1_700_000_004_000, &next_spawn, &["wf-node1"]);
        assert_eq!(
            restored.submit(&ack),
            uninterrupted.submit(&ack),
            "the restored process table authorizes the same next transition",
        );
        assert_eq!(surface(&restored), surface(&uninterrupted));
    }

    #[test]
    fn spc_009_06_a_restored_root_tasks_child_budget_remaining_matches_the_checkpointed_value() {
        // Plan §8 / spc_009-06: closes the checkpoint-wire half of spc_009-05 — root's
        // `child_budget_remaining` was seeded from a real RunGroup admission grant (spc_009-05),
        // but `TaskControlState` did not carry it on the wire, so a checkpoint→restore silently
        // reset root's pool to `None`, un-seeding the hierarchical budget check right after a
        // restore even though nothing about the RunGroup grant changed.
        //
        // Narrower than originally scoped: `LoopStateMachine.budget_grant` itself (the
        // whole-operation admission grant `engine.budget_grant()` reports) needs no new wire
        // field at all — `Runtime::restore_with` already rebuilds it from `genesis_config` via
        // `build_engine`, and `budget_grant` is boot-only (absent from `LivePolicyPatch` in
        // `command.rs`), so the genesis record it is already part of is authoritative for the
        // whole operation's lifetime. Verified empirically: the checkpoint round-trip below
        // reproduces the same `engine.budget_grant()` on both sides with no `SchedulerStateV1`
        // field for it at all. The one genuine gap is `child_budget_remaining`, which is derived,
        // per-task, debit-mutated state with no other durable home — that is what this test (and
        // `TaskControlState.child_budget_remaining`) actually closes.
        use crate::runtime::kernel::wire::config::BudgetGrant;
        use crate::scheduler::budget_grant::ResourceBudget;

        let mut uninterrupted = Runtime::new();
        uninterrupted.submit(&syscall_config_with(|config| {
            config.budget_grant = Some(BudgetGrant {
                reservation_id: "res-1".to_string(),
                tokens: Some(WireU64::new(1_000)),
                subagents: None,
                rounds: None,
            });
        }));
        uninterrupted.submit(&agent_start("in-start", 1_700_000_001_000));

        let expected_pool = uninterrupted
            .driver
            .engine()
            .expect("engine installed")
            .task_table()
            .get("root")
            .expect("root exists")
            .child_budget_remaining;
        assert_eq!(
            expected_pool,
            Some(ResourceBudget {
                tokens: Some(1_000),
                ..ResourceBudget::default()
            }),
            "sanity: root's pool really was seeded from the grant before any checkpoint"
        );

        let checkpoint = uninterrupted.checkpoint().decode().expect("verifies");
        let restored = Runtime::restore_with(Some(&checkpoint), &[]);

        let actual_pool = restored
            .driver
            .engine()
            .expect("engine installed")
            .task_table()
            .get("root")
            .expect("root exists")
            .child_budget_remaining;
        assert_eq!(
            actual_pool, expected_pool,
            "a restored root task's own grantable pool must match the checkpointed one exactly, \
             not be re-derived fresh from the admission grant (which would silently undo debits)"
        );
    }

    #[test]
    fn spc_002_09_a_restored_approval_wait_is_indexed_the_same_as_before_checkpoint() {
        // Plan §3.1 "Replay invariants": same checkpoint+journal must reproduce the same WaitSet
        // state. `Tcb.wait` alone restoring correctly is not the whole story — `WaitIndex` is a
        // separate structure (spc_003) that a restore must also reproduce, or a task that was
        // waiting when checkpointed becomes unwakeable (via `wake`/`notify`) after restore even
        // though its own `Tcb.wait` field looks fine.
        use crate::scheduler::tcb::ApprovalId;
        use crate::scheduler::wait_index::WaitKey;

        let (mut uninterrupted, provider) = agent_awaiting_approval();
        let requested = uninterrupted.submit(&provider_result(
            "in-gated",
            1_700_000_002_000,
            &provider,
            vec![tool_call("call-1", "search", json!({"q": "a"}))],
        ));
        assert_eq!(kinds(&requested), vec![EffectKindTag::RequestApproval]);

        let checkpoint = uninterrupted.checkpoint().decode().expect("verifies");
        let restored = Runtime::restore_with(Some(&checkpoint), &[]);

        let key = WaitKey::Approval(ApprovalId("pending".into()));
        let expected = uninterrupted
            .driver
            .engine()
            .expect("engine installed")
            .task_table()
            .wait_index()
            .lookup(&key)
            .to_vec();
        assert!(
            !expected.is_empty(),
            "sanity: the uninterrupted run really did index a waiting task"
        );
        let actual = restored
            .driver
            .engine()
            .expect("engine installed")
            .task_table()
            .wait_index()
            .lookup(&key)
            .to_vec();
        assert_eq!(
            actual, expected,
            "a restored Approval wait must be indexed identically to the uninterrupted run"
        );
    }

    #[test]
    fn a_pending_subagent_preemption_restores_from_the_transition_effect() {
        let (mut uninterrupted, _) = workflow_with_live_child();
        let requested = uninterrupted.submit(&signal_delivery(
            "in-sig-critical",
            1_700_000_003_000,
            "delivery-critical",
            1,
            logical_signal("sig-critical", SignalUrgency::Critical),
        ));
        let checkpoint = uninterrupted.checkpoint().decode().expect("verifies");
        let mut restored = Runtime::restore_with(Some(&checkpoint), &[]);

        let resolved = resolved(
            "in-preempted",
            1_700_000_004_000,
            &effect_id(requested.step_seq),
            EffectSuccess::TasksPreempted(super::super::effect::TasksPreemptedSuccess {
                attempts: vec![super::super::effect::TaskPreemptOutcome {
                    task_id: TaskId::new("wf-node0").unwrap(),
                    attempt_id: WireAttemptId::new("wf-node0:attempt:1").unwrap(),
                    outcome: super::super::effect::TaskPreemptStatus::Preempted(
                        super::super::effect::TaskPreempted {},
                    ),
                }],
            }),
        );
        assert_eq!(
            restored.submit(&resolved),
            uninterrupted.submit(&resolved),
            "the transition-owned preempt intent remains resolvable after restore",
        );
        assert_eq!(surface(&restored), surface(&uninterrupted));
    }

    /// Task 16b · a `PagedOut` handle restored from a checkpoint projects the archived tool body
    /// exactly as the uninterrupted renderer does on the next provider call.
    #[test]
    fn a_paged_out_result_restores_to_the_same_rendered_provider_context() {
        let mut uninterrupted = Runtime::new();
        uninterrupted.submit(&syscall_config());
        let started = uninterrupted.submit(&agent_start("in-start", 1_700_000_001_000));
        let archived_body = "ARCHIVED TOOL OUTPUT ".repeat(300);
        {
            let engine = uninterrupted
                .driver
                .engine
                .as_mut()
                .expect("the configured agent has an engine");
            let mut assistant = Message::assistant("I checked the archive.");
            assistant.tool_calls = vec![crate::types::message::ToolCall {
                id: "call-archived".into(),
                name: "search".into(),
                arguments: json!({"q": "archived evidence"}),
            }];
            engine.ctx.push_history(assistant, 8);
            engine.ctx.push_history(
                Message::tool(vec![ContentPart::ToolResult {
                    call_id: "call-archived".into(),
                    output: archived_body,
                    is_error: false,
                    durable_content: None,
                }]),
                1_200,
            );
            let handle_id = engine
                .ctx
                .handles
                .all()
                .iter()
                .find(|handle| handle.source.as_deref() == Some("call-archived"))
                .expect("the tool result is addressable")
                .id;
            engine
                .ctx
                .handles
                .get_mut(handle_id)
                .expect("the handle remains live")
                .residency = Residency::PagedOut {
                payload_ref: "payload:checkpoint-archive".to_string(),
                digest: format!("sha256:{}", "1".repeat(64)),
            };
        }
        let checkpoint = uninterrupted.checkpoint().decode().expect("verifies");
        let mut restored = Runtime::restore_with(Some(&checkpoint), &[]);
        assert_eq!(
            serde_json::to_value(
                &restored
                    .driver
                    .engine()
                    .expect("restored engine")
                    .ctx
                    .render()
                    .turns
            )
            .unwrap(),
            serde_json::to_value(
                &uninterrupted
                    .driver
                    .engine()
                    .expect("uninterrupted engine")
                    .ctx
                    .render()
                    .turns
            )
            .unwrap(),
            "the restored PagedOut preview itself matches before either run advances",
        );

        let acted = provider_result(
            "in-acted",
            1_700_000_002_000,
            &effect_id(started.step_seq),
            vec![tool_call(
                "call-after-page-out",
                "search",
                json!({
                    "q": "fresh evidence"
                }),
            )],
        );
        let uninterrupted_tools = uninterrupted.submit(&acted);
        let restored_tools = restored.submit(&acted);
        assert_eq!(restored_tools, uninterrupted_tools);

        let results = tools_resolved(
            "in-results",
            1_700_000_003_000,
            &effect_id(uninterrupted_tools.step_seq),
            &[("call-after-page-out", "fresh evidence found", false)],
        );
        let uninterrupted_provider = uninterrupted.submit(&results);
        let restored_provider = restored.submit(&results);
        assert_eq!(
            restored_provider, uninterrupted_provider,
            "the post-restore renderer emits the same provider context with a PagedOut preview",
        );
        assert_eq!(surface(&restored), surface(&uninterrupted));
    }

    /// **Verification 1 (full-state form)** · an uninterrupted run and a restored one are
    /// byte-identical.
    ///
    /// Both sides submit the *same* envelope list. The second takes a full-state checkpoint
    /// half-way, throws the runtime away, restores from the blob plus the records above it, and
    /// finishes. Every record digest on both journals must match, and so must the whole observable
    /// surface — including the digest of the logical state, which is the strongest statement
    /// available: the two runtimes would produce the same checkpoint.
    #[test]
    fn an_uninterrupted_run_and_a_full_state_restore_are_byte_identical() {
        let envelopes = turn_envelopes();

        let mut uninterrupted = Runtime::new();
        drive(&mut uninterrupted, &envelopes);

        let mut interrupted = Runtime::new();
        drive(&mut interrupted, &envelopes[..4]);
        let candidate = interrupted.checkpoint();
        let checkpoint = candidate.decode().expect("the candidate blob verifies");
        assert_eq!(
            checkpoint.base_step_seq(),
            checkpoint.through_step_seq(),
            "this half of the differential exercises the full-state form",
        );

        // The crash: everything in memory is gone, and all the host still holds is the blob and the
        // journal. Records at or below `through_step_seq` are deliberately *not* handed back.
        let mut restored = interrupted.restore(&checkpoint);
        assert_eq!(
            restored.restore_cost.unwrap().records_before_checkpoint,
            0,
            "a restore with a checkpoint reads nothing below it",
        );
        assert_eq!(
            surface(&restored),
            surface(&interrupted),
            "the restored runtime is the runtime that crashed",
        );

        drive(&mut restored, &envelopes[4..]);
        assert_eq!(
            digests(&restored.journal),
            digests(&uninterrupted.journal[4..]),
            "every post-restore record is byte-identical to the uninterrupted one",
        );
        assert_eq!(
            surface(&restored),
            surface(&uninterrupted),
            "and so is the state they end in",
        );
    }

    /// **Verification 1 (rebase form)** · the same differential over a checkpoint whose logical
    /// state sits *below* its covered head.
    ///
    /// The restore therefore has real work to do before it touches the journal: it replays the
    /// bounded tail, verifies each replayed record against the digest the checkpoint recorded, and
    /// only then continues.
    #[test]
    fn an_uninterrupted_run_and_a_rebased_restore_are_byte_identical() {
        let envelopes = turn_envelopes();

        let mut uninterrupted = Runtime::new();
        drive(&mut uninterrupted, &envelopes);

        let mut interrupted = Runtime::new();
        drive(&mut interrupted, &envelopes[..2]);
        let base = interrupted
            .checkpoint()
            .decode()
            .expect("the base candidate verifies");
        drive(&mut interrupted, &envelopes[2..4]);

        let rebased = interrupted
            .tx
            .checkpoint_rebase(
                &CheckpointBoundary {
                    through_step_seq: base.through_step_seq(),
                    covered_head: base.covered_transaction_head_digest().clone(),
                },
                base.logical_state().clone(),
            )
            .expect("a rebase over (1, 3] is assemblable")
            .decode()
            .expect("the rebase blob verifies");
        assert!(
            rebased.base_step_seq() < rebased.through_step_seq(),
            "this half of the differential exercises the rebase form",
        );
        assert_eq!(rebased.tail_inputs().len(), 2);

        let mut restored = interrupted.restore(&rebased);
        let cost = restored.restore_cost.unwrap();
        assert_eq!(cost.records_before_checkpoint, 0);
        assert_eq!(
            cost.tail_inputs_replayed, 2,
            "the tail was actually replayed"
        );
        assert_eq!(
            surface(&restored),
            surface(&interrupted),
            "replaying the bounded tail lands on the state the run was in",
        );

        // The rebase form is what Task 16 adds to the checkpoint contract, so its five candidate
        // values and the state a restore lands on are frozen together.
        let produced = json!({
            "description":
                "Spec 12.2 / 12.3 rule 11 · the rebase form end to end. `logical_state` is the \
                 state after `base_step_seq`, `tail_inputs` covers (base, through] exactly, and \
                 `base_record_digest` is the chain anchor the tail replays from — the record an \
                 acked checkpoint is allowed to have reclaimed. Restoring the blob replays that \
                 tail, verifies each replayed record against the digest the checkpoint carries, and \
                 lands on the head the run was at.",
            "base_step_seq": rebased.base_step_seq().to_string(),
            "base_record_digest": rebased.base_record_digest().as_str(),
            "through_step_seq": rebased.through_step_seq().to_string(),
            "covered_head": rebased.covered_transaction_head_digest().as_str(),
            "state_digest": rebased.state_digest().as_str(),
            "tail_steps": rebased
                .tail_inputs()
                .iter()
                .map(|entry| entry.step_seq.to_string())
                .collect::<Vec<_>>(),
            "restored_head": restored.tx.head().unwrap().digest.as_str(),
            "restore_cost": {
                "records_before_checkpoint": cost.records_before_checkpoint,
                "tail_inputs_replayed": cost.tail_inputs_replayed,
                "records_after_checkpoint": cost.records_after_checkpoint,
            },
        });
        let expected = golden("golden_checkpoint_rebase_restore.json", &produced);
        assert_eq!(produced, expected, "the rebase restore drifted");
        assert_eq!(expected["base_step_seq"], json!("1"));
        assert_eq!(expected["through_step_seq"], json!("3"));
        assert_eq!(
            expected["restore_cost"]["records_before_checkpoint"],
            json!(0),
            "the whole point: nothing below the base is read",
        );

        drive(&mut restored, &envelopes[4..]);
        assert_eq!(
            digests(&restored.journal),
            digests(&uninterrupted.journal[4..]),
        );
        assert_eq!(surface(&restored), surface(&uninterrupted));
    }

    /// §7.10 · a body that lives with the host travels as a reference, and comes back as one.
    ///
    /// The half of §5q-2 that keeps the checkpoint from re-inlining what §7.10 spent an effect kind
    /// keeping out: the message carries the preview, the handle carries the locator and the digest,
    /// and the restore rebuilds exactly that pairing.
    #[test]
    fn an_external_body_is_checkpointed_by_reference_and_restored_as_one() {
        let (mut runtime, effect) = agent_awaiting_tool_results();
        let body_digest =
            super::super::record::canonical_digest(b"a body far over the inline threshold");
        runtime.submit(&payloads_resolved(
            "in-results",
            1_700_000_003_000,
            &effect,
            vec![external_payload(
                "call-1",
                body_digest.clone(),
                64 * 1024,
                "the first 2 KiB of it",
            )],
        ));

        let context_vm = runtime.driver.project_logical_state().context_vm;
        let referenced: Vec<&StoredMessageState> = context_vm
            .messages
            .iter()
            .filter(|message| matches!(message.body, StoredMessageBody::Reference(_)))
            .collect();
        assert_eq!(referenced.len(), 1, "exactly the external result");
        let StoredMessageBody::Reference(reference) = &referenced[0].body else {
            unreachable!()
        };
        assert_eq!(reference.digest, body_digest.as_str());
        assert_eq!(reference.tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(reference.preview, "the first 2 KiB of it");
        assert!(
            !serde_json::to_string(&context_vm)
                .unwrap()
                .contains("a body far over the inline threshold"),
            "the body itself is nowhere in the checkpoint",
        );

        let checkpoint = runtime.checkpoint().decode().expect("verifies");
        let restored = Runtime::restore_with(Some(&checkpoint), &[]);
        assert_eq!(surface(&restored), surface(&runtime));
        assert_eq!(
            restored
                .driver
                .project_logical_state()
                .context_vm
                .handles
                .iter()
                .filter_map(|handle| handle.digest.clone())
                .collect::<Vec<_>>(),
            vec![body_digest.to_string()],
            "the handle that addresses the body is restored with its verification digest",
        );
    }

    #[test]
    fn a_structured_inline_tool_result_survives_checkpoint_restore() {
        let (mut runtime, effect) = agent_awaiting_structured_tool_results();
        let durable = DurableContent {
            schema_version: 1,
            blocks: vec![
                DurableContentBlock::Text {
                    text: "captured".into(),
                },
                DurableContentBlock::Image {
                    source: DurableSource::Base64 {
                        data: "aW1hZ2U=".into(),
                    },
                    media_type: Some("image/png".into()),
                    provider_options: None,
                },
                DurableContentBlock::File {
                    source: DurableSource::FileId {
                        id: "file-7".into(),
                        affinity: crate::types::durable_content::EndpointAffinity {
                            provider_id: "openai".into(),
                            endpoint_id: "responses".into(),
                        },
                    },
                    media_type: Some("application/pdf".into()),
                    provider_options: None,
                },
            ],
        };
        runtime.submit(&payloads_resolved(
            "in-structured-results",
            1_700_000_003_000,
            &effect,
            vec![WireToolResultPayload::Inline(InlineToolResult {
                call_id: CallId::new("call-1").unwrap(),
                result: WireToolResult {
                    output: "captured".into(),
                    durable_content: Some(durable.clone()),
                    is_error: false,
                    disposition: ToolResultDisposition::Recoverable,
                    tokens: None,
                },
            })],
        ));

        let checkpoint = runtime.checkpoint().decode().expect("checkpoint verifies");
        let structured = checkpoint
            .logical_state()
            .context_vm
            .messages
            .iter()
            .find_map(|message| match &message.body {
                StoredMessageBody::Structured(body) => body.durable_tool_result.as_ref(),
                _ => None,
            })
            .expect("structured tool result stays in checkpoint");
        assert_eq!(structured.call_id, "call-1");
        assert_eq!(structured.blocks, durable.blocks);

        let restored = Runtime::restore_with(Some(&checkpoint), &[]);
        let restored_result = restored
            .driver
            .engine()
            .expect("engine")
            .ctx
            .partitions
            .history
            .messages
            .iter()
            .find_map(|message| match &message.content {
                Content::Parts(parts) => parts.iter().find_map(|part| match part {
                    ContentPart::ToolResult {
                        call_id,
                        durable_content,
                        ..
                    } if call_id.as_str() == "call-1" => durable_content.as_ref(),
                    _ => None,
                }),
                Content::Text(_) => None,
            })
            .expect("restored result keeps durable blocks");
        assert_eq!(restored_result, &durable);
    }

    #[test]
    fn an_inline_tool_result_rejects_invalid_durable_content_before_state_mutation() {
        let (mut runtime, effect) = agent_awaiting_tool_results();
        let fault = runtime.reject(&payloads_resolved(
            "in-invalid-structured-result",
            1_700_000_003_000,
            &effect,
            vec![WireToolResultPayload::Inline(InlineToolResult {
                call_id: CallId::new("call-1").unwrap(),
                result: WireToolResult {
                    output: "bad".into(),
                    durable_content: Some(DurableContent {
                        schema_version: 2,
                        blocks: Vec::new(),
                    }),
                    is_error: false,
                    disposition: ToolResultDisposition::Recoverable,
                    tokens: None,
                },
            })],
        ));
        assert_eq!(fault.code, KernelFaultCode::MalformedEnvelope);
        assert!(fault.message.contains("invalid durable content"));
        assert_eq!(
            runtime.pending_effect_kinds(),
            vec![EffectKindTag::ExecuteTools]
        );
    }

    #[test]
    fn structured_tool_result_is_explicitly_downgraded_when_micro_compacted() {
        use crate::context::compression::{Compressor, MicroCompactor};
        use crate::context::partitions::ContextPartitions;
        use crate::context::token_engine::ContextTokenEngine;

        let durable = DurableContent {
            schema_version: 1,
            blocks: vec![DurableContentBlock::Image {
                source: DurableSource::Base64 {
                    data: "aW1hZ2U=".into(),
                },
                media_type: Some("image/png".into()),
                provider_options: None,
            }],
        };
        let mut partitions = ContextPartitions::default();
        let message = Message::tool(vec![ContentPart::ToolResult {
            call_id: "call-1".into(),
            output: "x".repeat(12_000),
            is_error: false,
            durable_content: Some(durable),
        }]);
        partitions.history.push(message, 3_000);
        let engine = ContextTokenEngine::char_approx();
        MicroCompactor.compress(&mut partitions, 0, 0, 0, &engine);
        let Content::Parts(parts) = &partitions.history.messages[0].content else {
            panic!("tool message remains structured")
        };
        let [
            ContentPart::ToolResult {
                durable_content,
                output,
                ..
            },
        ] = parts.as_slice()
        else {
            panic!("tool message keeps its result part")
        };
        assert!(durable_content.is_none());
        assert!(output.starts_with("[tool result:"));
    }

    /// §12.3 rule 11 · the two candidate forms agree on `state_digest` for the same logical state.
    ///
    /// This is what makes them interchangeable rather than merely both legal: a host that switches
    /// from full-state to rebase does not change what its checkpoints *mean*, only how much they
    /// re-serialise.
    #[test]
    fn a_rebase_and_a_full_state_candidate_agree_on_the_state_digest() {
        let envelopes = turn_envelopes();
        let mut runtime = Runtime::new();
        drive(&mut runtime, &envelopes[..2]);
        let base = runtime
            .checkpoint()
            .decode()
            .expect("the base candidate verifies");
        drive(&mut runtime, &envelopes[2..4]);

        let full_state = runtime.checkpoint();
        let rebase = runtime
            .tx
            .checkpoint_rebase(
                &CheckpointBoundary {
                    through_step_seq: base.through_step_seq(),
                    covered_head: base.covered_transaction_head_digest().clone(),
                },
                base.logical_state().clone(),
            )
            .expect("a rebase is assemblable");

        assert_eq!(
            full_state.through_step_seq, rebase.through_step_seq,
            "both cover the same prefix",
        );
        assert_eq!(full_state.covered_head, rebase.covered_head);
        assert_ne!(
            full_state.state_digest, rebase.state_digest,
            "they carry *different* logical states — one at the head, one at the base",
        );
        assert_eq!(
            rebase.state_digest,
            *base.state_digest(),
            "and a rebase carries its base's state forward untouched, byte for byte",
        );

        // Restoring either one lands on the same place, which is the property the digests are
        // evidence *for*.
        let from_full = runtime.restore(&full_state.decode().unwrap());
        let from_rebase = runtime.restore(&rebase.decode().unwrap());
        assert_eq!(surface(&from_full), surface(&from_rebase));
        assert_eq!(surface(&from_full), surface(&runtime));
    }

    /// **Verification 2** · restore cost is bounded by the tail, not by how long the run is.
    ///
    /// Deterministic counters, not a timer: the claim is about how much history is read, and that is
    /// a number the restore can report exactly. The run is driven to three different lengths and the
    /// cost is asserted *equal* across all three — not merely "small".
    #[test]
    fn long_run_restore_cost_is_bounded_by_the_tail_not_the_run() {
        fn cost_after(turns: usize) -> RestoreCost {
            let mut runtime = Runtime::new();
            runtime.submit(&syscall_config_with(|config| {
                config.execution_policy = Some(ExecutionPolicy {
                    max_turns: Some(10_000),
                    ..ExecutionPolicy::default()
                });
            }));
            let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));
            let mut effect = effect_id(started.step_seq);
            let mut at = 1_700_000_002_000;
            for turn in 0..turns {
                let acted = runtime.submit(&provider_result(
                    &format!("in-acted-{turn}"),
                    at,
                    &effect,
                    vec![tool_call(
                        &format!("call-{turn}"),
                        "search",
                        json!({ "q": turn }),
                    )],
                ));
                at += 1_000;
                let results = runtime.submit(&tools_resolved(
                    &format!("in-results-{turn}"),
                    at,
                    &effect_id(acted.step_seq),
                    &[(&format!("call-{turn}"), "a source", false)],
                ));
                at += 1_000;
                effect = effect_id(results.step_seq);
            }

            // The host checkpoints at the head and hands the restore only what is above it.
            let checkpoint = runtime.checkpoint().decode().expect("verifies");
            let after = runtime.journal_from(&checkpoint);
            assert!(after.is_empty(), "the checkpoint covers the whole journal");
            Runtime::restore_with(Some(&checkpoint), &after)
                .restore_cost
                .expect("a restored runtime reports its cost")
        }

        let short = cost_after(2);
        let medium = cost_after(8);
        let long = cost_after(32);

        assert_eq!(short.total_transitions(), 0, "nothing is replayed at all");
        assert_eq!(
            (short, medium),
            (medium, long),
            "restore cost does not grow with the length of the run",
        );

        // And the contrast that makes the number mean something: without a checkpoint the same
        // restore reads the whole journal.
        let mut runtime = Runtime::new();
        drive(&mut runtime, &turn_envelopes());
        let journal = runtime.journal.clone();
        let from_genesis = Runtime::restore_with(None, &journal);
        assert_eq!(
            from_genesis.restore_cost.unwrap().records_before_checkpoint,
            journal.len() as u64,
            "the no-checkpoint arm is O(run), which is exactly what §12 replaces",
        );
        assert_eq!(surface(&from_genesis), surface(&runtime));
    }

    /// §12.2 last line · with no checkpoint the fold starts at genesis and runs the **same** path.
    #[test]
    fn a_restore_without_a_checkpoint_uses_the_same_transaction_fold() {
        let mut runtime = Runtime::new();
        drive(&mut runtime, &turn_envelopes()[..4]);
        let from_genesis = Runtime::restore_with(None, &runtime.journal);
        assert_eq!(surface(&from_genesis), surface(&runtime));
        assert_eq!(from_genesis.restore_cost.unwrap().tail_inputs_replayed, 0);
    }

    /// **Verification 3, row 1** · §12.3 rule 5: a crash between install and ack still restores
    /// from the installed checkpoint.
    ///
    /// The ack is a *retention* signal, not a durability one. Nothing about the checkpoint's
    /// validity depends on it, which is why this row is a test rather than a caveat.
    #[test]
    fn crash_matrix_install_then_crash_before_ack_restores_from_the_installed_checkpoint() {
        let envelopes = turn_envelopes();
        let mut runtime = Runtime::new();
        drive(&mut runtime, &envelopes[..4]);

        let candidate = runtime.checkpoint();
        let checkpoint = candidate.decode().expect("the host installed this blob");
        // ... and then the process dies. `note_checkpoint_acked` was never called.
        let restored = runtime.restore(&checkpoint);
        assert_eq!(surface(&restored), surface(&runtime));
        assert_eq!(restored.restore_cost.unwrap().records_before_checkpoint, 0);

        // §12.2 line 8 · what the host is handed back: the effects to (re-)execute, or a terminal.
        let recovered = restore_operation(
            Some(&checkpoint),
            &[],
            ConfigDefaults::default(),
            InMemoryRecordIndex::new(),
        )
        .expect("the ladder runs");
        assert_eq!(
            recovered
                .pending_effects()
                .iter()
                .map(|effect| effect.tag())
                .collect::<Vec<_>>(),
            vec![EffectKindTag::CallProvider],
            "§5g-1 · the effect the operation is waiting on is exposed again",
        );
        assert!(recovered.terminal().is_none(), "the run had not ended");
    }

    /// **Verification 3, row 2** · §12.3 rules 1 and 3: records appended after the candidate are
    /// kept as tail, and a restore replays them from the journal.
    #[test]
    fn crash_matrix_appends_after_a_candidate_are_restored_from_the_journal() {
        let envelopes = turn_envelopes();
        let mut runtime = Runtime::new();
        drive(&mut runtime, &envelopes[..4]);

        let checkpoint = runtime.checkpoint().decode().expect("verifies");
        // The candidate is a read: the operation keeps going while the host persists the blob.
        drive(&mut runtime, &envelopes[4..6]);

        let restored = runtime.restore(&checkpoint);
        let cost = restored.restore_cost.unwrap();
        assert_eq!(
            cost.records_after_checkpoint, 2,
            "the two records appended after the candidate are replayed from the journal",
        );
        assert_eq!(cost.records_before_checkpoint, 0);
        assert_eq!(surface(&restored), surface(&runtime));
    }

    /// **Verification 3, row 3** · §12.3 rule 2: install does not require the covered head to still
    /// be the current head.
    #[test]
    fn crash_matrix_install_when_the_covered_head_has_moved_on() {
        let envelopes = turn_envelopes();
        let mut runtime = Runtime::new();
        drive(&mut runtime, &envelopes[..4]);
        let candidate = runtime.checkpoint();
        let covered = candidate.through_step_seq;

        drive(&mut runtime, &envelopes[4..]);
        assert_ne!(
            runtime.tx.head().unwrap().step_seq,
            covered,
            "the journal has moved past the covered head",
        );

        // The ack still names a prefix of *this* journal, which is the only precondition rule 2
        // leaves standing.
        let mut acked = Runtime::new();
        drive(&mut acked, &envelopes);
        let usage = acked
            .tx
            .note_checkpoint_acked(&candidate.boundary())
            .expect("a checkpoint that covers a prefix is ackable after the head moved");
        assert_eq!(
            usage.records, 3,
            "acking reclaims the covered prefix and keeps the rest as tail",
        );

        // And the blob still restores to the prefix it was taken over.
        let restored = runtime.restore(&candidate.decode().unwrap());
        assert_eq!(surface(&restored), surface(&runtime));
    }

    /// **Verification 3, row 4** · §12.3 rules 6, 7 and 10: after an ack the prefix may be
    /// reclaimed, and a redelivery from down there is still answered — by reference.
    ///
    /// This is the row that would have been silently wrong without the ledger: with the record
    /// gone, a `prepare` that consulted only the journal would accept the input a **second** time.
    #[test]
    fn crash_matrix_ack_then_prune_then_restore_still_answers_a_redelivery() {
        let envelopes = turn_envelopes();
        let mut runtime = Runtime::new();
        drive(&mut runtime, &envelopes[..4]);

        let checkpoint = runtime.checkpoint().decode().expect("verifies");
        runtime
            .tx
            .note_checkpoint_acked(&checkpoint.boundary())
            .expect("the boundary names a prefix of this journal");

        // Retention reclaims everything the checkpoint covers: the restore gets the blob and an
        // empty journal, exactly as a pruned host would hand it over.
        let mut restored = Runtime::restore_with(Some(&checkpoint), &[]);
        assert_eq!(surface(&restored), surface(&runtime));

        // A redelivery of an input from the reclaimed prefix is acknowledged, not re-accepted.
        let redelivered = restored.prepare(&envelopes[3]);
        let RecordPreparation::Replayed(replay) = redelivered else {
            panic!("a redelivery below the checkpoint base must not be accepted again");
        };
        assert_eq!(replay.step_seq, WireU64::new(3));
        assert_eq!(
            replay.record_digest,
            *runtime.journal[3].record_digest(),
            "§12.3 rule 10 · the answer is the original step and record digest",
        );
        assert!(
            replay.committed_step.is_none() && replay.record.is_none(),
            "and it carries no step payload — the guarantee down there is idempotent \
             acknowledgement, not step reproduction",
        );

        // The operation still runs forward from where it was.
        drive(&mut restored, &envelopes[4..]);
        let mut uninterrupted = Runtime::new();
        drive(&mut uninterrupted, &envelopes);
        assert_eq!(surface(&restored), surface(&uninterrupted));
    }

    /// §12.3 rule 8 / Task 16 acceptance · identity does not move across a restore.
    #[test]
    fn a_restore_preserves_effect_terminal_attempt_and_handle_identity() {
        let envelopes = turn_envelopes();
        let mut runtime = Runtime::new();
        drive(&mut runtime, &envelopes[..4]);

        let before_effects: Vec<String> = runtime
            .tx
            .pending_effects()
            .map(|effect| effect.effect_id.to_string())
            .collect();
        let before_handles =
            serde_json::to_value(runtime.driver.project_logical_state().context_vm.handles)
                .unwrap();
        let before_attempts =
            serde_json::to_value(runtime.driver.project_logical_state().scheduler.attempts)
                .unwrap();

        let checkpoint = runtime.checkpoint().decode().expect("verifies");
        let mut restored = Runtime::restore_with(Some(&checkpoint), &[]);

        assert!(!before_effects.is_empty(), "the fixture has live identity");
        assert_eq!(
            restored
                .tx
                .pending_effects()
                .map(|effect| effect.effect_id.to_string())
                .collect::<Vec<_>>(),
            before_effects,
            "§5g-1 · appended-but-unpublished effects are re-exposed under their own ids",
        );
        assert_eq!(
            serde_json::to_value(restored.driver.project_logical_state().context_vm.handles)
                .unwrap(),
            before_handles,
            "handle identity survives",
        );
        assert_eq!(
            serde_json::to_value(restored.driver.project_logical_state().scheduler.attempts)
                .unwrap(),
            before_attempts,
            "task attempt identity survives",
        );

        // A terminal survives too, and closes the restored operation to the same inputs.
        drive(&mut restored, &envelopes[4..]);
        let terminal = restored.tx.terminal().cloned().expect("the run ended");
        let after_terminal =
            Runtime::restore_with(Some(&restored.checkpoint().decode().unwrap()), &[]);
        assert_eq!(
            after_terminal.tx.terminal(),
            Some(&terminal),
            "the terminal is restored as the same terminal, not re-derived",
        );
    }

    /// §12.3 · the hard tail limit is a retryable `CheckpointRequired`, and there is no latch.
    ///
    /// The whole arc, because the latch this replaces was only visibly wrong at the *end* of it:
    /// refuse → checkpoint → ack → the same envelope, unchanged, succeeds.
    #[test]
    fn a_full_tail_refuses_retryably_and_the_same_envelope_succeeds_after_an_ack() {
        let mut runtime = Runtime::new();
        runtime.submit(&syscall_config_with(|config| {
            config.recovery_policy = Some(super::super::command::RecoveryPolicy {
                provider_recovery_attempts: None,
                output_recovery_attempts: None,
                tail_bounds: Some(super::super::command::TailBoundsPolicy {
                    soft_records: Some(WireU64::new(2)),
                    hard_records: Some(WireU64::new(3)),
                    soft_bytes: Some(WireU64::new(64 * 1024)),
                    hard_bytes: Some(WireU64::new(1024 * 1024)),
                }),
            });
        }));
        let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));
        let acted = runtime.submit(&provider_result(
            "in-acted",
            1_700_000_002_000,
            &effect_id(started.step_seq),
            vec![tool_call("call-1", "search", json!({"q": "sources"}))],
        ));

        let next = tools_resolved(
            "in-results",
            1_700_000_003_000,
            &effect_id(acted.step_seq),
            &[("call-1", "three sources found", false)],
        );
        let fault = runtime.reject(&next);
        assert_eq!(fault.code, KernelFaultCode::CheckpointRequired);
        assert!(fault.is_retryable(), "exactly one code says retry");

        // The refusal's shape is a contract in its own right — it is what tells four hosts to take
        // a checkpoint rather than to give up — so it is frozen as a fixture.
        let produced = json!({
            "expect": "checkpoint_required",
            "description":
                "Spec 12.3 · the next transaction would carry the journal tail past its hard limit, \
                 so `prepare` refuses with the one retryable fault code and zero mutation. The \
                 input was never accepted: after a checkpoint candidate is installed and acked, the \
                 *same* envelope — same input id, same clock, same payload — is submitted again and \
                 commits. There is no permanent overflow latch: an acked checkpoint takes the tail \
                 pressure straight back to nominal.",
            "tail_bounds": {
                "soft_records": "2",
                "hard_records": "3",
                "soft_bytes": "65536",
                "hard_bytes": "1048576",
            },
            "tail_usage_at_refusal": {
                "records": runtime.tx.tail_usage().records.to_string(),
            },
            "refused_envelope": serde_json::to_value(&next).unwrap(),
            "fault": serde_json::to_value(&fault).unwrap(),
            "retryable": fault.is_retryable(),
        });
        let expected = golden(
            "reject_transaction_checkpoint_required_tail_full.json",
            &produced,
        );
        assert_eq!(produced, expected, "the CheckpointRequired refusal drifted");

        // Zero mutation: the refusal moved nothing, so the checkpoint below covers the same prefix
        // the refusal saw.
        let head_before = runtime.tx.head().expect("a head");
        let candidate = runtime.checkpoint();
        assert_eq!(candidate.through_step_seq, head_before.step_seq);

        runtime
            .tx
            .note_checkpoint_acked(&candidate.boundary())
            .expect("the ack names this journal's head");

        // The *same* envelope, byte for byte — §5e-3 forbids re-stamping the clock — now commits.
        let committed = runtime.submit(&next);
        assert_eq!(committed.step_seq, WireU64::new(3));
        assert_eq!(
            runtime.tx.tail_pressure(),
            TailPressure::Nominal,
            "an acked checkpoint moves the pressure straight back — there is no latch",
        );
    }

    /// §12.3 · crossing the soft watermark is advice, delivered once, on the commit that crossed it.
    #[test]
    fn the_soft_watermark_is_advice_delivered_once_on_the_crossing() {
        let mut runtime = Runtime::new();
        let genesis = runtime.submit(&syscall_config_with(|config| {
            config.recovery_policy = Some(super::super::command::RecoveryPolicy {
                provider_recovery_attempts: None,
                output_recovery_attempts: None,
                tail_bounds: Some(super::super::command::TailBoundsPolicy {
                    soft_records: Some(WireU64::new(2)),
                    hard_records: Some(WireU64::new(8)),
                    soft_bytes: Some(WireU64::new(64 * 1024)),
                    hard_bytes: Some(WireU64::new(1024 * 1024)),
                }),
            });
        }));
        assert!(
            genesis.checkpoint_advice.is_none(),
            "one record is not a watermark crossing",
        );

        let started = runtime.submit(&agent_start("in-start", 1_700_000_001_000));
        let advice = started
            .checkpoint_advice
            .expect("the second record takes the tail to the soft watermark");
        assert_eq!(advice.through_step_seq, started.step_seq);
        assert_eq!(advice.usage.records, 2);
        assert_eq!(advice.bounds.soft_records, WireU64::new(2));

        let acted = runtime.submit(&provider_result(
            "in-acted",
            1_700_000_002_000,
            &effect_id(started.step_seq),
            vec![tool_call("call-1", "search", json!({"q": "sources"}))],
        ));
        assert!(
            acted.checkpoint_advice.is_none(),
            "advice is edge-triggered: staying over the watermark is not news",
        );
    }

    /// §12.3 rule 9 · rebuilding after a poisoned transaction costs O(tail) when a checkpoint
    /// exists.
    ///
    /// The failure this replaces re-executed every accepted input of the run, so the cost of one
    /// CAS conflict grew with the age of the operation. Here the recovery path *is* the restore
    /// path, and the counter says so.
    #[test]
    fn rebuilding_after_a_conflict_costs_the_tail_not_the_run() {
        let envelopes = turn_envelopes();
        let mut runtime = Runtime::new();
        drive(&mut runtime, &envelopes[..6]);
        let checkpoint = runtime.checkpoint().decode().expect("verifies");

        // A CAS conflict poisons the transaction: the journal moved under this runtime and §8.3
        // leaves exactly one way forward.
        let preparation = runtime.prepare(&envelopes[6]);
        let token = preparation.token().expect("prepared").clone();
        let fault = runtime.tx.note_append_conflict(&token, None);
        assert_eq!(fault.code, KernelFaultCode::TransactionConflict);
        assert!(runtime.tx.is_poisoned());

        let rebuilt = Runtime::restore_with(Some(&checkpoint), &[]);
        let cost = rebuilt.restore_cost.unwrap();
        assert_eq!(
            cost.total_transitions(),
            0,
            "the rebuild reads the checkpoint and nothing else — O(tail), not O(run)",
        );
        assert!(!rebuilt.tx.is_poisoned());
    }

    /// A checkpoint whose logical state was edited fails the restore's own re-projection check.
    ///
    /// The point of the check is that it catches a *hydration* gap as well as a tampered blob: both
    /// present as "the state that came back is not the state that was captured".
    #[test]
    fn a_restore_that_does_not_reproduce_the_captured_state_fails_closed() {
        let envelopes = turn_envelopes();
        let mut runtime = Runtime::new();
        drive(&mut runtime, &envelopes[..4]);
        let checkpoint = runtime.checkpoint().decode().expect("verifies");

        // Re-assemble the same header over a *different* logical state, so every digest is
        // internally consistent and only the state is wrong. A blob edited in storage would be
        // caught by `verify()`; this one gets past it and has to be caught by the re-projection.
        // A *derived* field is the honest probe here: the partition token counters come back from
        // re-pushing the messages, so a forged counter is exactly what a hydration gap would look
        // like from the outside — the state that came back is not the state that was captured.
        let mut state = checkpoint.logical_state().clone();
        state.context_vm.partition_tokens.history += 7;
        let forged = KernelCheckpoint::assemble(CheckpointDraft {
            operation_id: operation(),
            genesis_digest: checkpoint.genesis_digest().clone(),
            base_step_seq: checkpoint.base_step_seq(),
            base_record_digest: checkpoint.base_record_digest().clone(),
            through_step_seq: checkpoint.through_step_seq(),
            covered_transaction_head_digest: checkpoint.covered_transaction_head_digest().clone(),
            logical_state: state,
            tail_inputs: Vec::new(),
        })
        .expect("a self-consistent checkpoint over a different state");

        let error = restore_operation(
            Some(&forged),
            &[],
            ConfigDefaults::default(),
            InMemoryRecordIndex::new(),
        )
        .expect_err("a state that does not come back is a refusal, not a warning");
        assert_eq!(error.code, KernelFaultCode::CheckpointCorrupted);
        assert!(error.message.contains("restored logical state hashes to"));
    }

    /// §5e-5 · the tail bound the transaction enforces is the one the genesis record froze, not
    /// the binary's baseline.
    #[test]
    fn the_tail_bound_comes_from_the_genesis_configuration() {
        let mut runtime = Runtime::new();
        assert_eq!(
            runtime.tx.bounds(),
            TailBounds::DEFAULT,
            "before genesis the transaction runs on the bootstrap baseline",
        );

        runtime.submit(&syscall_config_with(|config| {
            config.recovery_policy = Some(super::super::command::RecoveryPolicy {
                provider_recovery_attempts: None,
                output_recovery_attempts: None,
                tail_bounds: Some(super::super::command::TailBoundsPolicy {
                    soft_records: Some(WireU64::new(4)),
                    hard_records: Some(WireU64::new(8)),
                    soft_bytes: Some(WireU64::new(64 * 1024)),
                    hard_bytes: Some(WireU64::new(256 * 1024)),
                }),
            });
        }));

        assert_eq!(
            runtime.tx.bounds(),
            TailBounds::new(4, 8, 64 * 1024, 256 * 1024).unwrap(),
            "the genesis record's resolved configuration is what bounds the tail",
        );
        assert_eq!(
            runtime
                .tx
                .config()
                .expect("configured")
                .recovery_policy
                .tail_bounds,
            runtime.tx.bounds(),
            "and it is frozen in the record, so a rebuild re-derives the same bound",
        );
    }
}
