//! Operation configuration (spec §7.3, §13.1, §13.3).
//!
//! One input class carries configuration, it is the genesis record, and it decodes inside the
//! absolute bootstrap boundary. This module fixes *what* travels in it.
//!
//! Three rules shape the whole field tree:
//!
//! 1. **Boot-only by construction.** Every knob here is admissible exactly once, through
//!    `ConfigureOperation`. The live surface is the closed [`LivePolicyPatch`](super::command::LivePolicyPatch)
//!    union; a knob that appears in both would re-create the historical situation where
//!    `ConfigureRun.governance` and `LoadGovernancePolicy` shared one implementation and therefore
//!    made the boot/live distinction unenforceable (§13.1 现状注记).
//! 2. **No implicit defaults survive the boundary.** [`OperationConfig`] is the *sparse* host
//!    input; [`resolve_operation_config`] normalises it into a dense [`ResolvedOperationConfig`],
//!    and that is what the genesis record stores. A replay therefore never re-applies a newer
//!    binary's defaults (§7.3).
//! 3. **Validation is atomic.** One illegal field rejects the whole `ConfigureOperation` and
//!    changes nothing: `resolve_operation_config` returns `Result` and owns no state, so a partial
//!    application is not expressible.
//!
//! Host concerns explicitly absent (§7.3 配置边界 table, §13.3): host effect retry/backoff, spool /
//! blob directories, provider endpoint / key / protocol, checkpoint storage location, and
//! `memory_path`. They are host executor and store configuration; a kernel that accepted them
//! would be persisting facts it cannot reproduce.

use serde::{Deserialize, Serialize};

use super::command::{
    GovernancePolicy, PolicyAction, RecoveryPolicy, SignalPolicy, TailBoundsPolicy,
};
use super::effect::{EffectKindTag, MemoryAccessBinding, ToolSchema};
use super::envelope::{WireRejection, WireRejectionKind};
use super::fault::{KernelFault, KernelFaultCode};
use super::scalar::{Ppm, WireU64};
use super::{KERNEL_ABI_VERSION, KernelBootstrapLimits};

// ---------------------------------------------------------------------------------------------
// rejection helpers
// ---------------------------------------------------------------------------------------------

/// A configuration value that decoded cleanly but breaks a contract rule.
///
/// [`WireRejectionKind::PolicyViolation`], never `InvalidScalar`: the host stated a coherent
/// value and the kernel refuses to adopt it. Telling those two apart is what lets a host
/// distinguish "fix your encoder" from "fix your configuration".
fn invalid(message: impl Into<String>) -> WireRejection {
    WireRejection::new(WireRejectionKind::PolicyViolation, message)
}

fn too_many(message: impl Into<String>) -> WireRejection {
    WireRejection::new(WireRejectionKind::CollectionTooLarge, message)
}

fn require_le_u32(
    label: &str,
    requested: u32,
    ceiling: u32,
    ceiling_label: &str,
) -> Result<(), WireRejection> {
    if requested > ceiling {
        return Err(invalid(format!(
            "{label} {requested} is wider than {ceiling_label} {ceiling}; \
             operation configuration may only tighten it"
        )));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// OperationConfig — the sparse wire input
// ---------------------------------------------------------------------------------------------

/// Boot configuration for one operation (§7.3).
///
/// Sparse on purpose: a host states what it wants to differ from the kernel's compile-time
/// defaults. `host_effect_support` is the one **mandatory** field — DEC-8 makes it an explicit
/// declaration, and a default would be exactly the implicit assumption it exists to remove.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationConfig {
    /// Turn/token/wall budgets plus the loop guards (criteria gate, repeat fuse, entropy watch).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_policy: Option<ExecutionPolicy>,
    /// Initial syscall-gate posture. Live changes go through
    /// [`LivePolicyPatch::ReplaceGovernancePolicy`](super::command::LivePolicyPatch).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub governance_policy: Option<GovernancePolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduler_policy: Option<SchedulerPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_quota: Option<ResourceQuota>,
    /// RunGroup admission result. Absent ⇒ the operation is not reservation-backed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_grant: Option<BudgetGrant>,
    /// Initial signal routing. Live changes go through
    /// [`LivePolicyPatch::ReplaceSignalPolicy`](super::command::LivePolicyPatch).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal_policy: Option<SignalPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_policy: Option<ContextPolicy>,
    /// Semantic recovery ladders the kernel owns. Host transport retry/backoff is **not** here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery_policy: Option<RecoveryPolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_policy: Option<PayloadPolicy>,
    /// May only **tighten** [`KernelBootstrapLimits`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel_limits: Option<KernelLimits>,
    /// Opaque memory access binding. Absent ⇒ the operation has no memory plane at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_access: Option<MemoryAccessBinding>,
    /// Validation / recall / promotion thresholds for the memory plane. Never a path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_policy: Option<MemoryPolicy>,
    /// Initial tool catalog (§13.3 · `SetTools`). Live narrowing/widening is a capability patch,
    /// not a second catalog install.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_catalog: Vec<ToolSchema>,
    /// Initial skill catalog (§13.3 · `SetAvailableSkills`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skill_catalog: Vec<SkillMetadata>,
    /// The verification contracts this operation may evaluate.
    ///
    /// A **skeleton**, not a specification: an ordered phase list and, per phase, the capabilities
    /// passing it unlocks. See [`VerificationContract`] for why that is the exact line.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verification_contracts: Vec<VerificationContract>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feature_policy: Option<FeaturePolicy>,
    /// DEC-8. Mandatory: the kernel emits an effect only for a kind the host declared it can
    /// execute, and fail-closes on the rest.
    pub host_effect_support: HostEffectSupport,
}

impl OperationConfig {
    /// Normalise and validate. See [`resolve_operation_config`].
    pub fn resolve(
        &self,
        defaults: &ConfigDefaults,
    ) -> Result<ResolvedOperationConfig, WireRejection> {
        resolve_operation_config(self, defaults)
    }
}

// ---------------------------------------------------------------------------------------------
// execution policy
// ---------------------------------------------------------------------------------------------

/// Budgets and loop guards (§13.3 · `SetCriteriaGate`, `SetRepeatFuse`, `SetEntropyWatch`, and the
/// `SchedulerBudget` the constructor may no longer accept, §13.1).
///
/// `SetSchedulerBudget` has no counterpart in the live union: the only axis it ever carried was
/// `max_wall_ms`, and the live form of that is
/// [`HostCommand::UpdateDeadline`](super::command::HostCommand).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionPolicy {
    /// Context window size the pressure monitor works against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_total_tokens: Option<WireU64>,
    /// Absolute wall budget. Absent ⇒ no wall-clock limit; that is a value, not a default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_wall_ms: Option<WireU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub criteria_gate_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat_fuse: Option<RepeatFusePolicy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entropy_watch: Option<EntropyWatchPolicy>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepeatFusePolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Consecutive identical calls before the call is denied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deny_after: Option<u32>,
    /// Consecutive identical calls before the operation terminates with `no_progress`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate_after: Option<u32>,
}

/// Entropy watch. The three historical `f64` knobs are fixed-point ppm (§7.1.1, §13.3): a
/// threshold that differs by one ULP between languages is a different kernel decision.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntropyWatchPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold_ppm: Option<Ppm>,
    /// Re-arm only once the score falls below `threshold - hysteresis` (anti-flap).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hysteresis_ppm: Option<Ppm>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify_model: Option<bool>,
}

// ---------------------------------------------------------------------------------------------
// scheduler policy
// ---------------------------------------------------------------------------------------------

/// Ready-queue ordering weights. No `version` field: §16.1 leaves exactly two revision markers on
/// the wire (`abi_version`, `checkpoint_version`), and a per-policy version is the third one
/// DEC-6 removed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SchedulerPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub critical_path_weight: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fanout_weight: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub age_weight: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_cost_weight: Option<u32>,
}

/// Upper bound of any scheduler weight, matching the legacy validator.
pub const MAX_SCHEDULER_WEIGHT: u32 = 1_000_000_000;

// ---------------------------------------------------------------------------------------------
// resource quota / budget grant
// ---------------------------------------------------------------------------------------------

/// Declarative caps enforced at the syscall trap. Absent axis ⇒ uncapped, which is a value.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceQuota {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent_subagents: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_total_subagents: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_spawn_depth: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_workflow_nodes: Option<u32>,
    /// Rolling-window memory-write rate limit. A named struct, not the legacy `(u32, u64)` tuple:
    /// a positional pair has no field names to reject unknowns against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_writes_per_window: Option<RateWindow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateWindow {
    pub max_events: u32,
    pub window_ms: WireU64,
}

/// RunGroup admission result. `reservation_id` is opaque — the kernel enforces the grant locally
/// and reports terminal usage against the same identity; it never interprets it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetGrant {
    pub reservation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<WireU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagents: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rounds: Option<u32>,
}

// ---------------------------------------------------------------------------------------------
// context policy
// ---------------------------------------------------------------------------------------------

/// Stable, replayable context behaviour. Every ratio is fixed-point ppm.
///
/// Two §13.3 rows land here: `SetKnowledgeBudget` (whose `f64` ratio becomes
/// [`Self::knowledge_budget_ppm`]) and the retired split prompt-budget input (the U2 field that had no live
/// setter and no §7.3 home).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pressure_thresholds_ppm: Option<PressureThresholds>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_after_compress_ppm: Option<Ppm>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preserve_recent_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renewal_carryover_ppm: Option<Ppm>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collapse_old_assistant_narration: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_micro_compact_minutes: Option<u32>,
    /// Share of the context budget the knowledge partition may hold. `0` disables the partition's
    /// budget entirely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge_budget_ppm: Option<Ppm>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_budget: Option<PromptBudget>,
}

/// The five pressure thresholds. Replaced as one value: a partial threshold ladder is how the
/// strictly-increasing invariant gets violated one field at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PressureThresholds {
    pub snip: Ppm,
    pub micro: Ppm,
    pub collapse: Ppm,
    pub auto: Ppm,
    pub renewal: Ppm,
}

/// Host-counted request overhead and hard reserves, deducted before the kernel renders any
/// content.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptBudget {
    pub prompt_overhead_tokens: u32,
    pub output_reserve_tokens: u32,
    pub safety_margin_tokens: u32,
}

impl PromptBudget {
    pub fn reserved_tokens(self) -> u32 {
        self.prompt_overhead_tokens
            .saturating_add(self.output_reserve_tokens)
            .saturating_add(self.safety_margin_tokens)
    }
}

// ---------------------------------------------------------------------------------------------
// payload policy
// ---------------------------------------------------------------------------------------------

/// Where the kernel draws the inline/external line for a tool result (§7.10).
///
/// The two knobs are the ones §13.3 keeps from the legacy `spool_threshold_bytes` /
/// `spool_preview_bytes` pair. They are renamed to §7.10's vocabulary: `SpoolLargeResult` is
/// deleted, so a canonical field called `spool_*` would be a legacy alias, and Checkpoint B
/// forbids those. There is deliberately **no** directory/root field — a `PayloadRef` is an opaque
/// locator, never a path (§7.10 rule 7).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PayloadPolicy {
    /// Results at or above this size are committed as `External` rather than inline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_threshold_bytes: Option<u32>,
    /// Bytes of preview the kernel keeps resident for an external payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_bytes: Option<u32>,
}

// ---------------------------------------------------------------------------------------------
// kernel limits
// ---------------------------------------------------------------------------------------------

/// Operation-scoped structural limits. These may only **tighten** [`KernelBootstrapLimits`];
/// widening any axis rejects the whole `ConfigureOperation`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KernelLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_input_bytes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_json_depth: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_collection_entries: Option<u32>,
    /// Per-collection bounds. `absolute_max_collection_entries` is one number for every container
    /// in the document, which is the wrong granularity for a tool catalog and a knowledge
    /// partition at the same time; each named bound may only tighten it further.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection_limits: Option<CollectionLimits>,
}

/// Named per-collection entry bounds. Absent ⇒ that collection inherits `max_collection_entries`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CollectionLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_catalog: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skill_catalog: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge_entries: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_messages: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability_grants: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub governance_rules: Option<u32>,
}

// ---------------------------------------------------------------------------------------------
// memory
// ---------------------------------------------------------------------------------------------

/// Validation / recall / promotion thresholds (§13.3 · `SetMemoryPolicy`).
///
/// `memory_path` is **not** here and has no replacement: it moved to the host `MemoryStore`
/// config (§14.4). The kernel performs no recall I/O, so a path in a kernel record is a fact the
/// kernel can neither verify nor reproduce.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale_warning_days: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval_top_k: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_content_bytes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_name_length: Option<u32>,
    /// Recall count at which a record becomes a promotion candidate. Absent ⇒ no suggestion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promotion_recall_threshold: Option<WireU64>,
}

// ---------------------------------------------------------------------------------------------
// catalogs
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillMetadata {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when_to_use: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_tools: Vec<String>,
    /// Fine-grained authority made effective while this skill is active. The mounting agent's
    /// authority is only known at start time, so attenuation is checked by each activation path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capability_grants: Vec<crate::types::capability::Capability>,
    /// Effort level 1–5; scales the per-skill token budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimated_tokens: Option<u32>,
}

/// One verification contract: an ordered cascade of phases the operation must pass in sequence.
///
/// **A skeleton, deliberately.** The kernel owns exactly two things about a contract — the order
/// its phases run in, and what passing a phase unlocks — because both are kernel decisions: the
/// order decides which `EvaluateMilestone` is published next, and the unlocks mutate the capability
/// table, which is the operation's authority surface. Everything else about a contract — the
/// acceptance criteria, the evidence, the verifier and the I/O that runs it — stays host-side
/// (§5.2, adjudication §5m item 3). A bare `Vec<String>` of ids could not express the first two,
/// so the phase cascade had no canonical producer at all and `EvaluateMilestone` was unreachable
/// from the wire (Task 12 SPEC-ISSUE-4); a full contract type would have moved criteria ownership
/// into core. This is the line between them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationContract {
    /// Logical id. Unique within the operation, and the value
    /// [`LogicalAgentSpec::verification_contract_id`](super::root::LogicalAgentSpec) resolves
    /// against.
    pub contract_id: String,
    pub phases: Vec<MilestonePhase>,
}

/// One phase of a [`VerificationContract`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MilestonePhase {
    /// Stable id, unique within its contract. It is what `EvaluateMilestone` names and what the
    /// host keys its verifier lookup on.
    pub phase_id: String,
    /// Capability ids mounted when this phase passes. Each must name an entry in the operation's
    /// capability directory — the declared `tool_catalog` and `skill_catalog` — so a phase cannot
    /// unlock a surface the operation never declared. Fail-closed at resolve time, for the same
    /// reason `feature_policy.stable_core_tool_ids` is: discovering it as a mid-run capability
    /// mutation with nothing behind it is strictly worse than discovering it at
    /// `ConfigureOperation`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unlocks: Vec<String>,
}

// ---------------------------------------------------------------------------------------------
// feature policy
// ---------------------------------------------------------------------------------------------

/// The feature switches §13.3 folds together: `SetMemoryEnabled`, `SetKnowledgeEnabled`,
/// `SetPlanToolEnabled`, `SetStableCoreTools` and the tool dispatch gate (§13.1's one genuinely
/// boot-only item in this group).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeaturePolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_tool_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_dispatch_gate: Option<ToolDispatchGate>,
    /// Tool ids always exposed under skill gating — the exposure baseline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stable_core_tool_ids: Option<Vec<String>>,
}

/// Fail-closed dispatch (§13.1). `Exposed` executes only tools this operation actually advertised
/// to the model; `Registered` is the permissive escape hatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolDispatchGate {
    #[default]
    Exposed,
    Registered,
}

// ---------------------------------------------------------------------------------------------
// host effect support (DEC-8)
// ---------------------------------------------------------------------------------------------

/// The host's explicit declaration of which effect kinds it can execute (§7.3, DEC-8).
///
/// The kernel emits an effect only for a declared kind and fail-closes on the rest. Without this,
/// the same effect produces a different outcome per language — Rust's `spawn_workflow` always
/// fails, Python handles memory effects only inside a host syscall, and the Node/WASM `if/else-if`
/// chains busy-wait on anything unhandled.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostEffectSupport {
    /// Not `skip_serializing_if`: an empty declaration ("I can execute nothing") must be visible
    /// on the wire rather than indistinguishable from an omitted field.
    pub supported: Vec<EffectKindTag>,
}

impl HostEffectSupport {
    pub fn new(supported: impl IntoIterator<Item = EffectKindTag>) -> Self {
        Self {
            supported: supported.into_iter().collect(),
        }
    }

    pub fn supports(&self, kind: EffectKindTag) -> bool {
        self.supported.contains(&kind)
    }
}

// ---------------------------------------------------------------------------------------------
// ResolvedOperationConfig — what the genesis record stores
// ---------------------------------------------------------------------------------------------

/// The dense, defaults-free configuration written into the genesis record (§7.3).
///
/// Every `Option` that stood for "use whatever this binary defaults to" is gone. The `Option`s
/// that remain — `max_wall_ms`, the quota axes, `budget_grant`, `memory_access` — encode a real
/// value ("no wall limit", "uncapped", "not reservation-backed", "no memory plane"), which is why
/// a replay of this record on a newer binary produces the same decisions it did on the first run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedOperationConfig {
    /// The revision this configuration was resolved under. A resolved config is a durable record,
    /// so it states which contract produced it rather than relying on the reader's constant.
    pub abi_version: u32,
    pub execution_policy: ResolvedExecutionPolicy,
    pub governance_policy: ResolvedGovernancePolicy,
    pub scheduler_policy: ResolvedSchedulerPolicy,
    pub resource_quota: ResourceQuota,
    pub budget_grant: Option<BudgetGrant>,
    pub signal_policy: ResolvedSignalPolicy,
    pub context_policy: ResolvedContextPolicy,
    pub recovery_policy: ResolvedRecoveryPolicy,
    pub payload_policy: ResolvedPayloadPolicy,
    pub kernel_limits: ResolvedKernelLimits,
    pub memory_access: Option<MemoryAccessBinding>,
    pub memory_policy: ResolvedMemoryPolicy,
    pub tool_catalog: Vec<ToolSchema>,
    pub skill_catalog: Vec<SkillMetadata>,
    pub verification_contracts: Vec<VerificationContract>,
    pub feature_policy: ResolvedFeaturePolicy,
    pub host_effect_support: HostEffectSupport,
}

impl ResolvedOperationConfig {
    /// The contract this id names, or `None` when the catalog does not declare it.
    pub fn verification_contract(&self, contract_id: &str) -> Option<&VerificationContract> {
        self.verification_contracts
            .iter()
            .find(|contract| contract.contract_id == contract_id)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedExecutionPolicy {
    pub max_context_tokens: u32,
    pub max_turns: u32,
    pub max_total_tokens: WireU64,
    pub max_wall_ms: Option<WireU64>,
    pub criteria_gate_enabled: bool,
    pub repeat_fuse: ResolvedRepeatFuse,
    pub entropy_watch: ResolvedEntropyWatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedRepeatFuse {
    pub enabled: bool,
    pub deny_after: u32,
    pub terminate_after: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedEntropyWatch {
    pub enabled: bool,
    pub threshold_ppm: Ppm,
    pub hysteresis_ppm: Ppm,
    pub cooldown_turns: u32,
    pub notify_model: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedGovernancePolicy {
    pub default_action: PolicyAction,
    pub rules: Vec<super::command::PolicyRule>,
    pub vetoed_tools: Vec<String>,
    pub rate_limits: Vec<super::command::RateLimitSpec>,
    pub constraints: Vec<super::command::ParamConstraint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedSchedulerPolicy {
    pub critical_path_weight: u32,
    pub fanout_weight: u32,
    pub age_weight: u32,
    pub token_cost_weight: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedSignalPolicy {
    pub queue_max: u32,
    pub ttl_ms: Option<WireU64>,
    pub deadline_escalation: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedContextPolicy {
    pub pressure_thresholds_ppm: PressureThresholds,
    pub target_after_compress_ppm: Ppm,
    pub preserve_recent_turns: u32,
    pub renewal_carryover_ppm: Ppm,
    pub collapse_old_assistant_narration: bool,
    pub idle_micro_compact_minutes: u32,
    pub knowledge_budget_ppm: Ppm,
    pub prompt_budget: PromptBudget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedRecoveryPolicy {
    pub provider_recovery_attempts: u8,
    pub output_recovery_attempts: u8,
    /// §12.3 · the journal tail this operation may carry between checkpoints.
    ///
    /// It lives in the **resolved** configuration, which means the genesis record freezes it: an
    /// operation's tail bound cannot change under it because a later binary shipped a different
    /// default, and a rebuild re-derives exactly the bound the original run was refused against.
    /// The transaction adopts this value the moment the genesis record installs the configuration
    /// (until then it runs on the bootstrap baseline, which is what bounds the genesis append
    /// itself).
    pub tail_bounds: TailBounds,
}

/// Soft watermark and hard limit of the journal tail, on both axes §12.3 names: canonical input
/// count and bytes.
///
/// The hard limit is what makes [`KernelFaultCode::CheckpointRequired`] reachable, and reaching it
/// is a **retryable, zero-mutation rejection** — not the historical overflow latch, which
/// permanently disabled snapshots and all later staged transitions once it tripped.
///
/// Both axes exist because either one alone is escapable: a run of many tiny inputs blows the
/// record count long before the byte budget, and a single oversized payload blows the byte budget
/// on one record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TailBounds {
    pub soft_records: WireU64,
    pub hard_records: WireU64,
    pub soft_bytes: WireU64,
    pub hard_bytes: WireU64,
}

impl TailBounds {
    /// The documented default: ~500 records / 4 MiB before the host is asked to checkpoint, and
    /// four times that before the next input is refused with a retryable `CheckpointRequired`.
    pub const DEFAULT: Self = Self {
        soft_records: WireU64::new(512),
        hard_records: WireU64::new(2_048),
        soft_bytes: WireU64::new(4 * 1024 * 1024),
        hard_bytes: WireU64::new(16 * 1024 * 1024),
    };

    /// Fail closed on an incoherent bound rather than silently reordering it: a soft watermark
    /// above its hard limit would mean "warn after it is already too late".
    pub fn new(
        soft_records: u64,
        hard_records: u64,
        soft_bytes: u64,
        hard_bytes: u64,
    ) -> Result<Self, KernelFault> {
        let bounds = Self {
            soft_records: WireU64::new(soft_records),
            hard_records: WireU64::new(hard_records),
            soft_bytes: WireU64::new(soft_bytes),
            hard_bytes: WireU64::new(hard_bytes),
        };
        bounds
            .check()
            .map_err(|message| KernelFault::new(KernelFaultCode::InvalidConfig, message))?;
        Ok(bounds)
    }

    /// The one place the coherence rules live, so the configuration resolver and the direct
    /// constructor cannot drift apart.
    pub(super) fn check(&self) -> Result<(), String> {
        if self.soft_records > self.hard_records || self.soft_bytes > self.hard_bytes {
            return Err(format!(
                "recovery_policy.tail_bounds watermark ({} records / {} bytes) exceeds its hard \
                 limit ({} records / {} bytes)",
                self.soft_records, self.soft_bytes, self.hard_records, self.hard_bytes
            ));
        }
        if self.hard_records.get() == 0 || self.hard_bytes.get() == 0 {
            return Err(
                "recovery_policy.tail_bounds hard limit of zero admits no transaction at all"
                    .to_string(),
            );
        }
        Ok(())
    }
}

impl Default for TailBounds {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedPayloadPolicy {
    pub inline_threshold_bytes: u32,
    pub preview_bytes: u32,
}

/// Structural limits after resolution: the three absolute axes plus a **dense** per-collection
/// table. Every named bound is concrete here, so no later stage has to re-derive "which ceiling
/// applies to this container".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedKernelLimits {
    pub max_input_bytes: u32,
    pub max_json_depth: u16,
    pub max_collection_entries: u32,
    pub collection_limits: ResolvedCollectionLimits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedCollectionLimits {
    pub tool_catalog: u32,
    pub skill_catalog: u32,
    pub knowledge_entries: u32,
    pub initial_messages: u32,
    pub capability_grants: u32,
    pub governance_rules: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedMemoryPolicy {
    pub stale_warning_days: u32,
    pub retrieval_top_k: u32,
    pub validation_enabled: bool,
    pub max_content_bytes: u32,
    pub max_name_length: u32,
    pub promotion_recall_threshold: Option<WireU64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedFeaturePolicy {
    pub memory_enabled: bool,
    pub knowledge_enabled: bool,
    pub plan_tool_enabled: bool,
    pub tool_dispatch_gate: ToolDispatchGate,
    pub stable_core_tool_ids: Vec<String>,
}

// ---------------------------------------------------------------------------------------------
// defaults
// ---------------------------------------------------------------------------------------------

/// The kernel's compile-time baseline plus the bootstrap ceiling to resolve against.
///
/// This value is what "the current binary's defaults" means, made explicit. It is an input to
/// resolution and never a fallback afterwards: once the genesis record holds a
/// [`ResolvedOperationConfig`], changing these constants cannot change that operation's replay.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigDefaults {
    pub bootstrap_limits: KernelBootstrapLimits,
    pub baseline: ResolvedOperationConfig,
}

impl ConfigDefaults {
    pub fn new(bootstrap_limits: KernelBootstrapLimits) -> Self {
        let entries = bootstrap_limits.absolute_max_collection_entries;
        Self {
            bootstrap_limits,
            baseline: ResolvedOperationConfig {
                abi_version: KERNEL_ABI_VERSION,
                execution_policy: ResolvedExecutionPolicy {
                    max_context_tokens: 128_000,
                    max_turns: 25,
                    max_total_tokens: WireU64::new(1_000_000),
                    max_wall_ms: None,
                    criteria_gate_enabled: true,
                    repeat_fuse: ResolvedRepeatFuse {
                        enabled: true,
                        deny_after: 5,
                        terminate_after: 8,
                    },
                    entropy_watch: ResolvedEntropyWatch {
                        enabled: false,
                        threshold_ppm: Ppm::from_ppm_const(650_000),
                        hysteresis_ppm: Ppm::from_ppm_const(100_000),
                        cooldown_turns: 4,
                        notify_model: false,
                    },
                },
                governance_policy: ResolvedGovernancePolicy {
                    default_action: PolicyAction::Allow,
                    rules: Vec::new(),
                    vetoed_tools: Vec::new(),
                    rate_limits: Vec::new(),
                    constraints: Vec::new(),
                },
                scheduler_policy: ResolvedSchedulerPolicy {
                    critical_path_weight: 1_000_000,
                    fanout_weight: 10_000,
                    age_weight: 1_000,
                    token_cost_weight: 1,
                },
                resource_quota: ResourceQuota::default(),
                budget_grant: None,
                signal_policy: ResolvedSignalPolicy {
                    queue_max: 64,
                    ttl_ms: None,
                    deadline_escalation: false,
                },
                context_policy: ResolvedContextPolicy {
                    pressure_thresholds_ppm: PressureThresholds {
                        snip: Ppm::from_ppm_const(700_000),
                        micro: Ppm::from_ppm_const(800_000),
                        collapse: Ppm::from_ppm_const(900_000),
                        auto: Ppm::from_ppm_const(950_000),
                        renewal: Ppm::from_ppm_const(980_000),
                    },
                    target_after_compress_ppm: Ppm::from_ppm_const(650_000),
                    preserve_recent_turns: 2,
                    renewal_carryover_ppm: Ppm::from_ppm_const(50_000),
                    collapse_old_assistant_narration: true,
                    idle_micro_compact_minutes: 60,
                    knowledge_budget_ppm: Ppm::from_ppm_const(250_000),
                    prompt_budget: PromptBudget {
                        prompt_overhead_tokens: 0,
                        output_reserve_tokens: 0,
                        safety_margin_tokens: 0,
                    },
                },
                recovery_policy: ResolvedRecoveryPolicy {
                    provider_recovery_attempts: 1,
                    output_recovery_attempts: 1,
                    tail_bounds: TailBounds::DEFAULT,
                },
                payload_policy: ResolvedPayloadPolicy {
                    inline_threshold_bytes: 50 * 1024,
                    preview_bytes: 2 * 1024,
                },
                kernel_limits: ResolvedKernelLimits {
                    max_input_bytes: bootstrap_limits.absolute_max_input_bytes,
                    max_json_depth: bootstrap_limits.absolute_max_json_depth,
                    max_collection_entries: entries,
                    collection_limits: ResolvedCollectionLimits {
                        tool_catalog: entries,
                        skill_catalog: entries,
                        knowledge_entries: entries,
                        initial_messages: entries,
                        capability_grants: entries,
                        governance_rules: entries,
                    },
                },
                memory_access: None,
                memory_policy: ResolvedMemoryPolicy {
                    stale_warning_days: 2,
                    retrieval_top_k: 5,
                    validation_enabled: true,
                    max_content_bytes: 10_000,
                    max_name_length: 100,
                    promotion_recall_threshold: None,
                },
                tool_catalog: Vec::new(),
                skill_catalog: Vec::new(),
                verification_contracts: Vec::new(),
                feature_policy: ResolvedFeaturePolicy {
                    memory_enabled: false,
                    knowledge_enabled: false,
                    plan_tool_enabled: false,
                    tool_dispatch_gate: ToolDispatchGate::Exposed,
                    stable_core_tool_ids: Vec::new(),
                },
                host_effect_support: HostEffectSupport::default(),
            },
        }
    }
}

impl Default for ConfigDefaults {
    fn default() -> Self {
        Self::new(KernelBootstrapLimits::DEFAULT)
    }
}

// ---------------------------------------------------------------------------------------------
// resolution
// ---------------------------------------------------------------------------------------------

/// Normalise a sparse [`OperationConfig`] into the dense record the kernel stores, rejecting the
/// whole configuration if any field — or any relationship between fields — is illegal.
///
/// Atomicity is structural, not a discipline: this function owns no state and returns either a
/// complete resolved value or a rejection, so "the first eight fields applied and the ninth
/// failed" has no representation.
pub fn resolve_operation_config(
    config: &OperationConfig,
    defaults: &ConfigDefaults,
) -> Result<ResolvedOperationConfig, WireRejection> {
    let base = &defaults.baseline;

    let kernel_limits = resolve_kernel_limits(
        config.kernel_limits.as_ref(),
        &defaults.bootstrap_limits,
        &base.kernel_limits,
    )?;
    let execution_policy =
        resolve_execution(config.execution_policy.as_ref(), &base.execution_policy)?;
    let governance_policy = resolve_governance(
        config.governance_policy.as_ref(),
        &base.governance_policy,
        kernel_limits.collection_limits.governance_rules,
    )?;
    let scheduler_policy =
        resolve_scheduler(config.scheduler_policy.as_ref(), &base.scheduler_policy)?;
    let resource_quota = resolve_quota(config.resource_quota.as_ref(), &base.resource_quota)?;
    let budget_grant = resolve_budget_grant(config.budget_grant.as_ref())?;
    let signal_policy = resolve_signal(config.signal_policy.as_ref(), &base.signal_policy)?;
    let context_policy = resolve_context(
        config.context_policy.as_ref(),
        &base.context_policy,
        execution_policy.max_context_tokens,
    )?;
    let recovery_policy = resolve_recovery(config.recovery_policy.as_ref(), &base.recovery_policy)?;
    let payload_policy = resolve_payload(config.payload_policy.as_ref(), &base.payload_policy)?;
    let memory_policy = resolve_memory_policy(config.memory_policy.as_ref(), &base.memory_policy)?;
    let feature_policy = resolve_features(config.feature_policy.as_ref(), &base.feature_policy)?;

    let tool_catalog = resolve_tool_catalog(
        &config.tool_catalog,
        kernel_limits.collection_limits.tool_catalog,
    )?;
    let skill_catalog = resolve_skill_catalog(
        &config.skill_catalog,
        kernel_limits.collection_limits.skill_catalog,
        kernel_limits.collection_limits.capability_grants,
        &tool_catalog,
    )?;
    let host_effect_support = resolve_host_effect_support(&config.host_effect_support)?;
    let verification_contracts = resolve_verification_contracts(
        &config.verification_contracts,
        kernel_limits.max_collection_entries,
        &tool_catalog,
        &skill_catalog,
    )?;

    // cross-policy relationships that no single sub-resolver can see
    if feature_policy.memory_enabled && config.memory_access.is_none() {
        return Err(invalid(
            "feature_policy.memory_enabled is true but no memory_access binding was configured",
        ));
    }
    require_declared_effect_support(
        &host_effect_support,
        &feature_policy,
        config.memory_access.as_ref(),
        &resource_quota,
        budget_grant.as_ref(),
        &governance_policy,
        &tool_catalog,
        &verification_contracts,
    )?;
    for tool_id in &feature_policy.stable_core_tool_ids {
        if !tool_catalog.iter().any(|tool| &tool.name == tool_id) {
            return Err(invalid(format!(
                "feature_policy.stable_core_tool_ids names {tool_id:?}, \
                 which the tool catalog does not declare"
            )));
        }
    }

    Ok(ResolvedOperationConfig {
        abi_version: KERNEL_ABI_VERSION,
        execution_policy,
        governance_policy,
        scheduler_policy,
        resource_quota,
        budget_grant,
        signal_policy,
        context_policy,
        recovery_policy,
        payload_policy,
        kernel_limits,
        memory_access: config.memory_access.clone(),
        memory_policy,
        tool_catalog,
        skill_catalog,
        verification_contracts,
        feature_policy,
        host_effect_support,
    })
}

/// §7.3 · normalise the verification-contract catalog.
///
/// Three rules, all of them "a reference must resolve to something this operation declared":
///
/// 1. **`contract_id` is unique.** Duplicates make
///    `LogicalAgentSpec.verification_contract_id` ambiguous, and an ambiguous authority reference
///    resolves by list order — i.e. silently.
/// 2. **`phase_id` is unique within its contract.** The phase id is what `EvaluateMilestone`
///    carries and what a `MilestoneCheckResult` names on the way back, so two phases sharing one
///    would let a verdict for the second advance the first.
/// 3. **Every `unlocks` entry names a declared capability.** The capability directory an operation
///    has at configure time is its `tool_catalog` plus its `skill_catalog`; unlocking anything else
///    is a mount with nothing behind it.
///
/// An empty contract (no phases) is refused too: it can never publish an `EvaluateMilestone`, so a
/// spec pointing at one would be a gate that silently is not there.
fn resolve_verification_contracts(
    contracts: &[VerificationContract],
    max_entries: u32,
    tool_catalog: &[ToolSchema],
    skill_catalog: &[SkillMetadata],
) -> Result<Vec<VerificationContract>, WireRejection> {
    if contracts.len() as u64 > max_entries as u64 {
        return Err(too_many(format!(
            "verification_contracts carries {} entries; the bound is {max_entries}",
            contracts.len()
        )));
    }
    let mut seen_contracts: Vec<&str> = Vec::with_capacity(contracts.len());
    for contract in contracts {
        if contract.contract_id.is_empty() {
            return Err(invalid(
                "a verification contract must carry a non-empty contract_id",
            ));
        }
        if seen_contracts.contains(&contract.contract_id.as_str()) {
            return Err(invalid(format!(
                "verification_contracts declares {:?} twice; a contract id is the reference \
                 `verification_contract_id` resolves against and must be unique",
                contract.contract_id
            )));
        }
        seen_contracts.push(&contract.contract_id);

        if contract.phases.is_empty() {
            return Err(invalid(format!(
                "verification contract {:?} declares no phases; a contract with no phase can \
                 never be evaluated",
                contract.contract_id
            )));
        }
        if contract.phases.len() as u64 > max_entries as u64 {
            return Err(too_many(format!(
                "verification contract {:?} carries {} phases; the bound is {max_entries}",
                contract.contract_id,
                contract.phases.len()
            )));
        }
        let mut seen_phases: Vec<&str> = Vec::with_capacity(contract.phases.len());
        for phase in &contract.phases {
            if phase.phase_id.is_empty() {
                return Err(invalid(format!(
                    "verification contract {:?} carries a phase with an empty phase_id",
                    contract.contract_id
                )));
            }
            if seen_phases.contains(&phase.phase_id.as_str()) {
                return Err(invalid(format!(
                    "verification contract {:?} declares phase {:?} twice; a milestone verdict \
                     names its phase by id and could not say which one it advanced",
                    contract.contract_id, phase.phase_id
                )));
            }
            seen_phases.push(&phase.phase_id);
            for capability_id in &phase.unlocks {
                let declared = tool_catalog.iter().any(|tool| &tool.name == capability_id)
                    || skill_catalog
                        .iter()
                        .any(|skill| &skill.name == capability_id);
                if !declared {
                    return Err(invalid(format!(
                        "verification contract {:?} phase {:?} unlocks {capability_id:?}, which \
                         is in neither the tool catalog nor the skill catalog; a phase cannot \
                         mount a capability the operation never declared",
                        contract.contract_id, phase.phase_id
                    )));
                }
            }
        }
    }
    Ok(contracts.to_vec())
}

/// Refuse a configuration that switches a capability on while declaring the host cannot execute
/// the effect that capability needs (DEC-8, §7.3).
///
/// DEC-8 already fail-closes at *runtime*: an undeclared effect kind is never emitted and the
/// kernel commits a fault instead. This check exists for the case runtime fail-closure handles
/// badly — a **self-contradictory configuration**, where the host asks for a capability in one
/// field and disowns its only execution path in another. Discovering that on turn 40, as a fault,
/// is strictly worse than discovering it at `ConfigureOperation`.
///
/// That is why the conditions are *affirmative declarations*, not absences. An uncapped
/// `resource_quota` does not mean "this operation will spawn"; it means the host never said. A
/// `max_total_subagents: 8`, on the other hand, is a statement of intent, and pairing it with an
/// undeclared `spawn_tasks` is a contradiction the host can only have written by mistake.
///
/// | effect kind | required when | why it is hard |
/// | --- | --- | --- |
/// | `call_provider` | always | no execution mode exists that never calls a provider |
/// | `execute_tools` | `tool_catalog` non-empty | a catalog the host cannot dispatch is a fail-open exposure surface |
/// | `load_payload` | `tool_catalog` non-empty | any result may exceed the inline threshold and become `External`; producing one the host cannot load back makes it unreadable (§7.10) |
/// | `request_approval` | governance can yield `AskUser` | the gate would otherwise produce an approval it has no way to ask for |
/// | `spawn_tasks` | quota/grant declares positive spawn or workflow capacity | declared capacity with no launch path |
/// | `preempt_tasks` | same condition as `spawn_tasks` | children you cannot stop leak past cancellation and budget exhaustion |
/// | `persist_memory` | `memory_enabled`, or the binding grants `write` | |
/// | `query_memory` | `memory_enabled`, or the binding grants `read` | |
/// | `archive_page_out` | `knowledge_enabled` | the knowledge partition is what gets swept out under budget pressure |
/// | `evaluate_milestone` | `verification_contracts` non-empty | a contract nothing can evaluate never resolves |
#[allow(clippy::too_many_arguments)]
fn require_declared_effect_support(
    support: &HostEffectSupport,
    features: &ResolvedFeaturePolicy,
    memory_access: Option<&MemoryAccessBinding>,
    quota: &ResourceQuota,
    grant: Option<&BudgetGrant>,
    governance: &ResolvedGovernancePolicy,
    tool_catalog: &[ToolSchema],
    verification_contracts: &[VerificationContract],
) -> Result<(), WireRejection> {
    let has_tools = !tool_catalog.is_empty();
    let can_ask_user = governance.default_action == PolicyAction::AskUser
        || governance
            .rules
            .iter()
            .any(|rule| rule.action == PolicyAction::AskUser);
    let declares_spawn_capacity = [
        quota.max_concurrent_subagents,
        quota.max_total_subagents,
        quota.max_spawn_depth,
        quota.max_workflow_nodes,
        grant.and_then(|grant| grant.subagents),
    ]
    .into_iter()
    .flatten()
    .any(|capacity| capacity > 0);
    let memory_write =
        features.memory_enabled || memory_access.is_some_and(|access| access.capabilities.write);
    let memory_read =
        features.memory_enabled || memory_access.is_some_and(|access| access.capabilities.read);

    for (kind, required, because) in [
        (
            EffectKindTag::CallProvider,
            true,
            "every operation reaches a provider call",
        ),
        (
            EffectKindTag::ExecuteTools,
            has_tools,
            "tool_catalog declares tools this operation may dispatch",
        ),
        (
            EffectKindTag::LoadPayload,
            has_tools,
            "a tool result above the inline threshold becomes an external payload the kernel \
             must be able to page back in",
        ),
        (
            EffectKindTag::RequestApproval,
            can_ask_user,
            "governance_policy can return ask_user",
        ),
        (
            EffectKindTag::SpawnTasks,
            declares_spawn_capacity,
            "resource_quota or budget_grant declares spawn/workflow capacity",
        ),
        (
            EffectKindTag::PreemptTasks,
            declares_spawn_capacity,
            "an operation that may start child tasks must be able to stop them on \
             cancellation or budget exhaustion",
        ),
        (
            EffectKindTag::PersistMemory,
            memory_write,
            "the memory plane is writable",
        ),
        (
            EffectKindTag::QueryMemory,
            memory_read,
            "the memory plane is readable",
        ),
        (
            EffectKindTag::ArchivePageOut,
            features.knowledge_enabled,
            "feature_policy.knowledge_enabled exposes a partition that is paged out under \
             budget pressure",
        ),
        (
            EffectKindTag::EvaluateMilestone,
            !verification_contracts.is_empty(),
            "verification_contracts declares contracts that must be evaluated",
        ),
    ] {
        if required && !support.supports(kind) {
            return Err(invalid(format!(
                "host_effect_support does not declare {:?}, but {because}; \
                 a capability the host cannot execute must not be configured on",
                kind.as_str()
            )));
        }
    }
    Ok(())
}

fn resolve_kernel_limits(
    limits: Option<&KernelLimits>,
    bootstrap: &KernelBootstrapLimits,
    base: &ResolvedKernelLimits,
) -> Result<ResolvedKernelLimits, WireRejection> {
    let mut resolved = *base;
    resolved.max_input_bytes = bootstrap.absolute_max_input_bytes;
    resolved.max_json_depth = bootstrap.absolute_max_json_depth;
    resolved.max_collection_entries = bootstrap.absolute_max_collection_entries;

    if let Some(limits) = limits {
        if let Some(bytes) = limits.max_input_bytes {
            require_le_u32(
                "kernel_limits.max_input_bytes",
                bytes,
                bootstrap.absolute_max_input_bytes,
                "absolute_max_input_bytes",
            )?;
            if bytes == 0 {
                return Err(invalid("kernel_limits.max_input_bytes must be positive"));
            }
            resolved.max_input_bytes = bytes;
        }
        if let Some(depth) = limits.max_json_depth {
            require_le_u32(
                "kernel_limits.max_json_depth",
                u32::from(depth),
                u32::from(bootstrap.absolute_max_json_depth),
                "absolute_max_json_depth",
            )?;
            if depth == 0 {
                return Err(invalid("kernel_limits.max_json_depth must be positive"));
            }
            resolved.max_json_depth = depth;
        }
        if let Some(entries) = limits.max_collection_entries {
            require_le_u32(
                "kernel_limits.max_collection_entries",
                entries,
                bootstrap.absolute_max_collection_entries,
                "absolute_max_collection_entries",
            )?;
            if entries == 0 {
                return Err(invalid(
                    "kernel_limits.max_collection_entries must be positive",
                ));
            }
            resolved.max_collection_entries = entries;
        }
    }

    let ceiling = resolved.max_collection_entries;
    let mut per_collection = ResolvedCollectionLimits {
        tool_catalog: ceiling,
        skill_catalog: ceiling,
        knowledge_entries: ceiling,
        initial_messages: ceiling,
        capability_grants: ceiling,
        governance_rules: ceiling,
    };

    if let Some(named) = limits.and_then(|limits| limits.collection_limits.as_ref()) {
        for (label, requested, slot) in [
            (
                "tool_catalog",
                named.tool_catalog,
                &mut per_collection.tool_catalog,
            ),
            (
                "skill_catalog",
                named.skill_catalog,
                &mut per_collection.skill_catalog,
            ),
            (
                "knowledge_entries",
                named.knowledge_entries,
                &mut per_collection.knowledge_entries,
            ),
            (
                "initial_messages",
                named.initial_messages,
                &mut per_collection.initial_messages,
            ),
            (
                "capability_grants",
                named.capability_grants,
                &mut per_collection.capability_grants,
            ),
            (
                "governance_rules",
                named.governance_rules,
                &mut per_collection.governance_rules,
            ),
        ] {
            if let Some(requested) = requested {
                require_le_u32(
                    &format!("kernel_limits.collection_limits.{label}"),
                    requested,
                    ceiling,
                    "the resolved max_collection_entries",
                )?;
                *slot = requested;
            }
        }
    }

    resolved.collection_limits = per_collection;
    Ok(resolved)
}

fn resolve_execution(
    policy: Option<&ExecutionPolicy>,
    base: &ResolvedExecutionPolicy,
) -> Result<ResolvedExecutionPolicy, WireRejection> {
    let mut resolved = base.clone();
    if let Some(policy) = policy {
        if let Some(value) = policy.max_context_tokens {
            resolved.max_context_tokens = value;
        }
        if let Some(value) = policy.max_turns {
            resolved.max_turns = value;
        }
        if let Some(value) = policy.max_total_tokens {
            resolved.max_total_tokens = value;
        }
        // absent `max_wall_ms` keeps the baseline; clearing a deadline is `UpdateDeadline`'s job
        if let Some(value) = policy.max_wall_ms {
            resolved.max_wall_ms = Some(value);
        }
        if let Some(value) = policy.criteria_gate_enabled {
            resolved.criteria_gate_enabled = value;
        }
        if let Some(fuse) = &policy.repeat_fuse {
            if let Some(value) = fuse.enabled {
                resolved.repeat_fuse.enabled = value;
            }
            if let Some(value) = fuse.deny_after {
                resolved.repeat_fuse.deny_after = value;
            }
            if let Some(value) = fuse.terminate_after {
                resolved.repeat_fuse.terminate_after = value;
            }
        }
        if let Some(watch) = &policy.entropy_watch {
            if let Some(value) = watch.enabled {
                resolved.entropy_watch.enabled = value;
            }
            if let Some(value) = watch.threshold_ppm {
                resolved.entropy_watch.threshold_ppm = value;
            }
            if let Some(value) = watch.hysteresis_ppm {
                resolved.entropy_watch.hysteresis_ppm = value;
            }
            if let Some(value) = watch.cooldown_turns {
                resolved.entropy_watch.cooldown_turns = value;
            }
            if let Some(value) = watch.notify_model {
                resolved.entropy_watch.notify_model = value;
            }
        }
    }

    if resolved.max_turns == 0 {
        return Err(invalid("execution_policy.max_turns must be positive"));
    }
    if resolved.max_context_tokens == 0 {
        return Err(invalid(
            "execution_policy.max_context_tokens must be positive",
        ));
    }
    if resolved.max_total_tokens.get() == 0 {
        return Err(invalid(
            "execution_policy.max_total_tokens must be positive",
        ));
    }
    if resolved.max_wall_ms.is_some_and(|ms| ms.get() == 0) {
        return Err(invalid(
            "execution_policy.max_wall_ms must be positive; omit it for no wall-clock limit",
        ));
    }
    if resolved.repeat_fuse.enabled {
        if resolved.repeat_fuse.deny_after == 0 {
            return Err(invalid(
                "execution_policy.repeat_fuse.deny_after must be positive while the fuse is enabled",
            ));
        }
        if resolved.repeat_fuse.terminate_after <= resolved.repeat_fuse.deny_after {
            return Err(invalid(format!(
                "execution_policy.repeat_fuse.terminate_after ({}) must exceed deny_after ({}); \
                 otherwise the run terminates before the deny ever takes effect",
                resolved.repeat_fuse.terminate_after, resolved.repeat_fuse.deny_after
            )));
        }
    }
    if resolved.entropy_watch.enabled
        && resolved.entropy_watch.hysteresis_ppm > resolved.entropy_watch.threshold_ppm
    {
        return Err(invalid(format!(
            "execution_policy.entropy_watch.hysteresis_ppm ({}) must not exceed threshold_ppm ({}); \
             a wider hysteresis than threshold can never re-arm",
            resolved.entropy_watch.hysteresis_ppm.get(),
            resolved.entropy_watch.threshold_ppm.get()
        )));
    }
    Ok(resolved)
}

fn resolve_governance(
    policy: Option<&GovernancePolicy>,
    base: &ResolvedGovernancePolicy,
    rule_bound: u32,
) -> Result<ResolvedGovernancePolicy, WireRejection> {
    let mut resolved = base.clone();
    if let Some(policy) = policy {
        if let Some(action) = policy.default_action {
            resolved.default_action = action;
        }
        resolved.rules = policy.rules.clone();
        resolved.vetoed_tools = policy.vetoed_tools.clone();
        resolved.rate_limits = policy.rate_limits.clone();
        resolved.constraints = policy.constraints.clone();
    }
    validate_governance(&resolved, rule_bound)?;
    Ok(resolved)
}

/// Shared by boot resolution and the live `ReplaceGovernancePolicy` patch, so the two can never
/// disagree about what a legal governance posture is.
pub(super) fn validate_governance(
    policy: &ResolvedGovernancePolicy,
    rule_bound: u32,
) -> Result<(), WireRejection> {
    let total = policy.rules.len() + policy.rate_limits.len() + policy.constraints.len();
    if total > rule_bound as usize {
        return Err(too_many(format!(
            "governance policy declares {total} rules/limits/constraints; \
             the resolved governance_rules bound is {rule_bound}"
        )));
    }
    for rule in &policy.rules {
        if rule.tool_pattern.is_empty() {
            return Err(invalid("governance rule tool_pattern must not be empty"));
        }
    }
    for tool in &policy.vetoed_tools {
        if tool.is_empty() {
            return Err(invalid("governance vetoed_tools entries must not be empty"));
        }
    }
    for limit in &policy.rate_limits {
        if limit.tool.is_empty() {
            return Err(invalid("governance rate limit tool must not be empty"));
        }
        if limit.window_ms.get() == 0 {
            return Err(invalid(format!(
                "governance rate limit for {:?} has a zero window",
                limit.tool
            )));
        }
    }
    for constraint in &policy.constraints {
        constraint.validate().map_err(invalid)?;
    }
    Ok(())
}

fn resolve_scheduler(
    policy: Option<&SchedulerPolicy>,
    base: &ResolvedSchedulerPolicy,
) -> Result<ResolvedSchedulerPolicy, WireRejection> {
    let mut resolved = *base;
    if let Some(policy) = policy {
        if let Some(value) = policy.critical_path_weight {
            resolved.critical_path_weight = value;
        }
        if let Some(value) = policy.fanout_weight {
            resolved.fanout_weight = value;
        }
        if let Some(value) = policy.age_weight {
            resolved.age_weight = value;
        }
        if let Some(value) = policy.token_cost_weight {
            resolved.token_cost_weight = value;
        }
    }
    for (label, weight) in [
        ("critical_path_weight", resolved.critical_path_weight),
        ("fanout_weight", resolved.fanout_weight),
        ("age_weight", resolved.age_weight),
        ("token_cost_weight", resolved.token_cost_weight),
    ] {
        if weight > MAX_SCHEDULER_WEIGHT {
            return Err(invalid(format!(
                "scheduler_policy.{label} is {weight}; the bound is {MAX_SCHEDULER_WEIGHT}"
            )));
        }
    }
    Ok(resolved)
}

fn resolve_quota(
    quota: Option<&ResourceQuota>,
    base: &ResourceQuota,
) -> Result<ResourceQuota, WireRejection> {
    let resolved = quota.cloned().unwrap_or_else(|| base.clone());
    validate_quota(&resolved)?;
    Ok(resolved)
}

pub(super) fn validate_quota(quota: &ResourceQuota) -> Result<(), WireRejection> {
    if let (Some(concurrent), Some(total)) =
        (quota.max_concurrent_subagents, quota.max_total_subagents)
        && concurrent > total
    {
        return Err(invalid(format!(
            "resource_quota.max_concurrent_subagents ({concurrent}) exceeds \
             max_total_subagents ({total}); the concurrent cap can never be reached"
        )));
    }
    if quota.max_spawn_depth == Some(0) {
        return Err(invalid(
            "resource_quota.max_spawn_depth must be positive; omit it for no depth cap",
        ));
    }
    if let Some(window) = &quota.memory_writes_per_window
        && window.window_ms.get() == 0
    {
        return Err(invalid(
            "resource_quota.memory_writes_per_window.window_ms must be positive",
        ));
    }
    Ok(())
}

fn resolve_budget_grant(grant: Option<&BudgetGrant>) -> Result<Option<BudgetGrant>, WireRejection> {
    let Some(grant) = grant else {
        return Ok(None);
    };
    if grant.reservation_id.is_empty() {
        return Err(invalid("budget_grant.reservation_id must not be empty"));
    }
    if grant.tokens.is_some_and(|tokens| tokens.get() == 0) {
        return Err(invalid(
            "budget_grant.tokens must be positive; a zero grant is a refused admission, \
             not a configuration",
        ));
    }
    Ok(Some(grant.clone()))
}

fn resolve_signal(
    policy: Option<&SignalPolicy>,
    base: &ResolvedSignalPolicy,
) -> Result<ResolvedSignalPolicy, WireRejection> {
    let resolved = match policy {
        Some(policy) => ResolvedSignalPolicy {
            queue_max: policy.queue_max,
            ttl_ms: policy.ttl_ms,
            deadline_escalation: policy
                .deadline_escalation
                .unwrap_or(base.deadline_escalation),
        },
        None => *base,
    };
    validate_signal(&resolved)?;
    Ok(resolved)
}

pub(super) fn validate_signal(policy: &ResolvedSignalPolicy) -> Result<(), WireRejection> {
    if policy.queue_max == 0 {
        return Err(invalid("signal_policy.queue_max must be positive"));
    }
    if policy.ttl_ms.is_some_and(|ttl| ttl.get() == 0) {
        return Err(invalid(
            "signal_policy.ttl_ms must be positive; omit it for no expiry",
        ));
    }
    Ok(())
}

fn resolve_context(
    policy: Option<&ContextPolicy>,
    base: &ResolvedContextPolicy,
    max_context_tokens: u32,
) -> Result<ResolvedContextPolicy, WireRejection> {
    let mut resolved = base.clone();
    if let Some(policy) = policy {
        if let Some(value) = policy.pressure_thresholds_ppm {
            resolved.pressure_thresholds_ppm = value;
        }
        if let Some(value) = policy.target_after_compress_ppm {
            resolved.target_after_compress_ppm = value;
        }
        if let Some(value) = policy.preserve_recent_turns {
            resolved.preserve_recent_turns = value;
        }
        if let Some(value) = policy.renewal_carryover_ppm {
            resolved.renewal_carryover_ppm = value;
        }
        if let Some(value) = policy.collapse_old_assistant_narration {
            resolved.collapse_old_assistant_narration = value;
        }
        if let Some(value) = policy.idle_micro_compact_minutes {
            resolved.idle_micro_compact_minutes = value;
        }
        if let Some(value) = policy.knowledge_budget_ppm {
            resolved.knowledge_budget_ppm = value;
        }
        if let Some(value) = policy.prompt_budget {
            resolved.prompt_budget = value;
        }
    }

    let t = &resolved.pressure_thresholds_ppm;
    if !(t.snip < t.micro && t.micro < t.collapse && t.collapse < t.auto && t.auto < t.renewal) {
        return Err(invalid(format!(
            "context_policy pressure thresholds must strictly increase \
             (snip {} < micro {} < collapse {} < auto {} < renewal {})",
            t.snip.get(),
            t.micro.get(),
            t.collapse.get(),
            t.auto.get(),
            t.renewal.get()
        )));
    }
    if resolved.target_after_compress_ppm >= t.snip {
        return Err(invalid(format!(
            "context_policy.target_after_compress_ppm ({}) must be below the snip threshold ({}); \
             otherwise a compression pass can never reach its own target",
            resolved.target_after_compress_ppm.get(),
            t.snip.get()
        )));
    }
    if resolved.preserve_recent_turns == 0 {
        return Err(invalid(
            "context_policy.preserve_recent_turns must be positive",
        ));
    }
    if resolved.knowledge_budget_ppm.get() + resolved.renewal_carryover_ppm.get() > Ppm::MAX_PPM {
        return Err(invalid(format!(
            "context_policy.knowledge_budget_ppm ({}) plus renewal_carryover_ppm ({}) exceeds \
             the whole context budget",
            resolved.knowledge_budget_ppm.get(),
            resolved.renewal_carryover_ppm.get()
        )));
    }
    if resolved.prompt_budget.reserved_tokens() >= max_context_tokens {
        return Err(invalid(format!(
            "context_policy.prompt_budget reserves {} tokens of a {max_context_tokens}-token \
             context window, leaving nothing to render",
            resolved.prompt_budget.reserved_tokens()
        )));
    }
    Ok(resolved)
}

fn resolve_recovery(
    policy: Option<&RecoveryPolicy>,
    base: &ResolvedRecoveryPolicy,
) -> Result<ResolvedRecoveryPolicy, WireRejection> {
    let mut resolved = *base;
    if let Some(policy) = policy {
        if let Some(value) = policy.provider_recovery_attempts {
            resolved.provider_recovery_attempts = value;
        }
        if let Some(value) = policy.output_recovery_attempts {
            resolved.output_recovery_attempts = value;
        }
        if let Some(bounds) = &policy.tail_bounds {
            resolved.tail_bounds = apply_tail_bounds(bounds, resolved.tail_bounds);
        }
    }
    validate_recovery(&resolved)?;
    Ok(resolved)
}

/// Per-axis override. A host that states one axis keeps the kernel's baseline on the other three,
/// which is the same sparse-overlay rule every other policy here follows.
fn apply_tail_bounds(policy: &TailBoundsPolicy, base: TailBounds) -> TailBounds {
    TailBounds {
        soft_records: policy.soft_records.unwrap_or(base.soft_records),
        hard_records: policy.hard_records.unwrap_or(base.hard_records),
        soft_bytes: policy.soft_bytes.unwrap_or(base.soft_bytes),
        hard_bytes: policy.hard_bytes.unwrap_or(base.hard_bytes),
    }
}

/// Ceiling shared by both semantic recovery ladders, matching the legacy validator.
pub const MAX_RECOVERY_ATTEMPTS: u8 = 16;

pub(super) fn validate_recovery(policy: &ResolvedRecoveryPolicy) -> Result<(), WireRejection> {
    for (label, value) in [
        (
            "provider_recovery_attempts",
            policy.provider_recovery_attempts,
        ),
        ("output_recovery_attempts", policy.output_recovery_attempts),
    ] {
        if value > MAX_RECOVERY_ATTEMPTS {
            return Err(invalid(format!(
                "recovery_policy.{label} is {value}; the bound is {MAX_RECOVERY_ATTEMPTS}"
            )));
        }
    }
    policy.tail_bounds.check().map_err(invalid)?;
    Ok(())
}

fn resolve_payload(
    policy: Option<&PayloadPolicy>,
    base: &ResolvedPayloadPolicy,
) -> Result<ResolvedPayloadPolicy, WireRejection> {
    let mut resolved = *base;
    if let Some(policy) = policy {
        if let Some(value) = policy.inline_threshold_bytes {
            resolved.inline_threshold_bytes = value;
        }
        if let Some(value) = policy.preview_bytes {
            resolved.preview_bytes = value;
        }
    }
    if resolved.inline_threshold_bytes == 0 {
        return Err(invalid(
            "payload_policy.inline_threshold_bytes must be positive",
        ));
    }
    if resolved.preview_bytes == 0 || resolved.preview_bytes > resolved.inline_threshold_bytes {
        return Err(invalid(format!(
            "payload_policy.preview_bytes ({}) must be positive and no larger than \
             inline_threshold_bytes ({})",
            resolved.preview_bytes, resolved.inline_threshold_bytes
        )));
    }
    Ok(resolved)
}

fn resolve_memory_policy(
    policy: Option<&MemoryPolicy>,
    base: &ResolvedMemoryPolicy,
) -> Result<ResolvedMemoryPolicy, WireRejection> {
    let mut resolved = *base;
    if let Some(policy) = policy {
        if let Some(value) = policy.stale_warning_days {
            resolved.stale_warning_days = value;
        }
        if let Some(value) = policy.retrieval_top_k {
            resolved.retrieval_top_k = value;
        }
        if let Some(value) = policy.validation_enabled {
            resolved.validation_enabled = value;
        }
        if let Some(value) = policy.max_content_bytes {
            resolved.max_content_bytes = value;
        }
        if let Some(value) = policy.max_name_length {
            resolved.max_name_length = value;
        }
        if let Some(value) = policy.promotion_recall_threshold {
            resolved.promotion_recall_threshold = Some(value);
        }
    }
    if resolved.retrieval_top_k == 0 {
        return Err(invalid("memory_policy.retrieval_top_k must be positive"));
    }
    if resolved.validation_enabled
        && (resolved.max_content_bytes == 0 || resolved.max_name_length == 0)
    {
        return Err(invalid(
            "memory_policy validation is enabled but max_content_bytes / max_name_length is zero, \
             which rejects every write",
        ));
    }
    if resolved
        .promotion_recall_threshold
        .is_some_and(|threshold| threshold.get() == 0)
    {
        return Err(invalid(
            "memory_policy.promotion_recall_threshold must be positive; \
             omit it to disable promotion suggestions",
        ));
    }
    Ok(resolved)
}

fn resolve_features(
    policy: Option<&FeaturePolicy>,
    base: &ResolvedFeaturePolicy,
) -> Result<ResolvedFeaturePolicy, WireRejection> {
    let mut resolved = base.clone();
    if let Some(policy) = policy {
        if let Some(value) = policy.memory_enabled {
            resolved.memory_enabled = value;
        }
        if let Some(value) = policy.knowledge_enabled {
            resolved.knowledge_enabled = value;
        }
        if let Some(value) = policy.plan_tool_enabled {
            resolved.plan_tool_enabled = value;
        }
        if let Some(value) = policy.tool_dispatch_gate {
            resolved.tool_dispatch_gate = value;
        }
        if let Some(ids) = &policy.stable_core_tool_ids {
            resolved.stable_core_tool_ids = ids.clone();
        }
    }
    for id in &resolved.stable_core_tool_ids {
        if id.is_empty() {
            return Err(invalid(
                "feature_policy.stable_core_tool_ids entries must not be empty",
            ));
        }
    }
    Ok(resolved)
}

fn resolve_tool_catalog(
    catalog: &[ToolSchema],
    bound: u32,
) -> Result<Vec<ToolSchema>, WireRejection> {
    if catalog.len() > bound as usize {
        return Err(too_many(format!(
            "tool_catalog declares {} tools; the resolved bound is {bound}",
            catalog.len()
        )));
    }
    for (index, tool) in catalog.iter().enumerate() {
        if tool.name.is_empty() {
            return Err(invalid("tool_catalog entry has an empty name"));
        }
        if catalog[..index].iter().any(|other| other.name == tool.name) {
            return Err(invalid(format!(
                "tool_catalog declares {:?} twice; a catalog is a set, and a duplicate makes \
                 dispatch order-dependent",
                tool.name
            )));
        }
    }
    Ok(catalog.to_vec())
}

fn resolve_skill_catalog(
    catalog: &[SkillMetadata],
    bound: u32,
    capability_grants_bound: u32,
    tools: &[ToolSchema],
) -> Result<Vec<SkillMetadata>, WireRejection> {
    if catalog.len() > bound as usize {
        return Err(too_many(format!(
            "skill_catalog declares {} skills; the resolved bound is {bound}",
            catalog.len()
        )));
    }
    for (index, skill) in catalog.iter().enumerate() {
        if skill.name.is_empty() {
            return Err(invalid("skill_catalog entry has an empty name"));
        }
        if catalog[..index]
            .iter()
            .any(|other| other.name == skill.name)
        {
            return Err(invalid(format!(
                "skill_catalog declares {:?} twice",
                skill.name
            )));
        }
        if skill
            .effort
            .is_some_and(|effort| !(1..=5).contains(&effort))
        {
            return Err(invalid(format!(
                "skill {:?} declares effort {}; the range is 1..=5",
                skill.name,
                skill.effort.unwrap_or_default()
            )));
        }
        if skill.capability_grants.len() > capability_grants_bound as usize {
            return Err(too_many(format!(
                "skill {:?} declares {} capability grants; the resolved bound is {capability_grants_bound}",
                skill.name,
                skill.capability_grants.len()
            )));
        }
        for tool in &skill.allowed_tools {
            if !tools.iter().any(|declared| &declared.name == tool) {
                return Err(invalid(format!(
                    "skill {:?} allows {tool:?}, which the tool catalog does not declare; \
                     a skill can only ever narrow the catalog",
                    skill.name
                )));
            }
        }
    }
    Ok(catalog.to_vec())
}

fn resolve_host_effect_support(
    support: &HostEffectSupport,
) -> Result<HostEffectSupport, WireRejection> {
    for (index, kind) in support.supported.iter().enumerate() {
        if support.supported[..index].contains(kind) {
            return Err(invalid(format!(
                "host_effect_support declares {kind:?} twice"
            )));
        }
    }
    Ok(support.clone())
}

// ---------------------------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::kernel::wire::command::{
        HostCommand, LivePolicyPatch, ParamConstraint, PolicyRule, RateLimitSpec,
        ReplaceGovernancePolicy, ReplaceRecoveryPolicy, ReplaceSignalPolicy, RequiredParam,
        TightenResourceQuota,
    };
    use crate::runtime::kernel::wire::effect::MemoryCapabilities;
    use crate::runtime::kernel::wire::scalar::{BoundedJson, MemoryBindingId, SCALAR_ERROR_MARKER};
    use serde_json::{Value, json};
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::PathBuf;

    // -----------------------------------------------------------------------------------------
    // helpers
    // -----------------------------------------------------------------------------------------

    fn ppm(value: u32) -> Ppm {
        Ppm::new(value).unwrap()
    }

    fn minimal_config() -> OperationConfig {
        OperationConfig {
            host_effect_support: HostEffectSupport::new([EffectKindTag::CallProvider]),
            ..OperationConfig::default()
        }
    }

    fn defaults() -> ConfigDefaults {
        ConfigDefaults::default()
    }

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
                let raw = fs::read_to_string(dir.join(&name))
                    .unwrap_or_else(|e| panic!("failed to read {name}: {e}"));
                let value: Value = serde_json::from_str(&raw)
                    .unwrap_or_else(|e| panic!("{name} is not JSON: {e}"));
                (name, value)
            })
            .collect()
    }

    fn all_keys(value: &Value, out: &mut BTreeSet<String>) {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    out.insert(key.clone());
                    all_keys(child, out);
                }
            }
            Value::Array(items) => items.iter().for_each(|item| all_keys(item, out)),
            _ => {}
        }
    }

    /// A config exercising every field, so key-absence tests see the whole surface.
    fn fully_populated_config() -> OperationConfig {
        OperationConfig {
            execution_policy: Some(ExecutionPolicy {
                max_context_tokens: Some(200_000),
                max_turns: Some(40),
                max_total_tokens: Some(WireU64::new(2_000_000)),
                max_wall_ms: Some(WireU64::new(600_000)),
                criteria_gate_enabled: Some(true),
                repeat_fuse: Some(RepeatFusePolicy {
                    enabled: Some(true),
                    deny_after: Some(4),
                    terminate_after: Some(7),
                }),
                entropy_watch: Some(EntropyWatchPolicy {
                    enabled: Some(true),
                    threshold_ppm: Some(ppm(650_000)),
                    hysteresis_ppm: Some(ppm(100_000)),
                    cooldown_turns: Some(3),
                    notify_model: Some(true),
                }),
            }),
            governance_policy: Some(GovernancePolicy {
                default_action: Some(PolicyAction::AskUser),
                rules: vec![PolicyRule {
                    tool_pattern: "shell.*".to_string(),
                    action: PolicyAction::Deny,
                }],
                vetoed_tools: vec!["rm".to_string()],
                rate_limits: vec![RateLimitSpec {
                    tool: "search".to_string(),
                    max_calls: 10,
                    window_ms: WireU64::new(60_000),
                }],
                constraints: vec![ParamConstraint::Required(RequiredParam {
                    tool: "write".to_string(),
                    param_path: "destination".to_string(),
                })],
            }),
            scheduler_policy: Some(SchedulerPolicy {
                critical_path_weight: Some(900_000),
                fanout_weight: Some(9_000),
                age_weight: Some(900),
                token_cost_weight: Some(2),
            }),
            resource_quota: Some(ResourceQuota {
                max_concurrent_subagents: Some(2),
                max_total_subagents: Some(8),
                max_spawn_depth: Some(2),
                max_workflow_nodes: Some(64),
                memory_writes_per_window: Some(RateWindow {
                    max_events: 4,
                    window_ms: WireU64::new(60_000),
                }),
            }),
            budget_grant: Some(BudgetGrant {
                reservation_id: "res-1".to_string(),
                tokens: Some(WireU64::new(500_000)),
                subagents: Some(4),
                rounds: Some(3),
            }),
            signal_policy: Some(SignalPolicy {
                queue_max: 32,
                ttl_ms: Some(WireU64::new(30_000)),
                deadline_escalation: Some(true),
            }),
            context_policy: Some(ContextPolicy {
                pressure_thresholds_ppm: Some(PressureThresholds {
                    snip: ppm(700_000),
                    micro: ppm(800_000),
                    collapse: ppm(900_000),
                    auto: ppm(950_000),
                    renewal: ppm(980_000),
                }),
                target_after_compress_ppm: Some(ppm(650_000)),
                preserve_recent_turns: Some(3),
                renewal_carryover_ppm: Some(ppm(50_000)),
                collapse_old_assistant_narration: Some(true),
                idle_micro_compact_minutes: Some(45),
                knowledge_budget_ppm: Some(ppm(250_000)),
                prompt_budget: Some(PromptBudget {
                    prompt_overhead_tokens: 1_200,
                    output_reserve_tokens: 4_000,
                    safety_margin_tokens: 500,
                }),
            }),
            recovery_policy: Some(RecoveryPolicy {
                provider_recovery_attempts: Some(2),
                output_recovery_attempts: Some(1),
                tail_bounds: Some(TailBoundsPolicy {
                    soft_records: Some(WireU64::new(8)),
                    hard_records: Some(WireU64::new(16)),
                    soft_bytes: Some(WireU64::new(4_096)),
                    hard_bytes: Some(WireU64::new(65_536)),
                }),
            }),
            payload_policy: Some(PayloadPolicy {
                inline_threshold_bytes: Some(32_768),
                preview_bytes: Some(1_024),
            }),
            kernel_limits: Some(KernelLimits {
                max_input_bytes: Some(1_048_576),
                max_json_depth: Some(32),
                max_collection_entries: Some(4_096),
                collection_limits: Some(CollectionLimits {
                    tool_catalog: Some(256),
                    skill_catalog: Some(64),
                    knowledge_entries: Some(512),
                    initial_messages: Some(1_024),
                    capability_grants: Some(128),
                    governance_rules: Some(64),
                }),
            }),
            memory_access: Some(MemoryAccessBinding {
                binding_id: MemoryBindingId::new("mem-binding-1").unwrap(),
                capabilities: MemoryCapabilities {
                    read: true,
                    write: true,
                },
            }),
            memory_policy: Some(MemoryPolicy {
                stale_warning_days: Some(7),
                retrieval_top_k: Some(8),
                validation_enabled: Some(true),
                max_content_bytes: Some(20_000),
                max_name_length: Some(120),
                promotion_recall_threshold: Some(WireU64::new(3)),
            }),
            tool_catalog: vec![
                ToolSchema {
                    name: "search".to_string(),
                    description: "search the corpus".to_string(),
                    parameters: BoundedJson::new(json!({"type": "object"})).unwrap(),
                },
                ToolSchema {
                    name: "write".to_string(),
                    description: "write a file".to_string(),
                    parameters: BoundedJson::new(json!({"type": "object"})).unwrap(),
                },
            ],
            verification_contracts: vec![VerificationContract {
                contract_id: "brief-quality-v1".to_string(),
                phases: vec![
                    MilestonePhase {
                        phase_id: "collect".to_string(),
                        unlocks: vec!["research".to_string()],
                    },
                    MilestonePhase {
                        phase_id: "write".to_string(),
                        unlocks: vec!["write".to_string()],
                    },
                ],
            }],
            skill_catalog: vec![SkillMetadata {
                name: "research".to_string(),
                description: "run a literature sweep".to_string(),
                when_to_use: Some("sources,citations".to_string()),
                allowed_tools: vec!["search".to_string()],
                capability_grants: Vec::new(),
                effort: Some(3),
                estimated_tokens: Some(900),
            }],
            feature_policy: Some(FeaturePolicy {
                memory_enabled: Some(true),
                knowledge_enabled: Some(true),
                plan_tool_enabled: Some(true),
                tool_dispatch_gate: Some(ToolDispatchGate::Exposed),
                stable_core_tool_ids: Some(vec!["search".to_string()]),
            }),
            // an all-fields config switches on every capability, so it must declare every
            // effect kind those capabilities need
            host_effect_support: HostEffectSupport::new(EffectKindTag::ALL),
        }
    }

    // -----------------------------------------------------------------------------------------
    // §13.3 · deleted items exist nowhere in the new contract
    // -----------------------------------------------------------------------------------------

    #[test]
    fn deleted_and_host_side_config_appears_in_no_new_type() {
        // §13.3 deletions and host-side moves. If any of these ever reappears, the config
        // convergence silently regressed to the pre-Canonical surface.
        const BANNED: [&str; 16] = [
            "memory_path",
            "tokenizer",
            "host_effect_retry_attempts",
            "spool_dir",
            "spool_ref",
            "spool_threshold_bytes",
            "spool_preview_bytes",
            "archive_ref",
            "checkpoint_path",
            "endpoint",
            "api_key",
            "base_url",
            "provider",
            "session_id",
            "parent_session_id",
            "path_root",
        ];

        let mut keys = BTreeSet::new();
        all_keys(
            &serde_json::to_value(fully_populated_config()).unwrap(),
            &mut keys,
        );
        all_keys(
            &serde_json::to_value(fully_populated_config().resolve(&defaults()).unwrap()).unwrap(),
            &mut keys,
        );

        for banned in BANNED {
            assert!(
                !keys.contains(banned),
                "the canonical configuration still carries the removed field {banned:?}"
            );
        }
    }

    #[test]
    fn no_sub_policy_carries_its_own_version_marker() {
        // §16.1 / DEC-6: exactly two revision markers exist on the wire, and neither of them is a
        // per-policy `version`. `abi_version` on the *resolved* record is the ABI marker itself.
        let mut keys = BTreeSet::new();
        all_keys(
            &serde_json::to_value(fully_populated_config()).unwrap(),
            &mut keys,
        );
        assert!(!keys.contains("version"));
        assert!(!keys.contains("abi_version"));

        for probe in [
            json!({ "version": 1, "critical_path_weight": 10 }),
            json!({ "version": 1 }),
        ] {
            assert!(serde_json::from_value::<SchedulerPolicy>(probe).is_err());
        }
        assert!(
            serde_json::from_value::<ContextPolicy>(json!({ "version": 1 })).is_err(),
            "context policy must not accept a per-policy version"
        );
    }

    #[test]
    fn there_is_no_scheduler_budget_setter_shaped_hole_in_the_live_union() {
        // §13.3 · `SetSchedulerBudget` becomes an explicit deadline command. Its only axis
        // (`max_wall_ms`) is boot-only here, and live changes go through `UpdateDeadline`.
        let mut patch_keys = BTreeSet::new();
        for patch in [
            LivePolicyPatch::ReplaceSignalPolicy(ReplaceSignalPolicy {
                policy: SignalPolicy {
                    queue_max: 8,
                    ttl_ms: Some(WireU64::new(1_000)),
                    deadline_escalation: Some(true),
                },
            }),
            LivePolicyPatch::ReplaceGovernancePolicy(ReplaceGovernancePolicy {
                policy: GovernancePolicy::default(),
            }),
            LivePolicyPatch::TightenResourceQuota(TightenResourceQuota {
                max_concurrent_subagents: Some(1),
                max_total_subagents: Some(2),
                max_spawn_depth: Some(1),
                max_workflow_nodes: Some(4),
            }),
            LivePolicyPatch::ReplaceRecoveryPolicy(ReplaceRecoveryPolicy {
                policy: RecoveryPolicy::default(),
            }),
        ] {
            all_keys(&serde_json::to_value(&patch).unwrap(), &mut patch_keys);
        }
        assert!(
            !patch_keys.contains("max_wall_ms"),
            "a wall-clock budget must not be reachable through a policy patch"
        );
    }

    // -----------------------------------------------------------------------------------------
    // setup-only / live split (§13.1, §13.2)
    // -----------------------------------------------------------------------------------------

    #[test]
    fn setup_only_configuration_is_unreachable_from_the_live_patch_union() {
        let setup_only = [
            "execution_policy",
            "scheduler_policy",
            "context_policy",
            "payload_policy",
            "kernel_limits",
            "memory_access",
            "memory_policy",
            "tool_catalog",
            "skill_catalog",
            "feature_policy",
            "host_effect_support",
            "budget_grant",
            "tool_dispatch_gate",
            "stable_core_tool_ids",
            "knowledge_budget_ppm",
            "prompt_budget",
            "binding_id",
        ];

        let mut patch_keys = BTreeSet::new();
        for patch in [
            LivePolicyPatch::ReplaceSignalPolicy(ReplaceSignalPolicy {
                policy: SignalPolicy {
                    queue_max: 8,
                    ttl_ms: None,
                    deadline_escalation: None,
                },
            }),
            LivePolicyPatch::ReplaceGovernancePolicy(ReplaceGovernancePolicy {
                policy: GovernancePolicy {
                    default_action: Some(PolicyAction::Deny),
                    rules: vec![PolicyRule {
                        tool_pattern: "*".to_string(),
                        action: PolicyAction::Deny,
                    }],
                    vetoed_tools: vec!["rm".to_string()],
                    rate_limits: vec![RateLimitSpec {
                        tool: "search".to_string(),
                        max_calls: 1,
                        window_ms: WireU64::new(1_000),
                    }],
                    constraints: Vec::new(),
                },
            }),
            LivePolicyPatch::TightenResourceQuota(TightenResourceQuota {
                max_concurrent_subagents: Some(1),
                max_total_subagents: Some(2),
                max_spawn_depth: Some(1),
                max_workflow_nodes: Some(4),
            }),
            LivePolicyPatch::ReplaceRecoveryPolicy(ReplaceRecoveryPolicy {
                policy: RecoveryPolicy {
                    provider_recovery_attempts: Some(1),
                    output_recovery_attempts: Some(1),
                    tail_bounds: None,
                },
            }),
        ] {
            all_keys(&serde_json::to_value(&patch).unwrap(), &mut patch_keys);
        }

        for key in setup_only {
            assert!(
                !patch_keys.contains(key),
                "{key:?} is setup-only but is reachable through LivePolicyPatch"
            );
        }
    }

    // -----------------------------------------------------------------------------------------
    // strictness (§7.1)
    // -----------------------------------------------------------------------------------------

    #[test]
    fn every_configuration_struct_rejects_unknown_fields() {
        assert!(
            serde_json::from_value::<OperationConfig>(json!({
                "host_effect_support": { "supported": [] },
                "tokenizer": "cl100k",
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<MemoryPolicy>(json!({ "memory_path": "/tmp/mem" })).is_err(),
            "memory_path moved to the host MemoryStore config and must not decode"
        );
        assert!(
            serde_json::from_value::<PayloadPolicy>(json!({ "spool_dir": "/tmp/.spool" })).is_err()
        );
        assert!(
            serde_json::from_value::<ExecutionPolicy>(json!({ "max_tokens": 1 })).is_err(),
            "the renamed context-window axis must not silently accept its legacy name"
        );
        assert!(serde_json::from_value::<KernelLimits>(json!({ "max_bytes": 1 })).is_err());
        assert!(
            serde_json::from_value::<HostEffectSupport>(json!({ "supported": [], "all": true }))
                .is_err()
        );
        assert!(
            serde_json::from_value::<HostEffectSupport>(json!({})).is_err(),
            "DEC-8 support declaration is mandatory, not defaulted"
        );
        assert!(
            serde_json::from_value::<OperationConfig>(json!({})).is_err(),
            "a config without host_effect_support is not a config"
        );
    }

    #[test]
    fn policy_ratios_are_fixed_point_not_floats() {
        assert!(
            serde_json::from_value::<ContextPolicy>(json!({ "knowledge_budget_ppm": 0.25 }))
                .is_err()
        );
        assert!(
            serde_json::from_value::<EntropyWatchPolicy>(json!({ "threshold_ppm": 0.65 })).is_err()
        );
        assert!(
            serde_json::from_value::<ContextPolicy>(json!({ "knowledge_budget_ppm": 250_000 }))
                .is_ok()
        );
        assert!(
            serde_json::from_value::<ContextPolicy>(json!({ "knowledge_budget_ppm": 1_000_001 }))
                .is_err(),
            "a ratio above 1.0 is not a ratio"
        );
    }

    #[test]
    fn sixty_four_bit_config_axes_travel_as_decimal_strings() {
        assert!(
            serde_json::from_value::<ExecutionPolicy>(json!({ "max_total_tokens": 1_000_000 }))
                .is_err()
        );
        assert!(
            serde_json::from_value::<ExecutionPolicy>(json!({ "max_total_tokens": "1000000" }))
                .is_ok()
        );
        assert!(
            serde_json::from_value::<BudgetGrant>(
                json!({ "reservation_id": "r", "tokens": 5_000 })
            )
            .is_err()
        );
    }

    #[test]
    fn the_effect_support_tag_set_is_closed() {
        assert!(
            serde_json::from_value::<HostEffectSupport>(
                json!({ "supported": ["spool_large_result"] })
            )
            .is_err(),
            "SpoolLargeResult is deleted; declaring support for it must not decode"
        );
        assert!(
            serde_json::from_value::<HostEffectSupport>(json!({ "supported": ["load_payload"] }))
                .is_ok()
        );
        assert_eq!(EffectKindTag::ALL.len(), 11);
    }

    // -----------------------------------------------------------------------------------------
    // resolution: no implicit defaults survive (§7.3)
    // -----------------------------------------------------------------------------------------

    #[test]
    fn resolution_removes_every_implicit_default() {
        let resolved = minimal_config().resolve(&defaults()).unwrap();
        let value = serde_json::to_value(&resolved).unwrap();

        // Nothing that stands for "ask the binary" is left: every knob has a concrete value.
        assert_eq!(value["execution_policy"]["max_turns"], json!(25));
        assert_eq!(
            value["context_policy"]["knowledge_budget_ppm"],
            json!(250_000)
        );
        assert_eq!(value["payload_policy"]["preview_bytes"], json!(2_048));
        assert_eq!(value["memory_policy"]["retrieval_top_k"], json!(5));
        assert_eq!(
            value["feature_policy"]["tool_dispatch_gate"],
            json!("exposed")
        );
        assert_eq!(value["abi_version"], json!(KERNEL_ABI_VERSION));

        // The resolved record round-trips, because that is what the genesis record stores.
        let text = serde_json::to_string(&resolved).unwrap();
        let back: ResolvedOperationConfig = serde_json::from_str(&text).unwrap();
        assert_eq!(back, resolved);
    }

    #[test]
    fn a_resolved_config_does_not_move_when_the_binary_defaults_move() {
        let resolved = minimal_config().resolve(&defaults()).unwrap();

        let mut newer = defaults();
        newer.baseline.execution_policy.max_turns = 999;
        newer.baseline.context_policy.knowledge_budget_ppm = ppm(1_000);

        // Re-decoding the stored record is unaffected; only a *fresh* resolve sees new defaults.
        let text = serde_json::to_string(&resolved).unwrap();
        let replayed: ResolvedOperationConfig = serde_json::from_str(&text).unwrap();
        assert_eq!(replayed.execution_policy.max_turns, 25);
        assert_eq!(
            minimal_config()
                .resolve(&newer)
                .unwrap()
                .execution_policy
                .max_turns,
            999
        );
    }

    #[test]
    fn a_fully_populated_config_resolves_to_exactly_what_it_declared() {
        let resolved = fully_populated_config().resolve(&defaults()).unwrap();
        assert_eq!(resolved.execution_policy.max_turns, 40);
        assert_eq!(resolved.execution_policy.repeat_fuse.terminate_after, 7);
        assert_eq!(resolved.scheduler_policy.token_cost_weight, 2);
        assert_eq!(resolved.payload_policy.inline_threshold_bytes, 32_768);
        assert_eq!(resolved.kernel_limits.collection_limits.tool_catalog, 256);
        assert_eq!(resolved.memory_policy.retrieval_top_k, 8);
        assert_eq!(resolved.tool_catalog.len(), 2);
        assert!(resolved.feature_policy.memory_enabled);
    }

    // -----------------------------------------------------------------------------------------
    // atomic cross-field validation (Task 5 verification)
    // -----------------------------------------------------------------------------------------

    #[test]
    fn one_illegal_field_rejects_the_whole_configure_and_changes_nothing() {
        let mut config = fully_populated_config();
        config
            .execution_policy
            .as_mut()
            .unwrap()
            .repeat_fuse
            .as_mut()
            .unwrap()
            .terminate_after = Some(2); // < deny_after (4)

        let before = config.clone();
        let rejection = config.resolve(&defaults()).expect_err("must reject");
        assert_eq!(rejection.kind, WireRejectionKind::PolicyViolation);
        assert!(rejection.message.contains("terminate_after"));
        // resolution owns no state: the input is untouched and no partial resolved value exists
        assert_eq!(config, before);
    }

    #[test]
    fn cross_field_relationships_are_all_enforced() {
        let cases: Vec<(&str, Box<dyn Fn(&mut OperationConfig)>, &str)> = vec![
            (
                "thresholds must strictly increase",
                Box::new(|config| {
                    config
                        .context_policy
                        .as_mut()
                        .unwrap()
                        .pressure_thresholds_ppm
                        .as_mut()
                        .unwrap()
                        .micro = ppm(600_000);
                }),
                "strictly increase",
            ),
            (
                "compression target must sit below snip",
                Box::new(|config| {
                    config
                        .context_policy
                        .as_mut()
                        .unwrap()
                        .target_after_compress_ppm = Some(ppm(750_000));
                }),
                "target_after_compress_ppm",
            ),
            (
                "prompt reserves cannot consume the window",
                Box::new(|config| {
                    config.context_policy.as_mut().unwrap().prompt_budget = Some(PromptBudget {
                        prompt_overhead_tokens: 200_000,
                        output_reserve_tokens: 1,
                        safety_margin_tokens: 0,
                    });
                }),
                "leaving nothing to render",
            ),
            (
                "preview cannot exceed the inline threshold",
                Box::new(|config| {
                    config.payload_policy.as_mut().unwrap().preview_bytes = Some(65_536);
                }),
                "preview_bytes",
            ),
            (
                "concurrent cap cannot exceed the cumulative cap",
                Box::new(|config| {
                    config
                        .resource_quota
                        .as_mut()
                        .unwrap()
                        .max_concurrent_subagents = Some(99);
                }),
                "max_concurrent_subagents",
            ),
            (
                "hysteresis cannot exceed the threshold",
                Box::new(|config| {
                    config
                        .execution_policy
                        .as_mut()
                        .unwrap()
                        .entropy_watch
                        .as_mut()
                        .unwrap()
                        .hysteresis_ppm = Some(ppm(900_000));
                }),
                "hysteresis_ppm",
            ),
            (
                "a skill cannot allow a tool the catalog never declared",
                Box::new(|config| {
                    config.skill_catalog[0].allowed_tools = vec!["undeclared".to_string()];
                }),
                "only ever narrow the catalog",
            ),
            (
                "a skill cannot exceed the capability-grants collection limit",
                Box::new(|config| {
                    config
                        .kernel_limits
                        .as_mut()
                        .unwrap()
                        .collection_limits
                        .as_mut()
                        .unwrap()
                        .capability_grants = Some(0);
                    config.skill_catalog[0].capability_grants =
                        vec![crate::types::capability::Capability {
                            id: crate::types::capability::CapabilityId("read-src".into()),
                            kind: crate::types::capability::CapabilityKind::Tool,
                            resource: crate::types::capability::ResourceSelector(
                                "/repo/src/**".into(),
                            ),
                            actions: crate::types::capability::ActionSet(
                                ["read".into()].into_iter().collect(),
                            ),
                            constraints: crate::types::capability::ConstraintSet::default(),
                            lease: None,
                            delegatable: false,
                            issuer: crate::types::capability::Principal("root".into()),
                        }];
                }),
                "capability grants",
            ),
            (
                "the exposure baseline cannot name an undeclared tool",
                Box::new(|config| {
                    config.feature_policy.as_mut().unwrap().stable_core_tool_ids =
                        Some(vec!["ghost".to_string()]);
                }),
                "tool catalog does not declare",
            ),
            (
                "memory cannot be enabled without a binding",
                Box::new(|config| {
                    config.memory_access = None;
                }),
                "no memory_access binding",
            ),
            (
                "a duplicate tool makes dispatch order-dependent",
                Box::new(|config| {
                    config.tool_catalog[1].name = "search".to_string();
                }),
                "twice",
            ),
        ];

        for (label, mutate, expected_fragment) in cases {
            let mut config = fully_populated_config();
            mutate(&mut config);
            let before = config.clone();
            let rejection = config
                .resolve(&defaults())
                .err()
                .unwrap_or_else(|| panic!("{label}: expected a rejection"));
            assert!(
                rejection.message.contains(expected_fragment),
                "{label}: message {:?} does not name the broken relationship {expected_fragment:?}",
                rejection.message
            );
            assert_eq!(config, before, "{label}: rejection mutated the input");
        }
    }

    #[test]
    fn cross_field_rejections_name_the_relationship_they_broke() {
        let mut config = fully_populated_config();
        config
            .context_policy
            .as_mut()
            .unwrap()
            .target_after_compress_ppm = Some(ppm(750_000));
        let rejection = config.resolve(&defaults()).expect_err("must reject");
        assert!(
            rejection.message.contains("target_after_compress_ppm")
                && rejection.message.contains("snip"),
            "unhelpful message: {}",
            rejection.message
        );
    }

    // -----------------------------------------------------------------------------------------
    // only-tighten (§7.3, §13.1)
    // -----------------------------------------------------------------------------------------

    #[test]
    fn operation_limits_may_only_tighten_the_bootstrap_ceiling() {
        let defaults = ConfigDefaults::new(KernelBootstrapLimits {
            absolute_max_input_bytes: 1_048_576,
            absolute_max_json_depth: 32,
            absolute_max_collection_entries: 1_024,
        });

        let tighter = OperationConfig {
            kernel_limits: Some(KernelLimits {
                max_input_bytes: Some(65_536),
                max_json_depth: Some(16),
                max_collection_entries: Some(256),
                collection_limits: None,
            }),
            ..minimal_config()
        };
        let resolved = tighter.resolve(&defaults).unwrap();
        assert_eq!(resolved.kernel_limits.max_input_bytes, 65_536);
        assert_eq!(resolved.kernel_limits.collection_limits.tool_catalog, 256);

        // equality with the bootstrap ceiling is "as tight as the ceiling", not a widening
        let at_ceiling = OperationConfig {
            kernel_limits: Some(KernelLimits {
                max_input_bytes: Some(1_048_576),
                max_json_depth: Some(32),
                max_collection_entries: Some(1_024),
                collection_limits: None,
            }),
            ..minimal_config()
        };
        assert!(at_ceiling.resolve(&defaults).is_ok());
    }

    #[test]
    fn widening_any_bootstrap_axis_is_rejected_with_the_direction_named() {
        let defaults = ConfigDefaults::new(KernelBootstrapLimits {
            absolute_max_input_bytes: 1_048_576,
            absolute_max_json_depth: 32,
            absolute_max_collection_entries: 1_024,
        });
        for limits in [
            KernelLimits {
                max_input_bytes: Some(2_097_152),
                ..KernelLimits::default()
            },
            KernelLimits {
                max_json_depth: Some(64),
                ..KernelLimits::default()
            },
            KernelLimits {
                max_collection_entries: Some(65_536),
                ..KernelLimits::default()
            },
        ] {
            let config = OperationConfig {
                kernel_limits: Some(limits),
                ..minimal_config()
            };
            let rejection = config.resolve(&defaults).expect_err("widening rejected");
            assert_eq!(rejection.kind, WireRejectionKind::PolicyViolation);
            assert!(
                rejection.message.contains("may only tighten"),
                "unexpected message: {}",
                rejection.message
            );
        }
    }

    #[test]
    fn a_named_collection_bound_may_only_tighten_the_resolved_entry_ceiling() {
        let defaults = defaults();
        let config = OperationConfig {
            kernel_limits: Some(KernelLimits {
                max_collection_entries: Some(128),
                collection_limits: Some(CollectionLimits {
                    tool_catalog: Some(512),
                    ..CollectionLimits::default()
                }),
                ..KernelLimits::default()
            }),
            ..minimal_config()
        };
        let rejection = config.resolve(&defaults).expect_err("must reject");
        assert!(rejection.message.contains("collection_limits.tool_catalog"));
    }

    #[test]
    fn per_collection_bounds_are_enforced_against_the_declared_catalog() {
        let mut config = fully_populated_config();
        config
            .kernel_limits
            .as_mut()
            .unwrap()
            .collection_limits
            .as_mut()
            .unwrap()
            .tool_catalog = Some(1);
        let rejection = config.resolve(&defaults()).expect_err("must reject");
        assert_eq!(rejection.kind, WireRejectionKind::CollectionTooLarge);
        assert!(rejection.message.contains("tool_catalog"));
    }

    // -----------------------------------------------------------------------------------------
    // DEC-8 · feature ↔ host_effect_support cross-validation
    // -----------------------------------------------------------------------------------------

    #[test]
    fn every_declared_capability_requires_its_effect_kind() {
        // The all-fields config switches on every producible capability, so dropping any one
        // required kind from the
        // support declaration must be refused — and the rejection must name the kind, because a
        // host reading it has to know which declaration to fix. Iterating `ALL` also means a new
        // effect kind cannot be added without deciding what switches it on. `MeasurePrompt` is a
        // reserved wire shape with no scheduler producer after SPC-013 A-00R, so it deliberately
        // imposes no host-support requirement.
        let full = fully_populated_config();
        full.resolve(&defaults())
            .expect("declaring every kind satisfies every trigger");

        for dropped in EffectKindTag::ALL
            .into_iter()
            .filter(|kind| *kind != EffectKindTag::MeasurePrompt)
        {
            let mut config = full.clone();
            config.host_effect_support = HostEffectSupport::new(
                EffectKindTag::ALL
                    .into_iter()
                    .filter(|kind| *kind != dropped),
            );
            let before = config.clone();
            let rejection = config.resolve(&defaults()).err().unwrap_or_else(|| {
                panic!("{dropped:?} is switched on by the all-fields config but was not required")
            });
            assert_eq!(rejection.kind, WireRejectionKind::PolicyViolation);
            assert!(
                rejection.message.contains(dropped.as_str()),
                "{dropped:?}: rejection does not name the missing kind: {}",
                rejection.message
            );
            assert_eq!(config, before, "{dropped:?}: rejection mutated the input");
        }
    }

    #[test]
    fn an_absent_capability_imposes_no_effect_requirement() {
        // Fail-closure at configure time keys on affirmative declarations, not on absences: an
        // operation that never says it will spawn is not obliged to declare spawn_tasks, because
        // the runtime DEC-8 gate already refuses to emit what was never declared.
        minimal_config()
            .resolve(&defaults())
            .expect("a minimal config needs only call_provider");

        // …but declaring a tool catalog *is* affirmative, and pulls in both tool effects.
        let with_tools = OperationConfig {
            tool_catalog: vec![ToolSchema {
                name: "search".to_string(),
                description: String::new(),
                parameters: BoundedJson::null(),
            }],
            ..minimal_config()
        };
        let rejection = with_tools.resolve(&defaults()).expect_err("must reject");
        assert!(rejection.message.contains("execute_tools"));

        let ok = OperationConfig {
            host_effect_support: HostEffectSupport::new([
                EffectKindTag::CallProvider,
                EffectKindTag::ExecuteTools,
                EffectKindTag::LoadPayload,
            ]),
            ..with_tools
        };
        ok.resolve(&defaults())
            .expect("execute_tools + load_payload satisfy a tool catalog");
    }

    // -----------------------------------------------------------------------------------------
    // §7.3 · the verification-contract skeleton (adjudication §5m item 4)
    // -----------------------------------------------------------------------------------------

    /// A config that declares tools, skills and the milestone effect, so a contract skeleton has
    /// something to reference and something to publish.
    fn contract_host_config(contracts: Vec<VerificationContract>) -> OperationConfig {
        OperationConfig {
            host_effect_support: HostEffectSupport::new([
                EffectKindTag::CallProvider,
                EffectKindTag::ExecuteTools,
                EffectKindTag::LoadPayload,
                EffectKindTag::EvaluateMilestone,
            ]),
            tool_catalog: vec![ToolSchema {
                name: "search".to_string(),
                description: String::new(),
                parameters: BoundedJson::null(),
            }],
            skill_catalog: vec![SkillMetadata {
                name: "research".to_string(),
                description: String::new(),
                when_to_use: None,
                allowed_tools: Vec::new(),
                capability_grants: Vec::new(),
                effort: None,
                estimated_tokens: None,
            }],
            verification_contracts: contracts,
            ..minimal_config()
        }
    }

    fn phase(phase_id: &str, unlocks: &[&str]) -> MilestonePhase {
        MilestonePhase {
            phase_id: phase_id.to_string(),
            unlocks: unlocks.iter().map(|id| (*id).to_string()).collect(),
        }
    }

    #[test]
    fn a_contract_skeleton_carries_phase_order_and_unlocks_and_nothing_else() {
        // The two facts core owns — the cascade order and what a pass mounts. Criteria, evidence,
        // verifier and the I/O that runs them stay host-side (§5.2).
        let config = contract_host_config(vec![VerificationContract {
            contract_id: "brief-quality-v1".to_string(),
            phases: vec![phase("collect", &["research"]), phase("write", &["search"])],
        }]);
        let resolved = config.resolve(&defaults()).expect("a legal skeleton");
        let contract = resolved
            .verification_contract("brief-quality-v1")
            .expect("resolution keeps the catalog addressable by id");
        assert_eq!(
            contract
                .phases
                .iter()
                .map(|p| p.phase_id.as_str())
                .collect::<Vec<_>>(),
            vec!["collect", "write"],
            "the cascade order is a kernel fact and must survive resolution verbatim"
        );
        assert!(resolved.verification_contract("nope").is_none());

        // the shape a fuller contract type would have carried is not expressible here
        let value = serde_json::to_value(&contract.phases[0]).unwrap();
        for host_owned in ["criteria", "required_evidence", "verifier", "retry_policy"] {
            assert!(
                value.get(host_owned).is_none(),
                "{host_owned} belongs to the host (§5.2)"
            );
        }
    }

    #[test]
    fn a_contract_catalog_with_a_duplicate_id_is_refused() {
        // `verification_contract_id` resolves by id; two contracts sharing one would resolve by
        // list order, i.e. silently.
        let config = contract_host_config(vec![
            VerificationContract {
                contract_id: "brief-quality-v1".to_string(),
                phases: vec![phase("collect", &[])],
            },
            VerificationContract {
                contract_id: "brief-quality-v1".to_string(),
                phases: vec![phase("write", &[])],
            },
        ]);
        let before = config.clone();
        let rejection = config.resolve(&defaults()).expect_err("must reject");
        assert_eq!(rejection.kind, WireRejectionKind::PolicyViolation);
        assert!(
            rejection.message.contains("brief-quality-v1") && rejection.message.contains("twice"),
            "{}",
            rejection.message
        );
        assert_eq!(config, before, "a rejection mutates nothing");
    }

    #[test]
    fn a_contract_with_a_duplicate_phase_id_is_refused() {
        // A verdict names its phase by id. Two phases sharing one means a verdict for the second
        // could advance the first.
        let config = contract_host_config(vec![VerificationContract {
            contract_id: "brief-quality-v1".to_string(),
            phases: vec![phase("collect", &[]), phase("collect", &["search"])],
        }]);
        let rejection = config.resolve(&defaults()).expect_err("must reject");
        assert_eq!(rejection.kind, WireRejectionKind::PolicyViolation);
        assert!(
            rejection.message.contains("collect"),
            "{}",
            rejection.message
        );
    }

    #[test]
    fn a_phase_cannot_unlock_a_capability_the_operation_never_declared() {
        // The capability directory at configure time is tool_catalog ∪ skill_catalog. Unlocking
        // anything else is a mount with nothing behind it — the same fail-closure
        // `stable_core_tool_ids` already gets, and for the same reason.
        let config = contract_host_config(vec![VerificationContract {
            contract_id: "brief-quality-v1".to_string(),
            phases: vec![phase("collect", &["deploy_to_prod"])],
        }]);
        let rejection = config.resolve(&defaults()).expect_err("must reject");
        assert_eq!(rejection.kind, WireRejectionKind::PolicyViolation);
        assert!(
            rejection.message.contains("deploy_to_prod"),
            "{}",
            rejection.message
        );

        // both directories count
        for declared in ["search", "research"] {
            contract_host_config(vec![VerificationContract {
                contract_id: "c".to_string(),
                phases: vec![phase("p", &[declared])],
            }])
            .resolve(&defaults())
            .unwrap_or_else(|e| panic!("{declared} is declared: {e}"));
        }
    }

    #[test]
    fn a_contract_with_no_phase_is_refused() {
        // It can never publish an `EvaluateMilestone`, so a spec pointing at it would be a gate
        // that silently is not there.
        let config = contract_host_config(vec![VerificationContract {
            contract_id: "brief-quality-v1".to_string(),
            phases: Vec::new(),
        }]);
        let rejection = config.resolve(&defaults()).expect_err("must reject");
        assert_eq!(rejection.kind, WireRejectionKind::PolicyViolation);
        assert!(
            rejection.message.contains("no phases"),
            "{}",
            rejection.message
        );

        for empty_id in [
            VerificationContract {
                contract_id: String::new(),
                phases: vec![phase("p", &[])],
            },
            VerificationContract {
                contract_id: "c".to_string(),
                phases: vec![phase("", &[])],
            },
        ] {
            assert!(
                contract_host_config(vec![empty_id])
                    .resolve(&defaults())
                    .is_err(),
                "an empty id names nothing"
            );
        }
    }

    #[test]
    fn a_contract_catalog_still_requires_the_milestone_effect() {
        // DEC-8's configure-time twin: declaring a contract is an affirmative statement that the
        // operation will ask for a verdict.
        let mut config = contract_host_config(vec![VerificationContract {
            contract_id: "brief-quality-v1".to_string(),
            phases: vec![phase("collect", &[])],
        }]);
        config.host_effect_support = HostEffectSupport::new([
            EffectKindTag::CallProvider,
            EffectKindTag::ExecuteTools,
            EffectKindTag::LoadPayload,
        ]);
        let rejection = config.resolve(&defaults()).expect_err("must reject");
        assert!(
            rejection.message.contains("evaluate_milestone"),
            "{}",
            rejection.message
        );
    }

    #[test]
    fn a_tool_catalog_also_requires_the_payload_page_in_path() {
        // §7.10: a result above the inline threshold becomes External. A host that can run tools
        // but cannot load payloads back produces results nothing can ever read.
        let config = OperationConfig {
            tool_catalog: vec![ToolSchema {
                name: "search".to_string(),
                description: String::new(),
                parameters: BoundedJson::null(),
            }],
            host_effect_support: HostEffectSupport::new([
                EffectKindTag::CallProvider,
                EffectKindTag::ExecuteTools,
            ]),
            ..OperationConfig::default()
        };
        let rejection = config.resolve(&defaults()).expect_err("must reject");
        assert!(rejection.message.contains("load_payload"));
    }

    #[test]
    fn spawn_capacity_obliges_the_host_to_be_able_to_stop_children() {
        // Starting children you cannot preempt leaks them past cancellation and budget
        // exhaustion, so the two kinds are required together or not at all.
        let config = OperationConfig {
            resource_quota: Some(ResourceQuota {
                max_total_subagents: Some(4),
                ..ResourceQuota::default()
            }),
            host_effect_support: HostEffectSupport::new([
                EffectKindTag::CallProvider,
                EffectKindTag::SpawnTasks,
            ]),
            ..OperationConfig::default()
        };
        let rejection = config.resolve(&defaults()).expect_err("must reject");
        assert!(rejection.message.contains("preempt_tasks"));

        // a quota that caps spawning at zero declares no capacity, so neither kind is required
        let no_capacity = OperationConfig {
            resource_quota: Some(ResourceQuota {
                max_total_subagents: Some(0),
                ..ResourceQuota::default()
            }),
            ..minimal_config()
        };
        no_capacity
            .resolve(&defaults())
            .expect("a zero cap is not a declaration of spawn capacity");
    }

    #[test]
    fn an_ask_user_gate_requires_somewhere_to_ask() {
        for governance in [
            GovernancePolicy {
                default_action: Some(PolicyAction::AskUser),
                ..GovernancePolicy::default()
            },
            GovernancePolicy {
                default_action: Some(PolicyAction::Allow),
                rules: vec![PolicyRule {
                    tool_pattern: "shell.*".to_string(),
                    action: PolicyAction::AskUser,
                }],
                ..GovernancePolicy::default()
            },
        ] {
            let config = OperationConfig {
                governance_policy: Some(governance),
                ..minimal_config()
            };
            let rejection = config.resolve(&defaults()).expect_err("must reject");
            assert!(rejection.message.contains("request_approval"));
        }
    }

    #[test]
    fn memory_needs_both_directions_not_either_one() {
        // The pre-Task-7 check accepted a config declaring *either* memory effect. A writable,
        // readable memory plane needs both, and "either" let half a plane through.
        for missing in [EffectKindTag::PersistMemory, EffectKindTag::QueryMemory] {
            let config = OperationConfig {
                memory_access: Some(MemoryAccessBinding {
                    binding_id: MemoryBindingId::new("mem-1").unwrap(),
                    capabilities: MemoryCapabilities {
                        read: true,
                        write: true,
                    },
                }),
                feature_policy: Some(FeaturePolicy {
                    memory_enabled: Some(true),
                    ..FeaturePolicy::default()
                }),
                host_effect_support: HostEffectSupport::new(
                    [
                        EffectKindTag::CallProvider,
                        EffectKindTag::PersistMemory,
                        EffectKindTag::QueryMemory,
                    ]
                    .into_iter()
                    .filter(|kind| *kind != missing),
                ),
                ..OperationConfig::default()
            };
            let rejection = config.resolve(&defaults()).expect_err("must reject");
            assert!(rejection.message.contains(missing.as_str()));
        }
    }

    #[test]
    fn a_read_only_memory_binding_does_not_require_the_write_path() {
        // The binding's own capabilities are finer-grained than the feature switch: a read-only
        // plane never persists, so demanding persist_memory would be a false positive.
        let config = OperationConfig {
            memory_access: Some(MemoryAccessBinding {
                binding_id: MemoryBindingId::new("mem-ro").unwrap(),
                capabilities: MemoryCapabilities {
                    read: true,
                    write: false,
                },
            }),
            host_effect_support: HostEffectSupport::new([
                EffectKindTag::CallProvider,
                EffectKindTag::QueryMemory,
            ]),
            ..OperationConfig::default()
        };
        config
            .resolve(&defaults())
            .expect("a read-only binding needs only query_memory");
    }

    // -----------------------------------------------------------------------------------------
    // rejection taxonomy
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_broken_encoder_and_a_refused_configuration_are_different_rejections() {
        // Same field, two failure modes that need different host handling: the first never became
        // a value at all, the second is a coherent value the kernel declines to adopt. Collapsing
        // them onto one kind is what made "re-read and rebase" indistinguishable from "your
        // serializer is wrong".
        let scalar_error = serde_json::from_value::<ContextPolicy>(json!({
            "knowledge_budget_ppm": 0.25,
        }))
        .expect_err("a float ratio never becomes a Ppm");
        assert!(scalar_error.to_string().contains(SCALAR_ERROR_MARKER));

        let mut config = fully_populated_config();
        config.context_policy.as_mut().unwrap().knowledge_budget_ppm = Some(ppm(990_000));
        let policy_error = config
            .resolve(&defaults())
            .expect_err("a knowledge budget that crowds out carryover is refused");
        assert_eq!(policy_error.kind, WireRejectionKind::PolicyViolation);
        assert_eq!(policy_error.kind.as_str(), "policy_violation");
    }

    #[test]
    fn every_resolution_rejection_is_a_policy_violation_or_a_bound() {
        // The decode taxonomy stops at the boundary; nothing past it may claim to be a scalar,
        // unknown-field or missing-field fault, because by then the document already decoded.
        let mut cases: Vec<OperationConfig> = Vec::new();

        let mut widen = minimal_config();
        widen.kernel_limits = Some(KernelLimits {
            max_json_depth: Some(u16::MAX),
            ..KernelLimits::default()
        });
        cases.push(widen);

        let mut ladder = fully_populated_config();
        ladder
            .context_policy
            .as_mut()
            .unwrap()
            .preserve_recent_turns = Some(0);
        cases.push(ladder);

        let mut catalog = fully_populated_config();
        catalog
            .kernel_limits
            .as_mut()
            .unwrap()
            .collection_limits
            .as_mut()
            .unwrap()
            .tool_catalog = Some(1);
        cases.push(catalog);

        let mut quota = fully_populated_config();
        quota.resource_quota.as_mut().unwrap().max_spawn_depth = Some(0);
        cases.push(quota);

        for config in cases {
            let kind = config.resolve(&defaults()).expect_err("must reject").kind;
            assert!(
                matches!(
                    kind,
                    WireRejectionKind::PolicyViolation | WireRejectionKind::CollectionTooLarge
                ),
                "resolution produced the decode-stage kind {kind:?}"
            );
        }
    }

    // -----------------------------------------------------------------------------------------
    // lifecycle (§13.1 · setup-only means "once, before execution exists")
    // -----------------------------------------------------------------------------------------

    #[test]
    fn configuration_is_admissible_only_before_the_operation_starts() {
        use crate::runtime::kernel::wire::envelope::{
            ConfigureOperation, KernelInput, OperationLifecycle,
        };

        let configure = KernelInput::ConfigureOperation(ConfigureOperation {
            config: minimal_config(),
        });
        assert_eq!(
            configure.admissible_lifecycles(),
            &[OperationLifecycle::Created],
            "boot configuration is admissible exactly once, before any execution exists"
        );

        // The live control plane, by contrast, is admissible while the operation runs — which is
        // precisely why nothing setup-only may travel through it.
        let control =
            KernelInput::HostControl(crate::runtime::kernel::wire::envelope::HostControl {
                command: HostCommand::ForceCompact(
                    crate::runtime::kernel::wire::command::ForceCompactCommand {},
                ),
            });
        assert!(
            control
                .admissible_lifecycles()
                .contains(&OperationLifecycle::Running)
        );
        assert!(
            !configure
                .admissible_lifecycles()
                .contains(&OperationLifecycle::Running),
            "a second ConfigureOperation against a running operation is an illegal lifecycle \
             mutation, not a live policy change"
        );
    }

    // -----------------------------------------------------------------------------------------
    // fixtures
    // -----------------------------------------------------------------------------------------

    #[test]
    fn configure_input_goldens_round_trip_through_the_typed_config() {
        let fixtures = fixtures_with_prefix("input_configure_");
        assert!(
            fixtures.len() >= 2,
            "need a minimal and a full configure golden, got {}",
            fixtures.len()
        );

        let mut saw_minimal = false;
        let mut saw_full = false;
        for (name, fixture) in fixtures {
            let config: OperationConfig =
                serde_json::from_value(fixture["input"]["config"].clone())
                    .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(
                serde_json::to_value(&config).unwrap(),
                fixture["input"]["config"],
                "{name}: config round-trip changed the document"
            );
            config
                .resolve(&defaults())
                .unwrap_or_else(|e| panic!("{name}: golden must resolve: {e}"));

            let field_count = fixture["input"]["config"].as_object().unwrap().len();
            saw_minimal |= field_count <= 2;
            saw_full |= field_count >= 15;
        }
        assert!(saw_minimal, "no minimal configure golden");
        assert!(saw_full, "no all-fields configure golden");
    }

    #[test]
    fn the_resolved_golden_matches_what_resolution_produces() {
        // The genesis record stores this shape. Freezing it is what makes "a replay never
        // re-applies a newer binary's defaults" a checkable claim rather than an intention.
        let fixture = fixtures_with_prefix("golden_config_resolved")
            .into_iter()
            .next()
            .map(|(_, value)| value)
            .expect("a resolved-config golden must exist");

        let config: OperationConfig =
            serde_json::from_value(fixture["config"].clone()).expect("golden config decodes");
        let resolved = config.resolve(&defaults()).expect("golden config resolves");
        assert_eq!(
            serde_json::to_value(&resolved).unwrap(),
            fixture["resolved"],
            "resolution drifted from the frozen golden"
        );
    }

    #[test]
    fn config_rejection_fixtures_fail_closed_with_the_declared_kind() {
        // Decode-stage rejections are whole envelopes (they also feed the §7.1 harness);
        // resolution-stage rejections carry the bare config, because they decode successfully
        // and only fail once the cross-field rules run.
        let decode_stage = fixtures_with_prefix("reject_config_");
        assert!(
            decode_stage.len() >= 4,
            "too few decode-stage config rejections"
        );
        for (name, fixture) in &decode_stage {
            let expected = fixture["expect"].as_str().expect("expect");
            let config = fixture["envelope"]["input"]["config"].clone();
            let error = serde_json::from_value::<OperationConfig>(config)
                .expect_err(&format!("{name}: expected a decode rejection"));
            let message = error.to_string();
            let kind = if message.contains(SCALAR_ERROR_MARKER) {
                "invalid_scalar"
            } else if message.contains("unknown field") {
                "unknown_field"
            } else if message.contains("unknown variant") {
                "unknown_variant"
            } else if message.contains("missing field") {
                "missing_field"
            } else {
                "type_mismatch"
            };
            assert_eq!(kind, expected, "{name}: wrong kind ({message})");
        }

        let resolution_stage: Vec<_> = fixtures_with_prefix("golden_config_reject_");
        assert!(
            resolution_stage.len() >= 4,
            "too few resolution-stage config rejections"
        );
        let mut kinds = BTreeSet::new();
        for (name, fixture) in &resolution_stage {
            let expected = fixture["expect"].as_str().expect("expect");
            let config: OperationConfig = serde_json::from_value(fixture["config"].clone())
                .unwrap_or_else(|e| panic!("{name}: a resolution-stage fixture must decode: {e}"));
            let defaults = fixture
                .get("bootstrap_limits")
                .map(|limits| ConfigDefaults::new(serde_json::from_value(limits.clone()).unwrap()))
                .unwrap_or_default();
            let rejection = config
                .resolve(&defaults)
                .map(|ok| panic!("{name}: expected a rejection, resolved {ok:?}"))
                .unwrap_err();
            assert_eq!(
                rejection.kind.as_str(),
                expected,
                "{name}: {}",
                rejection.message
            );
            kinds.insert(expected.to_string());
        }
        assert!(kinds.contains("policy_violation"));
        assert!(kinds.contains("collection_too_large"));
    }

    #[test]
    fn config_fixtures_never_carry_host_owned_facts() {
        const BANNED: [&str; 7] = [
            "memory_path",
            "spool_dir",
            "tokenizer",
            "host_effect_retry_attempts",
            "session_id",
            "api_key",
            "endpoint",
        ];
        for prefix in ["input_configure_", "golden_config_"] {
            for (name, fixture) in fixtures_with_prefix(prefix) {
                let mut keys = BTreeSet::new();
                all_keys(&fixture, &mut keys);
                for banned in BANNED {
                    assert!(
                        !keys.contains(banned),
                        "{name}: configuration fixture carries the host-owned fact {banned:?}"
                    );
                }
            }
        }
    }
}
