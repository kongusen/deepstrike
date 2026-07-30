//! Kernel effects, their resolutions and the payload shapes both sides carry
//! (spec §7.8, §7.9, §7.10).
//!
//! Three rules shape this module:
//!
//! 1. **An effect is something the host must execute and report back.** Terminals, observations,
//!    synchronous compaction, knowledge sweeps and budget reports left the union (§7.8, §22.7):
//!    they are facts, not commands, so they can never sit in the pending-effect table waiting for
//!    a resolution that will never come.
//! 2. **Every effect has exactly one matching success payload and the same failure path.**
//!    [`EffectKindTag::expected_success`] is a bijection onto [`EffectSuccessTag`], and
//!    [`KernelEffect::accept_outcome`] refuses any other pairing — a host cannot answer a
//!    `QueryMemory` with a provider message. Milestone evaluation is not special: it fails through
//!    the same [`HostEffectFailure`] as everything else (B7).
//! 3. **The kernel never retries** (DEC-5). A `Failed` outcome triggers exactly one policy
//!    decision; `retryable` is host diagnostics, not an instruction, and a host that wants the
//!    same intent attempted again must ask for it with a new causation.
//!
//! Two omissions are deliberate. No outcome payload carries a wall clock (DEC-2) — the envelope's
//! accepted time is the only clock fact — and no external payload carries a filesystem path
//! (§7.10 rule 7): [`PayloadRef`] is an opaque locator the kernel never interprets.

use std::fmt;

use serde::de::{self, Deserializer, Visitor};
use serde::{Deserialize, Serialize, Serializer};

use super::root::{LogicalAgentSpec, MessageRole};
use super::scalar::{
    AttemptId, BoundedJson, CallId, EffectId, FiniteF64, HandleId, InputId, MAX_ID_BYTES,
    MemoryBindingId, NodeId, SCALAR_ERROR_MARKER, TaskId, WireScalarError, WireU64,
};
use super::syscall::{MemoryKind, SyscallCausation};

// ---------------------------------------------------------------------------------------------
// opaque references
// ---------------------------------------------------------------------------------------------

#[doc(hidden)]
pub(crate) fn validate_opaque(label: &'static str, value: &str) -> Result<(), WireScalarError> {
    if value.is_empty() {
        return Err(WireScalarError::new(format!("{label} must not be empty")));
    }
    if value.len() > MAX_ID_BYTES {
        return Err(WireScalarError::new(format!(
            "{label} is {} bytes; the bound is {MAX_ID_BYTES}",
            value.len()
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(WireScalarError::new(format!(
            "{label} must not contain control characters"
        )));
    }
    Ok(())
}

/// Branded opaque reference. Same discipline as [`super::scalar`]'s identities: a non-empty,
/// bounded, control-character-free string that is never a number and never a path.
macro_rules! wire_opaque_ref {
    ($(#[$doc:meta])* $name:ident, $label:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, WireScalarError> {
                let value = value.into();
                $crate::runtime::kernel::wire::effect::validate_opaque($label, &value)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                struct RefVisitor;

                impl Visitor<'_> for RefVisitor {
                    type Value = $name;

                    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        f.write_str(concat!("a non-empty ", $label, " string"))
                    }

                    fn visit_str<E: de::Error>(self, value: &str) -> Result<Self::Value, E> {
                        $name::new(value).map_err(|err| {
                            E::custom(format!("{SCALAR_ERROR_MARKER}: {}", err.message))
                        })
                    }

                    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Self::Value, E> {
                        Err(E::custom(format!(
                            "{SCALAR_ERROR_MARKER}: {} must be a branded string, got {value}",
                            $label
                        )))
                    }

                    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Self::Value, E> {
                        Err(E::custom(format!(
                            "{SCALAR_ERROR_MARKER}: {} must be a branded string, got {value}",
                            $label
                        )))
                    }

                    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                        Err(E::custom(format!(
                            "{SCALAR_ERROR_MARKER}: {} must be a branded string, got null",
                            $label
                        )))
                    }
                }

                deserializer.deserialize_any(RefVisitor)
            }
        }
    };
}

pub(crate) use wire_opaque_ref;

wire_opaque_ref!(
    /// Opaque locator for a payload the host persisted (§7.10 rule 7).
    ///
    /// **Not a path.** The kernel never joins it, never opens it and never lets a meta-tool
    /// interpret it; the only legal use is handing it back to the host in a `LoadPayload` effect.
    /// The historical `spool_ref`/`archive_ref` were real filesystem paths, which is exactly how a
    /// model-visible `read` tool learned to bypass the handle table.
    PayloadRef,
    "payload ref"
);
wire_opaque_ref!(
    /// Content digest of a payload or record. Algorithm-prefixed (`sha256:…`) so a future
    /// algorithm change is visible on the wire rather than silently reinterpreted.
    Digest,
    "digest"
);
wire_opaque_ref!(
    /// Kernel-minted idempotency token for one task launch. The host keys its own launch
    /// deduplication on it, which is what makes "the kernel does not retry" (DEC-5) safe: a host
    /// re-attempt with the same token is the same launch, not a second child.
    LaunchToken,
    "launch token"
);
wire_opaque_ref!(
    /// Opaque handle for one persisted memory record. Never a tenant, namespace or path.
    MemoryRecordRef,
    "memory record ref"
);

// ---------------------------------------------------------------------------------------------
// §7.8 · the effect union
// ---------------------------------------------------------------------------------------------

/// One action the kernel published and is now waiting on.
///
/// `effect_id` is minted by the kernel, never by the host; `causation_input_id` names the accepted
/// input that produced it, so every effect is traceable to one committed transition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KernelEffect {
    pub effect_id: EffectId,
    pub causation_input_id: InputId,
    pub effect: EffectKind,
}

impl KernelEffect {
    pub fn tag(&self) -> EffectKindTag {
        self.effect.tag()
    }

    /// The contract between a pending effect and the resolution a host offers for it.
    ///
    /// A `Failed` outcome is always admissible — the failure path is uniform across every effect
    /// kind (§7.9). A `Succeeded` outcome is admissible only if its payload is *the* success shape
    /// of this effect kind. Anything else is a host protocol error, not a kernel state change; the
    /// caller turns it into a [`KernelFaultCode::UnexpectedEffectOutcome`](super::fault::KernelFaultCode)
    /// rejection with zero mutation.
    pub fn accept_outcome(&self, outcome: &EffectOutcome) -> Result<(), EffectResolutionMismatch> {
        match outcome {
            EffectOutcome::Failed(_) => Ok(()),
            EffectOutcome::Succeeded(success) => {
                let expected = self.effect.tag().expected_success();
                let received = success.result.tag();
                if expected == received {
                    Ok(())
                } else {
                    Err(EffectResolutionMismatch {
                        effect_id: self.effect_id.clone(),
                        expected,
                        received,
                    })
                }
            }
        }
    }
}

/// A host answered a pending effect with the success payload of a different effect kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectResolutionMismatch {
    pub effect_id: EffectId,
    pub expected: EffectSuccessTag,
    pub received: EffectSuccessTag,
}

impl fmt::Display for EffectResolutionMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "effect {} expects a {} resolution, got {}",
            self.effect_id,
            self.expected.as_str(),
            self.received.as_str()
        )
    }
}

impl std::error::Error for EffectResolutionMismatch {}

/// The closed set of actions a host must execute and report back.
///
/// Newtype variants over strict structs, not inline struct variants: `deny_unknown_fields` does
/// not apply to an inline variant of an internally tagged enum.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EffectKind {
    CallProvider(CallProviderEffect),
    ExecuteTools(ExecuteToolsEffect),
    RequestApproval(RequestApprovalEffect),
    SpawnTasks(SpawnTasksEffect),
    PreemptTasks(PreemptTasksEffect),
    PersistMemory(PersistMemoryEffect),
    QueryMemory(QueryMemoryEffect),
    ArchivePageOut(ArchivePageOutEffect),
    /// P3 page-in. Reached only through `SyscallRequest::PageIn`; `read_result` and friends reduce
    /// to it instead of scanning a session log for the original bytes (§7.10 rule 4).
    LoadPayload(LoadPayloadEffect),
    EvaluateMilestone(EvaluateMilestoneEffect),
}

impl EffectKind {
    pub fn tag(&self) -> EffectKindTag {
        match self {
            Self::CallProvider(_) => EffectKindTag::CallProvider,
            Self::ExecuteTools(_) => EffectKindTag::ExecuteTools,
            Self::RequestApproval(_) => EffectKindTag::RequestApproval,
            Self::SpawnTasks(_) => EffectKindTag::SpawnTasks,
            Self::PreemptTasks(_) => EffectKindTag::PreemptTasks,
            Self::PersistMemory(_) => EffectKindTag::PersistMemory,
            Self::QueryMemory(_) => EffectKindTag::QueryMemory,
            Self::ArchivePageOut(_) => EffectKindTag::ArchivePageOut,
            Self::LoadPayload(_) => EffectKindTag::LoadPayload,
            Self::EvaluateMilestone(_) => EffectKindTag::EvaluateMilestone,
        }
    }
}

/// The discriminant of [`EffectKind`], usable without a payload.
///
/// This is the vocabulary `host_effect_support` declares against (DEC-8, §7.3): the kernel refuses
/// to emit an effect whose tag the host did not declare, and commits
/// [`KernelFaultCode::UnsupportedEffect`](super::fault::KernelFaultCode) instead of letting the
/// same effect produce four different outcomes in four languages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectKindTag {
    CallProvider,
    ExecuteTools,
    RequestApproval,
    SpawnTasks,
    PreemptTasks,
    PersistMemory,
    QueryMemory,
    ArchivePageOut,
    LoadPayload,
    EvaluateMilestone,
}

impl EffectKindTag {
    pub const ALL: [Self; 10] = [
        Self::CallProvider,
        Self::ExecuteTools,
        Self::RequestApproval,
        Self::SpawnTasks,
        Self::PreemptTasks,
        Self::PersistMemory,
        Self::QueryMemory,
        Self::ArchivePageOut,
        Self::LoadPayload,
        Self::EvaluateMilestone,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::CallProvider => "call_provider",
            Self::ExecuteTools => "execute_tools",
            Self::RequestApproval => "request_approval",
            Self::SpawnTasks => "spawn_tasks",
            Self::PreemptTasks => "preempt_tasks",
            Self::PersistMemory => "persist_memory",
            Self::QueryMemory => "query_memory",
            Self::ArchivePageOut => "archive_page_out",
            Self::LoadPayload => "load_payload",
            Self::EvaluateMilestone => "evaluate_milestone",
        }
    }

    /// The one success payload this effect kind accepts. Total and injective by construction.
    pub fn expected_success(self) -> EffectSuccessTag {
        match self {
            Self::CallProvider => EffectSuccessTag::Provider,
            Self::ExecuteTools => EffectSuccessTag::Tools,
            Self::RequestApproval => EffectSuccessTag::Approval,
            Self::SpawnTasks => EffectSuccessTag::TasksSpawned,
            Self::PreemptTasks => EffectSuccessTag::TasksPreempted,
            Self::PersistMemory => EffectSuccessTag::MemoryPersisted,
            Self::QueryMemory => EffectSuccessTag::MemoryQueried,
            Self::ArchivePageOut => EffectSuccessTag::PageOutArchived,
            Self::LoadPayload => EffectSuccessTag::PayloadLoaded,
            Self::EvaluateMilestone => EffectSuccessTag::MilestoneEvaluated,
        }
    }
}

impl fmt::Display for EffectKindTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------------------------
// §7.8 · effect payloads
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CallProviderEffect {
    pub context: RenderedContext,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolSchema>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecuteToolsEffect {
    pub calls: Vec<ToolCall>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestApprovalEffect {
    pub requests: Vec<ApprovalRequest>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpawnTasksEffect {
    pub tasks: Vec<TaskLaunch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<WorkflowBudget>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreemptTasksEffect {
    pub attempts: Vec<TaskAttemptRef>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersistMemoryEffect {
    pub binding: MemoryAccessBinding,
    pub memory: CanonicalMemoryWrite,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueryMemoryEffect {
    pub binding: MemoryAccessBinding,
    pub query: CanonicalMemoryQuery,
    /// Already clamped by the operation's retrieval policy — the host does not re-decide it.
    pub requested_k: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchivePageOutEffect {
    pub handle_id: HandleId,
    pub payload: PageOutPayload,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadPayloadEffect {
    pub handle_id: HandleId,
    pub payload_ref: PayloadRef,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluateMilestoneEffect {
    pub request: MilestoneRequest,
}

/// What the kernel rendered for one provider call.
///
/// Partitioned rather than flattened because the partitions have different cache lifetimes: the
/// identity and knowledge blocks and the leading `frozen_prefix_len` turns are byte-stable, the
/// state turn is rebuilt every call and therefore always last.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenderedContext {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub system_stable: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub system_knowledge: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub turns: Vec<ProviderMessage>,
    /// Volatile state turn, rendered after the cacheable history. `None` when there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_turn: Option<ProviderMessage>,
    /// Number of leading `turns` that form the byte-stable frozen prefix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frozen_prefix_len: Option<u32>,
}

/// A message on the provider boundary. Distinct from
/// [`LogicalMessage`](super::root::LogicalMessage): a rendered/returned message can carry tool
/// calls, an initial-context message cannot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderMessage {
    pub role: MessageRole,
    pub content: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<CallId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSchema {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "BoundedJson::is_null")]
    pub parameters: BoundedJson,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    pub call_id: CallId,
    pub name: String,
    #[serde(default, skip_serializing_if = "BoundedJson::is_null")]
    pub arguments: BoundedJson,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRequest {
    pub call_id: CallId,
    pub tool_name: String,
    #[serde(default, skip_serializing_if = "BoundedJson::is_null")]
    pub arguments: BoundedJson,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// One child launch the host must perform.
///
/// The kernel mints `task_id`, `attempt_id` and `launch_token` before the effect is published, so
/// the child's identity exists as a committed fact even if the launch fails — the historical
/// "spawned ⇒ Running with no launch event" gap (B13/B26/D10) is not expressible here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskLaunch {
    pub task_id: TaskId,
    pub attempt_id: AttemptId,
    pub launch_token: LaunchToken,
    pub node_id: NodeId,
    pub spec: LogicalAgentSpec,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowBudget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_total_tokens: Option<WireU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskAttemptRef {
    pub task_id: TaskId,
    pub attempt_id: AttemptId,
}

/// The operation's memory authority, carried on every memory effect so the host never has to infer
/// it. Opaque: never a tenant, a namespace or a path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryAccessBinding {
    pub binding_id: MemoryBindingId,
    pub capabilities: MemoryCapabilities,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryCapabilities {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub read: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub write: bool,
}

/// A memory write the **kernel** authored from a model proposal (§7.6).
///
/// The proposal contributed name/kind/content/evidence; provenance — accepted time and causation —
/// is kernel-derived, which is why this is a `Canonical…` type and the syscall payload is only a
/// `…Proposal`. `accepted_at_ms` is the envelope's accepted time restamped by the kernel, not a
/// host clock: DEC-2 bans host clocks on *outcome* payloads, not kernel-authored effect payloads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalMemoryWrite {
    pub name: String,
    pub kind: MemoryKind,
    pub content: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<String>,
    pub accepted_at_ms: WireU64,
    pub causation: SyscallCausation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalMemoryQuery {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<MemoryKind>,
    pub accepted_at_ms: WireU64,
    pub causation: SyscallCausation,
}

/// The body the host must archive under pressure, with the facts the kernel keeps afterwards.
/// After the resolution only the handle, digest, size and preview survive in core (§7.10 rule 5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageOutPayload {
    pub content: String,
    pub digest: Digest,
    pub original_size: WireU64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub preview: String,
}

/// Which phase of which contract the host must evaluate — and nothing else.
///
/// The pair is the point: `phase_id` is unique only *within* its contract (§7.3), so
/// `(contract_id, phase_id)` is the host's complete lookup key. Neither criteria nor required
/// evidence travels here: under the §7.3 contract skeleton core never learns them, so a field for
/// them could only ever be empty, and an always-empty field is an invitation for a host to believe
/// the kernel knows something it does not (§5.2, adjudication §5m-3 as refined by §5p).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MilestoneRequest {
    pub contract_id: String,
    pub phase_id: String,
}

// ---------------------------------------------------------------------------------------------
// §7.9 · resolution
// ---------------------------------------------------------------------------------------------

/// The single entry point for the outcome of a kernel-owned pending effect.
///
/// Two arms, no third. There is no "succeeded, but…", no partial result and no per-effect result
/// event of its own — which is what makes "each pending effect accepts exactly one resolution"
/// checkable rather than aspirational.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum EffectOutcome {
    Succeeded(EffectSucceeded),
    Failed(EffectFailed),
}

impl EffectOutcome {
    /// `Some(tag)` for a success, `None` for a failure — the failure path is kind-agnostic.
    pub fn success_tag(&self) -> Option<EffectSuccessTag> {
        match self {
            Self::Succeeded(success) => Some(success.result.tag()),
            Self::Failed(_) => None,
        }
    }
}

/// The success arm. The effect kind is **implied** by the effect being resolved and by
/// `result`'s own tag; the host never restates the effect id's kind here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectSucceeded {
    pub result: EffectSuccess,
}

/// The failure arm — identical for every effect kind (§7.9).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EffectFailed {
    pub failure: HostEffectFailure,
}

/// What the host can say about a failure. No wall clock, no host path, no stack, no raw vendor
/// error — only facts the kernel can act on and replay. Vendor diagnostics belong in the host's
/// own log, never in a kernel recovery decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostEffectFailure {
    pub kind: HostEffectFailureKind,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
    /// Host **advice**, never an instruction, and uniformly inert across all six failure kinds.
    ///
    /// DEC-5 is what makes that stronger than a naming convention: the kernel makes exactly one
    /// policy decision per failure and never re-emits the same intent, so there is no branch for
    /// this flag to select. The decision is chosen by *the effect kind the kernel itself
    /// published* — never by the kind the host reports and never by this field — which is why
    /// `retryable: true`, `retryable: false` and an absent `retryable` must plan byte-identical
    /// steps for every one of the ten effect kinds.
    ///
    /// A host that wants another attempt asks again with a new causation and keeps its own
    /// idempotency on the effect id / launch token. Retry ladders that a vendor would call
    /// rate-limit or unavailable back-off run entirely inside the host and never reach the
    /// kernel; when one is spent, the failure that arrives is [`HostEffectFailureKind::TransportExhausted`]
    /// and nothing else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
}

/// The stable, cross-effect executor failure classification (§7.9).
///
/// Six values, closed, and the same six for every effect kind. This is the whole of what a host
/// may say about *why* an effect did not happen: a vendor's own error taxonomy — `rate_limited`,
/// `overloaded`, `service_unavailable`, `context_length_exceeded`, `429`, `503` — has no
/// representation here and must be folded into one of these before it reaches the kernel. That
/// fold is the host's job precisely because the alternative is core reading raw vendor strings,
/// which is how one provider's wording silently became another provider's recovery policy (§22.8).
///
/// Deliberately *not* here:
///
/// * **cancellation** — that is `HostControl::Cancel`, never an effect failure;
/// * **provider context overflow** — that is [`ProviderOutcome::ContextOverflow`], a semantic
///   success outcome the kernel recovers from, not a transport failure;
/// * **rate limiting and transient unavailability** — the host retries those against its own
///   ladder and reports [`Self::TransportExhausted`] only once the ladder is spent. A distinct
///   `rate_limited` code would be an invitation for the kernel to run a back-off it cannot see the
///   inputs to, which is the redispatch DEC-5 deletes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostEffectFailureKind {
    /// The host exhausted its own transport retry ladder. The kernel never saw the backoff, and
    /// this is the *only* code a spent rate-limit or unavailable ladder may arrive as.
    TransportExhausted,
    /// The host cannot execute this effect at all — unknown kind included. DEC-7 makes answering
    /// with this code an obligation: dropping the effect or busy-waiting on it is a contract
    /// violation, and it is why an `if/else-if` chain without a final `else` is not a legal main
    /// loop.
    ProtocolError,
    StorageUnavailable,
    PermissionDenied,
    ResourceExhausted,
    Unknown,
}

impl HostEffectFailureKind {
    pub const ALL: [Self; 6] = [
        Self::TransportExhausted,
        Self::ProtocolError,
        Self::StorageUnavailable,
        Self::PermissionDenied,
        Self::ResourceExhausted,
        Self::Unknown,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::TransportExhausted => "transport_exhausted",
            Self::ProtocolError => "protocol_error",
            Self::StorageUnavailable => "storage_unavailable",
            Self::PermissionDenied => "permission_denied",
            Self::ResourceExhausted => "resource_exhausted",
            Self::Unknown => "unknown",
        }
    }
}

/// The closed per-effect success union. One variant per [`EffectKind`], no more and no fewer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EffectSuccess {
    Provider(ProviderSuccess),
    Tools(ToolsSuccess),
    Approval(ApprovalSuccess),
    TasksSpawned(TasksSpawnedSuccess),
    TasksPreempted(TasksPreemptedSuccess),
    MemoryPersisted(MemoryPersistedSuccess),
    MemoryQueried(MemoryQueriedSuccess),
    PageOutArchived(PageOutArchivedSuccess),
    PayloadLoaded(PayloadLoadedSuccess),
    MilestoneEvaluated(MilestoneEvaluatedSuccess),
}

impl EffectSuccess {
    pub fn tag(&self) -> EffectSuccessTag {
        match self {
            Self::Provider(_) => EffectSuccessTag::Provider,
            Self::Tools(_) => EffectSuccessTag::Tools,
            Self::Approval(_) => EffectSuccessTag::Approval,
            Self::TasksSpawned(_) => EffectSuccessTag::TasksSpawned,
            Self::TasksPreempted(_) => EffectSuccessTag::TasksPreempted,
            Self::MemoryPersisted(_) => EffectSuccessTag::MemoryPersisted,
            Self::MemoryQueried(_) => EffectSuccessTag::MemoryQueried,
            Self::PageOutArchived(_) => EffectSuccessTag::PageOutArchived,
            Self::PayloadLoaded(_) => EffectSuccessTag::PayloadLoaded,
            Self::MilestoneEvaluated(_) => EffectSuccessTag::MilestoneEvaluated,
        }
    }
}

/// The discriminant of [`EffectSuccess`], usable without a payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectSuccessTag {
    Provider,
    Tools,
    Approval,
    TasksSpawned,
    TasksPreempted,
    MemoryPersisted,
    MemoryQueried,
    PageOutArchived,
    PayloadLoaded,
    MilestoneEvaluated,
}

impl EffectSuccessTag {
    pub const ALL: [Self; 10] = [
        Self::Provider,
        Self::Tools,
        Self::Approval,
        Self::TasksSpawned,
        Self::TasksPreempted,
        Self::MemoryPersisted,
        Self::MemoryQueried,
        Self::PageOutArchived,
        Self::PayloadLoaded,
        Self::MilestoneEvaluated,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::Tools => "tools",
            Self::Approval => "approval",
            Self::TasksSpawned => "tasks_spawned",
            Self::TasksPreempted => "tasks_preempted",
            Self::MemoryPersisted => "memory_persisted",
            Self::MemoryQueried => "memory_queried",
            Self::PageOutArchived => "page_out_archived",
            Self::PayloadLoaded => "payload_loaded",
            Self::MilestoneEvaluated => "milestone_evaluated",
        }
    }

    /// The effect kind this success resolves. Inverse of
    /// [`EffectKindTag::expected_success`].
    pub fn resolves(self) -> EffectKindTag {
        match self {
            Self::Provider => EffectKindTag::CallProvider,
            Self::Tools => EffectKindTag::ExecuteTools,
            Self::Approval => EffectKindTag::RequestApproval,
            Self::TasksSpawned => EffectKindTag::SpawnTasks,
            Self::TasksPreempted => EffectKindTag::PreemptTasks,
            Self::MemoryPersisted => EffectKindTag::PersistMemory,
            Self::MemoryQueried => EffectKindTag::QueryMemory,
            Self::PageOutArchived => EffectKindTag::ArchivePageOut,
            Self::PayloadLoaded => EffectKindTag::LoadPayload,
            Self::MilestoneEvaluated => EffectKindTag::EvaluateMilestone,
        }
    }
}

impl fmt::Display for EffectSuccessTag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------------------------
// §7.9 · success payloads
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderSuccess {
    pub outcome: ProviderOutcome,
}

/// A provider call that reached the vendor and came back with something the kernel can reason
/// about. `now_ms` is gone (DEC-2): the historical field fed the governance rate limiter directly
/// and changed the byte fingerprint of every redelivery, which made idempotent replay unreachable
/// on the highest-frequency path in the system.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderOutcome {
    Completed(ProviderCompleted),
    /// A **semantic** outcome, not a transport failure: the kernel compacts and re-emits a
    /// provider effect. Classifying it as a failure is what used to collapse a recoverable
    /// overflow into an unrecoverable one.
    ContextOverflow(ProviderContextOverflow),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderCompleted {
    pub message: ProviderMessage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_input_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<ProviderStopReason>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderContextOverflow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_input_tokens: Option<u32>,
}

/// Typed stop reason — the canonical vocabulary every provider family maps *onto*.
///
/// The historical free-form string let each host forward its vendor's spelling verbatim, so the
/// same event arrived as `end_turn`, `stop`, `STOP` or `FINISH_REASON_STOP` depending on who was
/// calling. None of those decode here: a vendor word is a host-side mapping input, and `Other` is
/// the honest landing place for anything the six do not cover. `Other` is deliberately *not* a
/// pass-through — it carries no vendor text, because a string the kernel keeps is a string
/// something will eventually branch on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderStopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
    ContentFilter,
    Other,
}

impl ProviderStopReason {
    pub const ALL: [Self; 6] = [
        Self::EndTurn,
        Self::ToolUse,
        Self::MaxTokens,
        Self::StopSequence,
        Self::ContentFilter,
        Self::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::EndTurn => "end_turn",
            Self::ToolUse => "tool_use",
            Self::MaxTokens => "max_tokens",
            Self::StopSequence => "stop_sequence",
            Self::ContentFilter => "content_filter",
            Self::Other => "other",
        }
    }
}

impl fmt::Display for ProviderStopReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsSuccess {
    pub results: Vec<ToolResultPayload>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalSuccess {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approved_call_ids: Vec<CallId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub denied_call_ids: Vec<CallId>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TasksSpawnedSuccess {
    pub attempts: Vec<TaskLaunchOutcome>,
}

/// The launch acknowledgement. Historically this was an unconditional echo of the ids with a
/// hard-coded empty failure list, so "spawned" and "actually started" were indistinguishable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskLaunchOutcome {
    pub task_id: TaskId,
    pub attempt_id: AttemptId,
    pub outcome: TaskLaunchStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TaskLaunchStatus {
    Started(TaskLaunchStarted),
    Failed(TaskLaunchFailed),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskLaunchStarted {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskLaunchFailed {
    pub failure: TaskLaunchFailure,
}

/// Per-launch failure. Reuses the cross-effect classification so a launch failure is triaged with
/// the same vocabulary as every other host failure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskLaunchFailure {
    pub kind: HostEffectFailureKind,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TasksPreemptedSuccess {
    pub attempts: Vec<TaskPreemptOutcome>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskPreemptOutcome {
    pub task_id: TaskId,
    pub attempt_id: AttemptId,
    pub outcome: TaskPreemptStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum TaskPreemptStatus {
    Preempted(TaskPreempted),
    /// The attempt had already finished when the preemption arrived — a benign race, not a
    /// failure, and the kernel must be able to tell the two apart.
    AlreadyFinished(TaskAlreadyFinished),
    Failed(TaskPreemptFailed),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskPreempted {}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskAlreadyFinished {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskPreemptFailed {
    pub failure: TaskLaunchFailure,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryPersistedSuccess {
    pub receipt: MemoryPersistReceipt,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryPersistReceipt {
    pub binding_id: MemoryBindingId,
    pub record_ref: MemoryRecordRef,
    pub digest: Digest,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryQueriedSuccess {
    pub recalls: Vec<MemoryRecall>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryRecall {
    pub record_ref: MemoryRecordRef,
    pub name: String,
    pub kind: MemoryKind,
    pub content: String,
    /// Observation-only relevance score. Finite by construction and never a branch input —
    /// thresholds that gate kernel decisions use fixed-point `Ppm`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<FiniteF64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageOutArchivedSuccess {
    pub receipt: ArchiveReceipt,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveReceipt {
    pub handle_id: HandleId,
    pub payload_ref: PayloadRef,
    pub digest: Digest,
    pub original_size: WireU64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayloadLoadedSuccess {
    pub handle_id: HandleId,
    pub payload: InlinePayload,
}

/// A payload the host paged back in. `digest` and `original_size` let the kernel verify it is the
/// same body it archived.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InlinePayload {
    pub content: String,
    pub digest: Digest,
    pub original_size: WireU64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MilestoneEvaluatedSuccess {
    pub result: MilestoneCheckResult,
}

/// A milestone verdict. Note what is *not* here: an `error` field. Milestone execution failures
/// travel the same [`HostEffectFailure`] path as every other effect (B7) instead of being folded
/// into the verdict, so "the verifier could not run" and "the verifier said no" stay distinct.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MilestoneCheckResult {
    pub phase_id: String,
    pub passed: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_criteria: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<FiniteF64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
}

// ---------------------------------------------------------------------------------------------
// §7.10 · inline / external payload
// ---------------------------------------------------------------------------------------------

/// One tool result, either inline or already persisted by the host.
///
/// The host persists the body **before** submitting `External` and hands the kernel only a
/// reference, a digest, a size and a preview. That is the whole point: the large body never
/// crosses core and never enters a journal record, whereas the historical path pushed the full
/// body in, had the kernel compute a preview, pushed the **full body back out** to be spooled, and
/// wrote it to the journal twice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ToolResultPayload {
    Inline(InlineToolResult),
    External(ExternalToolResult),
}

impl ToolResultPayload {
    pub fn call_id(&self) -> &CallId {
        match self {
            Self::Inline(inline) => &inline.call_id,
            Self::External(external) => &external.call_id,
        }
    }

    /// The two failure facts, read the same way whichever side of the inline threshold the body
    /// landed on. Every caller that branches on failure must go through these: §7.10 rule 9 makes
    /// failure orthogonal to residency, so a check that is total over one arm and not the other is
    /// exactly the bug the rule exists to prevent.
    pub fn disposition(&self) -> ToolResultDisposition {
        match self {
            Self::Inline(inline) => inline.result.disposition,
            Self::External(external) => external.disposition,
        }
    }

    pub fn is_error(&self) -> bool {
        match self {
            Self::Inline(inline) => inline.result.is_error,
            Self::External(external) => external.is_error,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InlineToolResult {
    pub call_id: CallId,
    pub result: ToolResult,
}

/// A tool result whose body the host persisted before submitting.
///
/// **Failure and size are orthogonal** (§7.10 rule 9). A tool that fails after producing a huge
/// diagnostic body is not a rare shape — it is the *common* one — so this arm carries the same two
/// failure facts as [`ToolResult`]: whether the call errored, and whether the executor could keep
/// going. Without them an externalised failure was indistinguishable from an externalised success,
/// and a fatal one could not exist at all.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalToolResult {
    pub call_id: CallId,
    /// Opaque locator — see [`PayloadRef`]. Not a path, and never resolved by the kernel.
    pub payload_ref: PayloadRef,
    pub digest: Digest,
    pub original_size: WireU64,
    /// Bounded excerpt the kernel may keep resident and show the model.
    pub preview: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_error: bool,
    /// Mandatory, exactly as on the inline arm: the dangerous value must never be the silent one.
    pub disposition: ToolResultDisposition,
}

/// An inline tool result body. `call_id` lives on [`InlineToolResult`], not here — one id, one
/// place.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolResult {
    pub output: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_error: bool,
    /// Whether the executor can keep going after this result. **Mandatory** — see
    /// [`ToolResultDisposition`].
    pub disposition: ToolResultDisposition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u32>,
}

/// Can the batch continue past this result?
///
/// Binary and **required**: it has no serde default, so every result states it. The alternative —
/// defaulting to `recoverable` to save wire bytes — makes the dangerous value the silent one, which
/// is the wrong way round for a fail-closed contract, and it makes "the host did not say" and "the
/// host said it is fine" indistinguishable at exactly the point where they differ most.
///
/// The historical `is_fatal` + six-way `ToolErrorKind` collapsed to these two values because only
/// two distinctions ever changed a kernel decision. `user_interrupt` in particular is **not** here:
/// cancellation travels on `HostControl::Cancel` and nothing else (§7.9), so the rollback rung it
/// used to trigger dies with the legacy wire rather than being re-expressible as a tool result.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ToolResultDisposition {
    /// The model can adapt and the executor kept going. Ordinary failures are this: a committed,
    /// model-visible error result is the trained-set convention, and erasing the attempt teaches
    /// the model nothing.
    #[default]
    Recoverable,
    /// The executor stopped. Every call this batch dispatched but did not answer is closed out by
    /// the kernel with a visible "not executed" result, so the tool_call/tool_result pairing the
    /// provider contract requires stays total (see the tools arm of the driver's resolution).
    Fatal,
}

impl ToolResultDisposition {
    pub const ALL: [Self; 2] = [Self::Recoverable, Self::Fatal];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Recoverable => "recoverable",
            Self::Fatal => "fatal",
        }
    }

    pub fn is_fatal(self) -> bool {
        matches!(self, Self::Fatal)
    }
}

impl fmt::Display for ToolResultDisposition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a P3 handle's body currently lives.
///
/// `External` and `PagedOut` are deliberately different states: the first is "generated over the
/// inline limit and never was resident", the second is "was resident, evicted under pressure".
/// The historical implementation collapsed both into one `spool` notion, which is why a paged-out
/// body and an oversized one could not be told apart on restore.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PayloadResidency {
    Resident(ResidentPayload),
    External(ExternalResidency),
    PagedOut(PagedOutResidency),
    /// Body dropped entirely; only the preview and accounting survive.
    Collapsed(CollapsedPayload),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResidentPayload {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExternalResidency {
    pub payload_ref: PayloadRef,
    pub digest: Digest,
    pub original_size: WireU64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PagedOutResidency {
    pub payload_ref: PayloadRef,
    pub digest: Digest,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollapsedPayload {}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::PathBuf;

    use serde_json::{Value, json};

    use super::super::*;

    // -----------------------------------------------------------------------------------------
    // helpers
    // -----------------------------------------------------------------------------------------

    fn fixture_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/kernel-wire")
    }

    fn fixtures_with_prefix(prefix: &str) -> Vec<(String, Value)> {
        let dir = fixture_dir();
        let mut names: Vec<String> = fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", dir.display()))
            .map(|entry| {
                entry
                    .expect("dir entry")
                    .file_name()
                    .to_string_lossy()
                    .to_string()
            })
            .filter(|name| name.ends_with(".json") && name.starts_with(prefix))
            .collect();
        names.sort();
        assert!(!names.is_empty(), "no {prefix}*.json fixtures");
        names
            .into_iter()
            .map(|name| {
                let raw = fs::read_to_string(dir.join(&name)).unwrap();
                let value: Value = serde_json::from_str(&raw).unwrap();
                (name, value)
            })
            .collect()
    }

    fn keys(value: &Value, out: &mut BTreeSet<String>) {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    out.insert(key.clone());
                    keys(child, out);
                }
            }
            Value::Array(items) => items.iter().for_each(|item| keys(item, out)),
            _ => {}
        }
    }

    fn effect_json(outcome: Value) -> String {
        serde_json::to_string(&json!({
            "abi_version": KERNEL_ABI_VERSION,
            "operation_id": "op-1",
            "input_id": "in-1",
            "observed_at_ms": "1700000000000",
            "input": { "kind": "resolve_effect", "effect_id": "op-1:step:1:effect:0", "outcome": outcome },
        }))
        .unwrap()
    }

    fn decode_effect(outcome: Value) -> Result<WireEnvelope, WireRejection> {
        decode_envelope_json(&effect_json(outcome), &KernelBootstrapLimits::default())
    }

    fn call_id(id: &str) -> CallId {
        CallId::new(id).unwrap()
    }

    fn digest() -> Digest {
        Digest::new("sha256:3b1f4a7c9e2d05186a4c7f0b9d3e8c25714f6a0b8c5d2e9f1a3b6c8d0e2f4a61")
            .unwrap()
    }

    fn payload_ref() -> PayloadRef {
        PayloadRef::new("payload:01J8Y2QK7C4N0V").unwrap()
    }

    fn binding() -> MemoryAccessBinding {
        MemoryAccessBinding {
            binding_id: MemoryBindingId::new("binding-a").unwrap(),
            capabilities: MemoryCapabilities {
                read: true,
                write: true,
            },
        }
    }

    fn causation() -> SyscallCausation {
        SyscallCausation::ProviderTool(ProviderToolCausation {
            provider_effect_id: EffectId::new("op-1:step:1:effect:0").unwrap(),
            call_id: call_id("call-1"),
            task_id: TaskId::new("task-1").unwrap(),
        })
    }

    /// One sample per [`EffectKind`] variant, in [`EffectKindTag::ALL`] order.
    fn effect_samples() -> Vec<EffectKind> {
        vec![
            EffectKind::CallProvider(CallProviderEffect {
                context: RenderedContext::default(),
                tools: vec![ToolSchema {
                    name: "read_file".to_string(),
                    description: "read a file".to_string(),
                    parameters: BoundedJson::null(),
                }],
            }),
            EffectKind::ExecuteTools(ExecuteToolsEffect {
                calls: vec![ToolCall {
                    call_id: call_id("call-1"),
                    name: "read_file".to_string(),
                    arguments: BoundedJson::null(),
                }],
            }),
            EffectKind::RequestApproval(RequestApprovalEffect {
                requests: vec![ApprovalRequest {
                    call_id: call_id("call-1"),
                    tool_name: "rm".to_string(),
                    arguments: BoundedJson::null(),
                    reason: Some("destructive".to_string()),
                }],
            }),
            EffectKind::SpawnTasks(SpawnTasksEffect {
                tasks: vec![TaskLaunch {
                    task_id: TaskId::new("task-1").unwrap(),
                    attempt_id: AttemptId::new("task-1:attempt:1").unwrap(),
                    launch_token: LaunchToken::new("launch-1").unwrap(),
                    node_id: NodeId::new("node-a").unwrap(),
                    spec: LogicalAgentSpec::new("research"),
                }],
                budget: None,
            }),
            EffectKind::PreemptTasks(PreemptTasksEffect {
                attempts: vec![TaskAttemptRef {
                    task_id: TaskId::new("task-1").unwrap(),
                    attempt_id: AttemptId::new("task-1:attempt:1").unwrap(),
                }],
                reason: "budget exhausted".to_string(),
            }),
            EffectKind::PersistMemory(PersistMemoryEffect {
                binding: binding(),
                memory: CanonicalMemoryWrite {
                    name: "release pipeline".to_string(),
                    kind: MemoryKind::Project,
                    content: "tag v* publishes".to_string(),
                    description: String::new(),
                    evidence_refs: Vec::new(),
                    accepted_at_ms: WireU64::new(1_700_000_000_000),
                    causation: causation(),
                },
            }),
            EffectKind::QueryMemory(QueryMemoryEffect {
                binding: binding(),
                query: CanonicalMemoryQuery {
                    text: "release".to_string(),
                    kinds: vec![MemoryKind::Project],
                    accepted_at_ms: WireU64::new(1_700_000_000_000),
                    causation: causation(),
                },
                requested_k: 5,
            }),
            EffectKind::ArchivePageOut(ArchivePageOutEffect {
                handle_id: HandleId::new("handle-9").unwrap(),
                payload: PageOutPayload {
                    content: "the full tool output".to_string(),
                    digest: digest(),
                    original_size: WireU64::new(262_144),
                    preview: "the full…".to_string(),
                },
            }),
            EffectKind::LoadPayload(LoadPayloadEffect {
                handle_id: HandleId::new("handle-9").unwrap(),
                payload_ref: payload_ref(),
            }),
            EffectKind::EvaluateMilestone(EvaluateMilestoneEffect {
                request: MilestoneRequest {
                    contract_id: "brief-quality-v1".to_string(),
                    phase_id: "phase-2".to_string(),
                },
            }),
        ]
    }

    /// One sample per [`EffectSuccess`] variant, in the same order as [`effect_samples`].
    fn success_samples() -> Vec<EffectSuccess> {
        vec![
            EffectSuccess::Provider(ProviderSuccess {
                outcome: ProviderOutcome::Completed(ProviderCompleted {
                    message: ProviderMessage {
                        role: MessageRole::Assistant,
                        content: "done".to_string(),
                        tool_calls: Vec::new(),
                        tool_call_id: None,
                        tokens: None,
                    },
                    observed_input_tokens: Some(120),
                    observed_output_tokens: Some(8),
                    stop_reason: Some(ProviderStopReason::EndTurn),
                }),
            }),
            EffectSuccess::Tools(ToolsSuccess {
                results: vec![
                    ToolResultPayload::Inline(InlineToolResult {
                        call_id: call_id("call-1"),
                        result: ToolResult {
                            output: "ok".to_string(),
                            is_error: false,
                            disposition: ToolResultDisposition::Recoverable,
                            tokens: Some(2),
                        },
                    }),
                    ToolResultPayload::External(ExternalToolResult {
                        call_id: call_id("call-2"),
                        payload_ref: payload_ref(),
                        digest: digest(),
                        original_size: WireU64::new(1_048_576),
                        preview: "total 42".to_string(),
                        is_error: false,
                        disposition: ToolResultDisposition::Recoverable,
                    }),
                ],
            }),
            EffectSuccess::Approval(ApprovalSuccess {
                approved_call_ids: vec![call_id("call-1")],
                denied_call_ids: vec![call_id("call-2")],
            }),
            EffectSuccess::TasksSpawned(TasksSpawnedSuccess {
                attempts: vec![TaskLaunchOutcome {
                    task_id: TaskId::new("task-1").unwrap(),
                    attempt_id: AttemptId::new("task-1:attempt:1").unwrap(),
                    outcome: TaskLaunchStatus::Started(TaskLaunchStarted {}),
                }],
            }),
            EffectSuccess::TasksPreempted(TasksPreemptedSuccess {
                attempts: vec![TaskPreemptOutcome {
                    task_id: TaskId::new("task-1").unwrap(),
                    attempt_id: AttemptId::new("task-1:attempt:1").unwrap(),
                    outcome: TaskPreemptStatus::Preempted(TaskPreempted {}),
                }],
            }),
            EffectSuccess::MemoryPersisted(MemoryPersistedSuccess {
                receipt: MemoryPersistReceipt {
                    binding_id: MemoryBindingId::new("binding-a").unwrap(),
                    record_ref: MemoryRecordRef::new("memory:01J8Y2QK7C4N0W").unwrap(),
                    digest: digest(),
                },
            }),
            EffectSuccess::MemoryQueried(MemoryQueriedSuccess {
                recalls: vec![MemoryRecall {
                    record_ref: MemoryRecordRef::new("memory:01J8Y2QK7C4N0X").unwrap(),
                    name: "release pipeline".to_string(),
                    kind: MemoryKind::Project,
                    content: "tag v* publishes".to_string(),
                    score: Some(FiniteF64::new(0.82).unwrap()),
                }],
            }),
            EffectSuccess::PageOutArchived(PageOutArchivedSuccess {
                receipt: ArchiveReceipt {
                    handle_id: HandleId::new("handle-9").unwrap(),
                    payload_ref: payload_ref(),
                    digest: digest(),
                    original_size: WireU64::new(262_144),
                },
            }),
            EffectSuccess::PayloadLoaded(PayloadLoadedSuccess {
                handle_id: HandleId::new("handle-9").unwrap(),
                payload: InlinePayload {
                    content: "the full tool output".to_string(),
                    digest: digest(),
                    original_size: WireU64::new(262_144),
                },
            }),
            EffectSuccess::MilestoneEvaluated(MilestoneEvaluatedSuccess {
                result: MilestoneCheckResult {
                    phase_id: "phase-2".to_string(),
                    passed: true,
                    failed_criteria: Vec::new(),
                    score: None,
                    notes: String::new(),
                },
            }),
        ]
    }

    fn kernel_effect(effect: EffectKind) -> KernelEffect {
        KernelEffect {
            effect_id: EffectId::new("op-1:step:1:effect:0").unwrap(),
            causation_input_id: InputId::new("in-1").unwrap(),
            effect,
        }
    }

    // -----------------------------------------------------------------------------------------
    // §7.8 · the effect union
    // -----------------------------------------------------------------------------------------

    #[test]
    fn the_effect_union_is_exactly_the_ten_host_executable_actions() {
        let tags: BTreeSet<&str> = EffectKindTag::ALL.iter().map(|tag| tag.as_str()).collect();
        assert_eq!(
            tags,
            BTreeSet::from([
                "call_provider",
                "execute_tools",
                "request_approval",
                "spawn_tasks",
                "preempt_tasks",
                "persist_memory",
                "query_memory",
                "archive_page_out",
                "load_payload",
                "evaluate_milestone",
            ])
        );
        assert_eq!(EffectKindTag::ALL.len(), 10);

        let sampled: Vec<EffectKindTag> = effect_samples().iter().map(EffectKind::tag).collect();
        assert_eq!(
            sampled,
            EffectKindTag::ALL.to_vec(),
            "one sample per variant"
        );
    }

    #[test]
    fn terminal_and_observation_shapes_are_not_effects() {
        // §7.8 / §22.7: these all left the effect union. None of them may reappear as a tag.
        for gone in [
            "done",
            "terminal",
            "compact",
            "sync_compact",
            "knowledge_sweep",
            "workflow_completed",
            "control_rejection",
            "signal_disposition",
            "budget_usage",
            "spool_large_result",
        ] {
            assert!(
                !EffectKindTag::ALL.iter().any(|tag| tag.as_str() == gone),
                "{gone} is a terminal or an observation, not an effect"
            );
            let raw = json!({ "kind": gone });
            assert!(serde_json::from_value::<EffectKind>(raw).is_err());
        }
    }

    #[test]
    fn every_effect_carries_its_kernel_minted_id_and_causation() {
        for effect in effect_samples() {
            let value = serde_json::to_value(kernel_effect(effect)).unwrap();
            assert_eq!(value["effect_id"], json!("op-1:step:1:effect:0"));
            assert_eq!(value["causation_input_id"], json!("in-1"));
        }
    }

    // -----------------------------------------------------------------------------------------
    // §7.9 · one matching success per effect, one failure path for all of them
    // -----------------------------------------------------------------------------------------

    #[test]
    fn each_effect_kind_has_exactly_one_matching_success_kind() {
        let expected: Vec<EffectSuccessTag> = EffectKindTag::ALL
            .iter()
            .map(|tag| tag.expected_success())
            .collect();
        let distinct: BTreeSet<&str> = expected.iter().map(|tag| tag.as_str()).collect();
        assert_eq!(
            distinct.len(),
            EffectKindTag::ALL.len(),
            "the effect→success map must be a bijection"
        );
        assert_eq!(
            distinct,
            EffectSuccessTag::ALL
                .iter()
                .map(|tag| tag.as_str())
                .collect::<BTreeSet<&str>>()
        );

        let sampled: Vec<EffectSuccessTag> =
            success_samples().iter().map(EffectSuccess::tag).collect();
        assert_eq!(
            sampled, expected,
            "samples must line up 1:1 with the effects"
        );
    }

    #[test]
    fn a_resolution_of_the_wrong_kind_is_rejected_for_every_wrong_pair() {
        let effects = effect_samples();
        let successes = success_samples();
        for (i, effect) in effects.iter().enumerate() {
            let pending = kernel_effect(effect.clone());
            for (j, success) in successes.iter().enumerate() {
                let outcome = EffectOutcome::Succeeded(EffectSucceeded {
                    result: success.clone(),
                });
                let verdict = pending.accept_outcome(&outcome);
                if i == j {
                    assert!(
                        verdict.is_ok(),
                        "{:?} must accept its own success payload",
                        effect.tag()
                    );
                } else {
                    let mismatch = verdict.expect_err("kind mismatch must be refused");
                    assert_eq!(mismatch.effect_id, pending.effect_id);
                    assert_eq!(mismatch.expected, effect.tag().expected_success());
                    assert_eq!(mismatch.received, success.tag());
                }
            }
        }
    }

    #[test]
    fn every_effect_accepts_the_same_host_failure_including_milestone() {
        let kinds = [
            HostEffectFailureKind::TransportExhausted,
            HostEffectFailureKind::ProtocolError,
            HostEffectFailureKind::StorageUnavailable,
            HostEffectFailureKind::PermissionDenied,
            HostEffectFailureKind::ResourceExhausted,
            HostEffectFailureKind::Unknown,
        ];
        assert_eq!(kinds.len(), 6, "§7.9 fixes six executor failure classes");

        for effect in effect_samples() {
            let pending = kernel_effect(effect);
            for kind in kinds {
                let outcome = EffectOutcome::Failed(EffectFailed {
                    failure: HostEffectFailure {
                        kind,
                        message: "boom".to_string(),
                        retryable: None,
                    },
                });
                assert!(
                    pending.accept_outcome(&outcome).is_ok(),
                    "{:?} must have the same failure path as every other effect",
                    pending.effect.tag()
                );
            }
        }
    }

    #[test]
    fn the_outcome_union_has_exactly_two_arms() {
        let arms: BTreeSet<String> = [
            EffectOutcome::Succeeded(EffectSucceeded {
                result: success_samples().remove(0),
            }),
            EffectOutcome::Failed(EffectFailed {
                failure: HostEffectFailure {
                    kind: HostEffectFailureKind::Unknown,
                    message: String::new(),
                    retryable: None,
                },
            }),
        ]
        .iter()
        .map(|outcome| {
            serde_json::to_value(outcome).unwrap()["status"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
        assert_eq!(
            arms,
            BTreeSet::from(["succeeded".to_string(), "failed".to_string()])
        );

        for third in ["partial", "pending", "deferred", "succeeded_with_warnings"] {
            let raw = json!({ "status": third });
            assert!(
                serde_json::from_value::<EffectOutcome>(raw).is_err(),
                "{third} is not an outcome"
            );
        }
    }

    #[test]
    fn the_kernel_never_retries_so_retryable_is_host_advice_only() {
        // DEC-5: `retryable` may travel as diagnostics, but it is optional and the kernel's own
        // decision path is the failure kind. Its presence must not change decoding.
        let with = json!({
            "status": "failed",
            "failure": { "kind": "transport_exhausted", "message": "429", "retryable": true },
        });
        let outcome: EffectOutcome = serde_json::from_value(with).unwrap();
        match outcome {
            EffectOutcome::Failed(failed) => {
                assert_eq!(failed.failure.retryable, Some(true));
                assert_eq!(
                    failed.failure.kind,
                    HostEffectFailureKind::TransportExhausted
                );
            }
            EffectOutcome::Succeeded(_) => panic!("failed outcome decoded as succeeded"),
        }
    }

    // -----------------------------------------------------------------------------------------
    // §17 Task 14 · provider outcome and executor failure are the *only* canonical vocabulary
    // -----------------------------------------------------------------------------------------

    /// The canonical stop-reason vocabulary is closed, and every provider family maps onto it.
    ///
    /// Task 14's "core does not parse a raw vendor error string" has a quieter twin: core does not
    /// accept a raw vendor *word* either. Every spelling below is a real stop/finish reason from
    /// some vendor's wire, and none of them decodes — a host must map before it submits.
    #[test]
    fn every_provider_family_maps_onto_the_canonical_stop_reason_vocabulary() {
        let canonical: BTreeSet<&str> = ProviderStopReason::ALL
            .iter()
            .map(|reason| reason.as_str())
            .collect();
        assert_eq!(
            canonical,
            BTreeSet::from([
                "end_turn",
                "tool_use",
                "max_tokens",
                "stop_sequence",
                "content_filter",
                "other",
            ])
        );
        for reason in ProviderStopReason::ALL {
            let decoded: ProviderStopReason =
                serde_json::from_value(json!(reason.as_str())).unwrap();
            assert_eq!(decoded, reason);
        }

        // one row per vendor family the SDKs actually speak
        for vendor_word in [
            "stop",               // OpenAI chat completions
            "length",             // OpenAI chat completions
            "tool_calls",         // OpenAI chat completions
            "function_call",      // OpenAI legacy
            "content_filter",     // OpenAI — same word, different meaning class
            "STOP",               // Gemini (SCREAMING_CASE)
            "MAX_TOKENS",         // Gemini
            "SAFETY",             // Gemini
            "FINISH_REASON_STOP", // Gemini proto spelling
            "eos",                // several open-weight servers
            "eos_token",          // llama.cpp / vLLM
            "sensitive",          // GLM
            "insufficient_system_resource",
        ] {
            let decoded = serde_json::from_value::<ProviderStopReason>(json!(vendor_word));
            if vendor_word == "content_filter" {
                assert!(decoded.is_ok(), "content_filter *is* canonical");
                continue;
            }
            assert!(
                decoded.is_err(),
                "{vendor_word:?} is a vendor spelling; the host maps it, core never learns it"
            );
        }

        // `other` is the honest landing place, and it carries no vendor text with it
        let value = serde_json::to_value(ProviderStopReason::Other).unwrap();
        assert_eq!(value, json!("other"));
        assert!(
            serde_json::from_value::<ProviderStopReason>(
                json!({ "kind": "other", "raw": "insufficient_system_resource" })
            )
            .is_err(),
            "`other` is not a pass-through for vendor text"
        );
    }

    /// The six executor failure classes are the whole vocabulary, and the classes a vendor would
    /// reach for are deliberately missing.
    ///
    /// * rate-limit / unavailable → the host retries and reports `transport_exhausted` when spent;
    /// * cancellation → `HostControl::Cancel`, never an effect failure;
    /// * context overflow → a *semantic* provider outcome, not a failure.
    #[test]
    fn no_vendor_failure_vocabulary_is_expressible_on_the_canonical_face() {
        assert_eq!(HostEffectFailureKind::ALL.len(), 6);
        let canonical: BTreeSet<&str> = HostEffectFailureKind::ALL
            .iter()
            .map(|kind| kind.as_str())
            .collect();
        assert_eq!(
            canonical,
            BTreeSet::from([
                "transport_exhausted",
                "protocol_error",
                "storage_unavailable",
                "permission_denied",
                "resource_exhausted",
                "unknown",
            ])
        );

        for absent in [
            "rate_limited",
            "rate_limit_exceeded",
            "too_many_requests",
            "overloaded",
            "service_unavailable",
            "server_error",
            "timeout",
            "context_length_exceeded",
            "context_overflow",
            "cancelled",
            "canceled",
            "aborted",
            "user_interrupt",
            "interrupted",
        ] {
            assert!(
                serde_json::from_value::<HostEffectFailureKind>(json!(absent)).is_err(),
                "{absent:?} must not be an executor failure class"
            );
            assert!(
                !canonical.contains(absent),
                "{absent:?} leaked into the canonical vocabulary"
            );
        }

        // A retry ladder that ran and lost reaches the kernel as exactly one code.
        let spent: EffectOutcome = serde_json::from_value(json!({
            "status": "failed",
            "failure": {
                "kind": "transport_exhausted",
                "message": "5 attempts over 41s",
                "retryable": false,
            },
        }))
        .unwrap();
        let EffectOutcome::Failed(failed) = spent else {
            panic!("a spent ladder is a failure");
        };
        assert_eq!(
            failed.failure.kind,
            HostEffectFailureKind::TransportExhausted
        );
    }

    /// `retryable` is advice with the same (null) meaning on all six kinds, and DEC-5 is what makes
    /// that safe: the kernel's decision comes from the effect kind it published, so the flag has no
    /// branch to select. Encoding-level half of the claim; the behavioural differential lives in
    /// `driver.rs`.
    #[test]
    fn retryable_is_uniformly_optional_advice_across_all_six_failure_kinds() {
        for kind in HostEffectFailureKind::ALL {
            for retryable in [None, Some(true), Some(false)] {
                let mut failure = json!({ "kind": kind.as_str(), "message": "boom" });
                if let Some(flag) = retryable {
                    failure
                        .as_object_mut()
                        .unwrap()
                        .insert("retryable".to_string(), json!(flag));
                }
                let outcome: EffectOutcome =
                    serde_json::from_value(json!({ "status": "failed", "failure": failure }))
                        .unwrap();
                let EffectOutcome::Failed(failed) = outcome else {
                    panic!("failure decoded as success");
                };
                assert_eq!(failed.failure.kind, kind);
                assert_eq!(failed.failure.retryable, retryable);
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // §7.10 · tool result disposition (adjudication §5m item 2)
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_tool_result_must_state_whether_the_batch_can_continue() {
        // required: no serde default, so "the host did not say" is not a decodable state
        assert_eq!(
            decode_effect(json!({
                "status": "succeeded",
                "result": { "kind": "tools", "results": [
                    { "kind": "inline", "call_id": "c-1", "result": { "output": "ok" } }] },
            }))
            .unwrap_err()
            .kind,
            WireRejectionKind::MissingField
        );

        // exactly two values
        assert_eq!(
            ToolResultDisposition::ALL
                .iter()
                .map(|d| d.as_str())
                .collect::<BTreeSet<&str>>(),
            BTreeSet::from(["recoverable", "fatal"])
        );
        for value in ["recoverable", "fatal"] {
            let decoded: ToolResultDisposition = serde_json::from_value(json!(value)).expect(value);
            assert_eq!(decoded.as_str(), value);
            assert_eq!(decoded.is_fatal(), value == "fatal");
        }

        // the legacy taxonomy's other four rungs are gone. `user_interrupt` in particular:
        // cancellation is `HostControl::Cancel` and nothing else (§7.9), so the rollback rung it
        // used to trigger dies with the legacy wire.
        for gone in [
            "user_interrupt",
            "governance_denied",
            "provider_failure",
            "timeout",
            "cancelled",
        ] {
            assert!(
                serde_json::from_value::<ToolResultDisposition>(json!(gone)).is_err(),
                "{gone:?} is not a tool-result disposition"
            );
        }
    }

    /// §7.10 rule 9 · failure is orthogonal to residency.
    ///
    /// Both arms carry the same two failure facts and read them the same way. Before this, an
    /// externalised failure was indistinguishable from an externalised success — which made "did
    /// this call fail?" depend on how big its output happened to be.
    #[test]
    fn both_residency_arms_state_the_same_two_failure_facts() {
        let inline = ToolResultPayload::Inline(InlineToolResult {
            call_id: call_id("call-1"),
            result: ToolResult {
                output: "boom".to_string(),
                is_error: true,
                disposition: ToolResultDisposition::Fatal,
                tokens: None,
            },
        });
        let external = ToolResultPayload::External(ExternalToolResult {
            call_id: call_id("call-2"),
            payload_ref: payload_ref(),
            digest: digest(),
            original_size: WireU64::new(1_048_576),
            preview: "Traceback (most recent call last):".to_string(),
            is_error: true,
            disposition: ToolResultDisposition::Fatal,
        });
        for payload in [&inline, &external] {
            assert!(payload.is_error(), "{payload:?}");
            assert_eq!(payload.disposition(), ToolResultDisposition::Fatal);
            assert!(payload.disposition().is_fatal());
        }

        // `disposition` is mandatory on the external arm too — same argument as inline: the
        // dangerous value must not be the silent one.
        assert_eq!(
            decode_effect(json!({
                "status": "succeeded",
                "result": { "kind": "tools", "results": [
                    { "kind": "external", "call_id": "c-1", "payload_ref": "p-1",
                      "digest": "sha256:ab", "original_size": "1", "preview": "x" }] },
            }))
            .unwrap_err()
            .kind,
            WireRejectionKind::MissingField
        );

        // `is_error` is not: absent means "the call succeeded", the same default the inline arm
        // has, and it stays off the wire when false.
        let ok: EffectOutcome = serde_json::from_value(json!({
            "status": "succeeded",
            "result": { "kind": "tools", "results": [
                { "kind": "external", "call_id": "c-1", "payload_ref": "p-1",
                  "digest": "sha256:ab", "original_size": "1", "preview": "x",
                  "disposition": "recoverable" }] },
        }))
        .unwrap();
        let EffectOutcome::Succeeded(success) = &ok else {
            panic!("not a success");
        };
        let EffectSuccess::Tools(tools) = &success.result else {
            panic!("not a tools success");
        };
        assert!(!tools.results[0].is_error());
        let value = serde_json::to_value(&tools.results[0]).unwrap();
        assert!(value.get("is_error").is_none(), "false stays off the wire");
        assert_eq!(value["disposition"], json!("recoverable"));
    }

    /// §7.8 · a milestone request is the host's lookup key and nothing else.
    #[test]
    fn a_milestone_request_is_exactly_the_contract_and_phase_pair() {
        let request = MilestoneRequest {
            contract_id: "brief-quality-v1".to_string(),
            phase_id: "collect".to_string(),
        };
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(
            value.as_object().unwrap().keys().collect::<Vec<_>>(),
            vec!["contract_id", "phase_id"],
            "a phase id is unique only inside its contract, so the pair is the whole key"
        );

        // criteria, evidence and the verifier are host-owned (§5.2). Under the §7.3 skeleton core
        // never learns them, so a field for them could only ever be empty — and an always-empty
        // field is an invitation to believe the kernel knows something it does not.
        for host_owned in ["criteria", "required_evidence", "verifier", "evidence"] {
            let mut with_extra = value.clone();
            with_extra
                .as_object_mut()
                .unwrap()
                .insert(host_owned.to_string(), json!([]));
            assert!(
                serde_json::from_value::<MilestoneRequest>(with_extra).is_err(),
                "{host_owned} must not decode"
            );
        }
        for missing in ["contract_id", "phase_id"] {
            let mut without = value.clone();
            without.as_object_mut().unwrap().remove(missing);
            assert!(
                serde_json::from_value::<MilestoneRequest>(without).is_err(),
                "{missing} is half the key and cannot be omitted"
            );
        }
    }

    // -----------------------------------------------------------------------------------------
    // DEC-2 · no outcome payload carries a host clock
    // -----------------------------------------------------------------------------------------

    #[test]
    fn no_effect_outcome_payload_carries_a_host_wall_clock() {
        const BANNED: [&str; 8] = [
            "now_ms",
            "observed_at_ms",
            "timestamp",
            "timestamp_ms",
            "started_at_ms",
            "completed_at_ms",
            "wall_clock_ms",
            "received_at_ms",
        ];

        let mut outcomes: Vec<EffectOutcome> = success_samples()
            .into_iter()
            .map(|result| EffectOutcome::Succeeded(EffectSucceeded { result }))
            .collect();
        outcomes.push(EffectOutcome::Failed(EffectFailed {
            failure: HostEffectFailure {
                kind: HostEffectFailureKind::TransportExhausted,
                message: "socket hang up".to_string(),
                retryable: Some(false),
            },
        }));

        for outcome in outcomes {
            let value = serde_json::to_value(&outcome).unwrap();
            let mut all = BTreeSet::new();
            keys(&value, &mut all);
            for banned in BANNED {
                assert!(
                    !all.contains(banned),
                    "outcome payload must not carry {banned:?}: {value}"
                );
            }
        }
    }

    // -----------------------------------------------------------------------------------------
    // §7.10 · inline / external payload
    // -----------------------------------------------------------------------------------------

    #[test]
    fn an_external_tool_result_is_a_handle_with_a_digest_size_and_preview() {
        let external = ToolResultPayload::External(ExternalToolResult {
            call_id: call_id("call-2"),
            payload_ref: payload_ref(),
            digest: digest(),
            original_size: WireU64::new(1_048_576),
            preview: "total 42".to_string(),
            is_error: false,
            disposition: ToolResultDisposition::Recoverable,
        });
        let value = serde_json::to_value(&external).unwrap();
        assert_eq!(value["kind"], json!("external"));
        for required in ["payload_ref", "digest", "original_size", "preview"] {
            assert!(value.get(required).is_some(), "external needs {required}");
        }
        assert_eq!(
            value["original_size"],
            json!("1048576"),
            "sizes are decimal-string u64, not JS numbers"
        );

        // rule 7: a payload ref is an opaque locator, never interpreted as a file path
        let mut all = BTreeSet::new();
        keys(&value, &mut all);
        for banned in ["path", "file_path", "spool_ref", "spool_dir", "archive_ref"] {
            assert!(!all.contains(banned), "payload ref must stay opaque");
        }
    }

    #[test]
    fn payload_residency_distinguishes_generated_over_limit_from_pressure_archival() {
        let residencies = [
            PayloadResidency::Resident(ResidentPayload {}),
            PayloadResidency::External(ExternalResidency {
                payload_ref: payload_ref(),
                digest: digest(),
                original_size: WireU64::new(1_048_576),
            }),
            PayloadResidency::PagedOut(PagedOutResidency {
                payload_ref: payload_ref(),
                digest: digest(),
            }),
            PayloadResidency::Collapsed(CollapsedPayload {}),
        ];
        let tags: BTreeSet<String> = residencies
            .iter()
            .map(|residency| {
                serde_json::to_value(residency).unwrap()["kind"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(
            tags,
            BTreeSet::from([
                "resident".to_string(),
                "external".to_string(),
                "paged_out".to_string(),
                "collapsed".to_string(),
            ]),
            "§7.10 keeps `external` (generated over limit) apart from `paged_out` (pressure)"
        );

        // `paged_out` carries no original_size — that is the External-only fact
        let paged = serde_json::to_value(&residencies[2]).unwrap();
        assert!(paged.get("original_size").is_none());
    }

    // -----------------------------------------------------------------------------------------
    // strictness
    // -----------------------------------------------------------------------------------------

    #[test]
    fn unknown_success_kinds_fields_and_variants_are_rejected() {
        // unknown success kind
        assert_eq!(
            decode_effect(json!({ "status": "succeeded", "result": { "kind": "spooled" } }))
                .unwrap_err()
                .kind,
            WireRejectionKind::UnknownVariant
        );
        // unknown failure kind
        assert_eq!(
            decode_effect(json!({ "status": "failed", "failure": { "kind": "rate_limited" } }))
                .unwrap_err()
                .kind,
            WireRejectionKind::UnknownVariant
        );
        // unknown field on the outcome itself
        assert_eq!(
            decode_effect(json!({
                "status": "succeeded",
                "result": { "kind": "approval" },
                "now_ms": 1,
            }))
            .unwrap_err()
            .kind,
            WireRejectionKind::UnknownField
        );
        // unknown field inside a success payload
        assert_eq!(
            decode_effect(json!({
                "status": "succeeded",
                "result": { "kind": "payload_loaded", "handle_id": "h-1",
                            "payload": { "content": "x", "digest": "sha256:ab",
                                         "original_size": "1", "path": "/tmp/x" } },
            }))
            .unwrap_err()
            .kind,
            WireRejectionKind::UnknownField
        );
        // missing required field
        assert_eq!(
            decode_effect(json!({
                "status": "succeeded",
                "result": { "kind": "tools", "results": [
                    { "kind": "external", "call_id": "c-1", "payload_ref": "p-1",
                      "original_size": "1", "preview": "x" }] },
            }))
            .unwrap_err()
            .kind,
            WireRejectionKind::MissingField
        );
        // scalar rule inside a success payload
        assert_eq!(
            decode_effect(json!({
                "status": "succeeded",
                "result": { "kind": "payload_loaded", "handle_id": "h-1",
                            "payload": { "content": "x", "digest": "sha256:ab",
                                         "original_size": 1 } },
            }))
            .unwrap_err()
            .kind,
            WireRejectionKind::InvalidScalar
        );
    }

    #[test]
    fn every_effect_and_success_payload_denies_unknown_fields() {
        for effect in effect_samples() {
            let mut value = serde_json::to_value(&effect).unwrap();
            value
                .as_object_mut()
                .unwrap()
                .insert("host_hint".to_string(), json!("x"));
            assert!(
                serde_json::from_value::<EffectKind>(value).is_err(),
                "{:?} must deny unknown fields",
                effect.tag()
            );
        }
        for success in success_samples() {
            let mut value = serde_json::to_value(&success).unwrap();
            value
                .as_object_mut()
                .unwrap()
                .insert("host_hint".to_string(), json!("x"));
            assert!(
                serde_json::from_value::<EffectSuccess>(value).is_err(),
                "{:?} must deny unknown fields",
                success.tag()
            );
        }
    }

    // -----------------------------------------------------------------------------------------
    // goldens
    // -----------------------------------------------------------------------------------------

    #[test]
    fn resolve_effect_goldens_cover_every_success_kind_and_the_failure_path() {
        let mut success_kinds: BTreeSet<String> = BTreeSet::new();
        let mut failure_kinds: BTreeSet<String> = BTreeSet::new();

        for (name, fixture) in fixtures_with_prefix("input_resolve_") {
            let text = serde_json::to_string(&fixture).unwrap();
            let envelope = decode_envelope_json(&text, &KernelBootstrapLimits::default())
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(
                serde_json::to_value(&envelope).unwrap(),
                fixture,
                "{name}: round-trip changed the document"
            );

            let KernelInput::ResolveEffect(resolve) = &envelope.input else {
                panic!("{name} is not a resolve_effect golden");
            };
            match &resolve.outcome {
                EffectOutcome::Succeeded(ok) => {
                    success_kinds.insert(ok.result.tag().as_str().to_string());
                }
                EffectOutcome::Failed(failed) => {
                    failure_kinds.insert(failed.failure.kind.as_str().to_string());
                }
            }
        }

        assert_eq!(
            success_kinds,
            EffectSuccessTag::ALL
                .iter()
                .map(|tag| tag.as_str().to_string())
                .collect::<BTreeSet<String>>(),
            "every effect kind needs at least one resolve golden"
        );
        assert!(
            !failure_kinds.is_empty(),
            "the unified failure path needs a golden too"
        );
    }

    #[test]
    fn the_milestone_effect_has_a_failure_channel_like_every_other_effect() {
        // B7: `MilestoneResult` historically had no error field at all.
        let golden = fixtures_with_prefix("input_resolve_effect_milestone_failed");
        assert_eq!(golden.len(), 1);
        let text = serde_json::to_string(&golden[0].1).unwrap();
        let envelope = decode_envelope_json(&text, &KernelBootstrapLimits::default()).unwrap();
        let KernelInput::ResolveEffect(resolve) = &envelope.input else {
            panic!("not a resolve_effect golden");
        };
        let EffectOutcome::Failed(failed) = &resolve.outcome else {
            panic!("milestone failure golden must be a Failed outcome");
        };
        assert_eq!(
            failed.failure.kind,
            HostEffectFailureKind::StorageUnavailable
        );

        let milestone = kernel_effect(EffectKind::EvaluateMilestone(EvaluateMilestoneEffect {
            request: MilestoneRequest {
                contract_id: "brief-quality-v1".to_string(),
                phase_id: "phase-2".to_string(),
            },
        }));
        assert!(milestone.accept_outcome(&resolve.outcome).is_ok());
    }

    #[test]
    fn effect_rejection_goldens_cover_the_payload_failure_modes() {
        let mut expected: BTreeSet<String> = BTreeSet::new();
        for (name, fixture) in fixtures_with_prefix("reject_effect_") {
            let kind = fixture["expect"]
                .as_str()
                .unwrap_or_else(|| panic!("{name}: missing `expect`"));
            let text = serde_json::to_string(&fixture["envelope"]).unwrap();
            let rejection = decode_envelope_json(&text, &KernelBootstrapLimits::default())
                .map(|ok| panic!("{name}: expected rejection, decoded {ok:?}"))
                .unwrap_err();
            assert_eq!(
                rejection.kind.as_str(),
                kind,
                "{name}: {}",
                rejection.message
            );
            expected.insert(kind.to_string());
        }
        for required in [
            "unknown_field",
            "unknown_variant",
            "missing_field",
            "invalid_scalar",
        ] {
            assert!(
                expected.contains(required),
                "effect rejection goldens must cover {required}"
            );
        }
    }
}
