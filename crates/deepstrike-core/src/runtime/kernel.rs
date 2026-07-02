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
use crate::runtime::session::RollbackReason;
use crate::scheduler::policy::LoopPolicy;
use crate::scheduler::state_machine::{LoopAction, LoopEvent, LoopStateMachine};
use crate::types::agent::AgentRunSpec;
use crate::types::capability::{CapabilityCommand, CapabilityDescriptor, CapabilityKind};
use crate::types::message::{Message, ToolCall, ToolResult, ToolSchema};
use crate::types::milestone::{MilestoneCheckResult, MilestoneContract};
use crate::types::result::{LoopResult, SubAgentResult};
use crate::types::signal::RuntimeSignal;
use crate::types::skill::SkillMetadata;
use crate::types::task::RuntimeTask;

pub const KERNEL_ABI_VERSION: u32 = 1;

/// Serializable permission action for the governance ABI.
/// Mirrors [`crate::governance::permission::PermissionAction`] without coupling
/// the wire format to the internal type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyAction {
    Allow,
    Deny,
    AskUser,
}

impl From<PolicyAction> for crate::governance::permission::PermissionAction {
    fn from(action: PolicyAction) -> Self {
        match action {
            PolicyAction::Allow => Self::Allow,
            PolicyAction::Deny => Self::Deny,
            PolicyAction::AskUser => Self::AskUser,
        }
    }
}

/// One permission rule for the governance ABI: glob `tool_pattern` → action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyRule {
    pub tool_pattern: String,
    pub action: PolicyAction,
}

/// Per-tool rate limit for the governance ABI.
/// Maps to [`crate::governance::rate_limit::RateLimit`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitSpec {
    pub tool: String,
    pub max_calls: u32,
    pub window_ms: u64,
}

/// Parameter constraint for the governance ABI.
/// Maps to [`crate::governance::constraint::ConstraintRule`] (structural rules only;
/// pattern/predicate matching stays in the SDK via `VetoCheck`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConstraintSpec {
    /// Parameter must be present and non-null.
    Required { tool: String, path: String },
    /// Parameter value must be one of `values`.
    Enum {
        tool: String,
        path: String,
        values: Vec<String>,
    },
    /// Numeric parameter must fall within `[min, max]`.
    Range {
        tool: String,
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
    },
}

fn default_signal_queue_size() -> u32 {
    64
}

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

/// K2: the governance sub-bundle of [`RunConfig`] — the same five fields as the `LoadGovernancePolicy`
/// event, grouped so a run's whole governance posture travels as one value.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GovernanceConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_action: Option<PolicyAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<PolicyRule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vetoed_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rate_limits: Vec<RateLimitSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<ConstraintSpec>,
}

/// K2: a bundle of run-setup configuration carried by the [`KernelInputEvent::ConfigureRun`] event.
/// Each field maps 1:1 to a granular `Set*` / `Load*` event; `None`/absent leaves that aspect untouched.
/// This is the host-side analogue of the SDK's `applyKernelPolicies` — one event for the whole setup.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolSchema>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_skills: Option<Vec<SkillMetadata>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stable_core_tools: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_tool_enabled: Option<bool>,
    /// Present (any value) ⇒ reset the token engine to the char-approx estimator (see `SetTokenizer`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokenizer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub governance: Option<GovernanceConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention_max_queue_size: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduler_max_wall_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_quota: Option<crate::governance::quota::ResourceQuota>,
    /// L1 (RunGroup): cumulative tokens already spent by *other* members of this run's governance
    /// domain, seeded at boot so the run-level token cap (`max_total_tokens`) is enforced across the
    /// whole group, not per-vehicle. `None`/0 ⇒ no group (N=1) ⇒ pre-L1 per-kernel behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_tokens_base: Option<u64>,
    /// L1 (RunGroup): sub-agents already spawned by *other* members of this run's governance domain,
    /// seeded at boot so `ResourceQuota::max_total_subagents` is enforced across the whole group.
    /// `None`/0 ⇒ no group (N=1) ⇒ pre-L1 per-vehicle behavior.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_spawns_base: Option<u32>,
    /// O6: repeat-fuse thresholds (see `SetRepeatFuse`). Absent ⇒ kernel defaults
    /// (enabled, deny_after=5, terminate_after=8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat_fuse: Option<crate::governance::repeat_fuse::RepeatFuseConfig>,
    /// O4: enable/disable the turn-end criteria gate. Absent ⇒ enabled (kernel default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub criteria_gate: Option<bool>,
    /// K2: max share of `max_tokens` the knowledge partition may occupy (see
    /// `ContextConfig::knowledge_budget_ratio`). Absent ⇒ kernel default (0.25); `0.0` disables.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub knowledge_budget_ratio: Option<f64>,
}

/// Build a [`GovernancePipeline`](crate::governance::pipeline::GovernancePipeline) from the ABI policy
/// fields. Shared by the `LoadGovernancePolicy` event and the `ConfigureRun` bundle so the two can never
/// drift in how they interpret rules / vetoes / rate-limits / constraints.
pub(crate) fn build_governance_pipeline(
    default_action: Option<PolicyAction>,
    rules: Vec<PolicyRule>,
    vetoed_tools: Vec<String>,
    rate_limits: Vec<RateLimitSpec>,
    constraints: Vec<ConstraintSpec>,
) -> crate::governance::pipeline::GovernancePipeline {
    use crate::governance::constraint::{ConstraintRule, ParamConstraint};
    use crate::governance::permission::PermissionRule;
    use crate::governance::rate_limit::RateLimit;
    let default = default_action.unwrap_or(PolicyAction::Allow).into();
    let mut pipeline = crate::governance::pipeline::GovernancePipeline::new(default);
    for rule in rules {
        pipeline.permission.add_rule(PermissionRule {
            tool_pattern: rule.tool_pattern.into(),
            action: rule.action.into(),
        });
    }
    for tool in vetoed_tools {
        pipeline.veto.block_tool(tool);
    }
    for rl in rate_limits {
        pipeline.rate_limiter.set_limit(
            rl.tool,
            RateLimit {
                max_calls: rl.max_calls,
                window_ms: rl.window_ms,
            },
        );
    }
    for c in constraints {
        let (tool_name, param_path, rule) = match c {
            ConstraintSpec::Required { tool, path } => (tool, path, ConstraintRule::Required),
            ConstraintSpec::Enum { tool, path, values } => (tool, path, ConstraintRule::Enum(values)),
            ConstraintSpec::Range { tool, path, min, max } => {
                (tool, path, ConstraintRule::Range { min, max })
            }
        };
        pipeline.constraints.add(ParamConstraint {
            tool_name,
            param_path,
            rule,
        });
    }
    pipeline
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
    /// P1-B tool gating: the model loaded a skill (`name`). The SDK emits this when it resolves a
    /// `skill` tool call. The kernel records it in the active-skill set and resolves the skill's
    /// `allowed_tools` from the catalog to narrow the toolset on subsequent turns.
    SkillActivated {
        name: String,
    },
    /// P1-B/D: configure the stable-core tool ids (always exposed under skill gating). Set once by
    /// the SDK; empty/absent ⇒ skills narrow to exactly their declared tools + meta-tools.
    SetStableCoreTools {
        tool_ids: Vec<String>,
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
    AddKnowledgeMessage {
        content: String,
        tokens: u32,
        /// K1: entry identity. `Some` ⇒ upsert semantics (same key replaces at the next
        /// boundary); `None` ⇒ legacy unkeyed append. Additive — old logs replay unchanged.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
        /// K1: host-pinned entries are exempt from the K2 budget sweep.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        pinned: bool,
    },
    /// K1: mark a keyed knowledge entry for removal at the next compaction/renewal boundary.
    /// Errs-open: unknown key is a no-op.
    RemoveKnowledge {
        key: String,
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
    /// Install a governance policy. Once loaded, every model-proposed tool call
    /// is evaluated in-kernel before execution. Omitting this event leaves the
    /// gate disabled (pre-governance behavior).
    LoadGovernancePolicy {
        #[serde(default)]
        default_action: Option<PolicyAction>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        rules: Vec<PolicyRule>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        vetoed_tools: Vec<String>,
        // COMPAT(gov-abi-additive): rate_limits/constraints are additive fields with
        // serde(default) so older SDKs that omit them still deserialize. Safe to keep.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        rate_limits: Vec<RateLimitSpec>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        constraints: Vec<ConstraintSpec>,
    },
    /// Override the default in-kernel signal router queue size (default 64).
    /// The router is always active; this only adjusts capacity.
    SetAttentionPolicy {
        #[serde(default = "default_signal_queue_size")]
        max_queue_size: u32,
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
    /// K2: apply a bundle of run-setup configuration in a single event — the consolidation of the
    /// ~10 discrete `Set*` / `Load*` config events the SDK used to fire one-by-one before `StartRun`.
    /// Every field is optional; an absent field leaves that aspect untouched. The granular events
    /// remain for runtime mutation (a skill mount changing tools, a mid-run budget change). ABI-additive.
    ConfigureRun {
        config: RunConfig,
    },
    CapabilityCommand {
        command: CapabilityCommand,
    },
    Resume {
        // COMPAT(sched-resume-generic): old SDKs send `{kind:"resume"}` with no
        // fields — serde(default) deserialises to empty vecs. Change to required
        // once all SDKs supply approved/denied explicitly.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        approved_calls: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        denied_calls: Vec<String>,
    },
    /// O4: enable/disable the turn-end criteria gate (default enabled; no-op for runs without
    /// criteria). Additive ABI.
    /// K2: set the knowledge-budget ratio at runtime (granular sibling of
    /// `RunConfig::knowledge_budget_ratio`). `0.0` disables the cap.
    SetKnowledgeBudget {
        ratio: f64,
    },
    SetCriteriaGate {
        enabled: bool,
    },
    /// O6: tune or disable the repeat fuse (defaults: enabled, deny_after=5, terminate_after=8).
    /// Each field is optional — an absent field keeps the current value. Additive ABI.
    SetRepeatFuse {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enabled: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deny_after: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        terminate_after: Option<u32>,
    },
    /// Adjust the wall-clock budget at runtime (e.g. to extend or set a deadline
    /// after a run has already started). Additive: omit to keep the value from
    /// `LoopPolicy` passed at construction.
    SetSchedulerBudget {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_wall_ms: Option<u64>,
    },
    /// M2 资源配额: install a declarative [`crate::governance::quota::ResourceQuota`] at the
    /// single syscall trap. Like governance/attention/scheduler config, quotas flow in through
    /// the versioned JSON event ABI (replayable, session-loggable) rather than a side-channel
    /// setter — sending it is opt-in, and omitting it preserves the pre-M2 unconditional `Allow`
    /// for spawn / memory-write syscalls.
    SetResourceQuota {
        quota: crate::governance::quota::ResourceQuota,
    },
    ProviderResult {
        message: Message,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        observed_input_tokens: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        observed_output_tokens: Option<u32>,
        // COMPAT(gov-clock): now_ms is optional so SDKs that don't drive the in-kernel
        // governance gate need not supply a clock. When absent, the rate limiter runs
        // on a 0 clock (effectively unlimited). Can become required once all SDKs feed time.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        now_ms: Option<u64>,
        /// Provider stop_reason for this response — `max_tokens` (Anthropic) / `length` (OpenAI)
        /// signal an output-cap truncation, which drives the kernel's max-output-tokens recovery.
        /// Additive: omitted by providers/SDKs that don't report it (no-op recovery).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stop_reason: Option<String>,
    },
    ToolResults {
        results: Vec<ToolResult>,
    },
    /// Reactive recovery entry point: the SDK's provider stream failed. The kernel classifies the
    /// error (context-overflow vs other) and runs the bounded compact-and-retry recovery ladder,
    /// returning `CallProvider` to retry with a freshly compacted context or `Done` to terminate.
    /// The runners forward the raw provider error text and dispatch the result, instead of each
    /// owning the classify + compact + retry + give-up policy. Additive ABI: a brand-new variant,
    /// byte-identical on the wire for SDKs that never send it.
    ProviderError {
        message: String,
    },
    Signal {
        signal: RuntimeSignal,
    },
    MilestoneResult {
        result: MilestoneCheckResult,
    },
    /// Spawn a sub-agent: registers/updates the kernel process table.
    SpawnSubAgent {
        spec: AgentRunSpec,
        parent_session_id: String,
    },
    /// W0-ABI: load a workflow DAG and spawn its first gated batch. The kernel drives the DAG;
    /// each node spawn passes the syscall trap and is reported via `workflow_batch_spawned`.
    /// Completions feed back through `SubAgentCompleted` (reused); finish emits
    /// `workflow_completed`.
    LoadWorkflow {
        spec: crate::orchestration::workflow::WorkflowSpec,
        parent_session_id: String,
        /// W0-ABI resume: node agent-ids already completed (recovered from the log). Empty = fresh.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        resumed_completed: Vec<String>,
        /// R3-1 resume: the runtime `submit_workflow_nodes` batches (in order) recovered from the log,
        /// re-applied before completions so dynamically-appended nodes are reconstructed. Additive:
        /// empty for a fresh run or a resume without dynamic submissions.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        resumed_submissions: Vec<Vec<crate::orchestration::workflow::WorkflowNode>>,
    },
    /// Feed a completed sub-agent result back into the parent loop.
    SubAgentCompleted {
        result: SubAgentResult,
    },
    /// R3-1: append nodes to the in-flight workflow DAG at runtime (dynamic fan-out /
    /// loop-until-done). Sent by the SDK while the submitting node is still running — the appended
    /// nodes spawn on the next gated drive. No-op if no workflow is active. Additive ABI: a brand-new
    /// event variant, so existing SDKs that never send it are byte-identical on the wire.
    SubmitWorkflowNodes {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        nodes: Vec<crate::orchestration::workflow::WorkflowNode>,
        /// G1: the agent id of the node that requested this submission. When it names a quarantined
        /// node, the kernel coerces every submitted node to quarantined (no privilege escalation
        /// across the trust boundary). Additive: omitted by older SDKs → `None` → no coercion.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        submitter_agent_id: Option<String>,
    },
    /// M5/G1: an agent authors a whole `WorkflowSpec` (the article's "model writes its own harness").
    /// The agent-reachable analogue of the host-only `LoadWorkflow`: **bootstraps** the DAG when no
    /// workflow is active, else **flattens** the spec's nodes onto the running DAG (bootstrap-or-flatten,
    /// one kernel / one quota — never a workflow stack). Gated by `Syscall::LoadWorkflow`. Additive ABI:
    /// a brand-new variant, byte-identical on the wire for SDKs that never send it.
    SubmitWorkflow {
        spec: crate::orchestration::workflow::WorkflowSpec,
        /// Used only on bootstrap (no workflow active) to seed child session ids; ignored on flatten.
        #[serde(default)]
        parent_session_id: String,
        /// G1: the authoring node's agent id (flatten case) — a quarantined author's nodes are coerced
        /// quarantined. Additive: omitted (top-level bootstrap) → `None` → the run's own trust applies.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        submitter_agent_id: Option<String>,
    },
    /// Feed long-term memory entries into the knowledge partition (page-in).
    /// SDK performs retrieval I/O; kernel only applies the result.
    PageIn {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        entries: Vec<crate::mm::PageInEntry>,
    },
    /// Configure long-term memory management policy (Phase 7). Opt-in: installing the policy makes
    /// `validation_enabled`, `retrieval_top_k`, and the optional size/name overrides authoritative.
    SetMemoryPolicy {
        #[serde(default)]
        memory_path: String,
        #[serde(default = "default_stale_days")]
        stale_warning_days: u32,
        #[serde(default = "default_top_k")]
        retrieval_top_k: usize,
        #[serde(default = "default_validation_enabled")]
        validation_enabled: bool,
        /// Override the validation content-size limit (bytes). Omit to keep the kernel default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_content_bytes: Option<u32>,
        /// Override the validation name-length limit. Omit to keep the kernel default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_name_length: Option<usize>,
    },
    /// Write a long-term memory entry (SDK background agent calls this).
    WriteMemory {
        memory: crate::mm::memory::MemoryWriteRequest,
    },
    /// Query long-term memory for context (kernel calls this; SDK responds asynchronously).
    QueryMemory {
        query: crate::mm::memory::MemoryQuery,
    },
    /// Feed memory selection results back after `QueryMemory` (SDK → kernel acknowledgment).
    MemoryRetrievalResult {
        retrieval: crate::mm::memory::MemoryRetrieval,
    },
    Timeout,
}

fn default_stale_days() -> u32 { 2 }
fn default_top_k() -> usize { 5 }
fn default_validation_enabled() -> bool { true }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KernelStep {
    pub version: u32,
    pub actions: Vec<KernelAction>,
    pub observations: Vec<KernelObservation>,
}

impl KernelStep {
    fn empty(observations: Vec<KernelObservation>) -> Self {
        Self {
            version: KERNEL_ABI_VERSION,
            actions: Vec::new(),
            observations,
        }
    }

    fn single(action: LoopAction, observations: Vec<KernelObservation>) -> Self {
        Self {
            version: KERNEL_ABI_VERSION,
            actions: vec![action.into()],
            observations,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        verifier: Option<crate::types::milestone::MilestoneVerifier>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        required_evidence: Vec<String>,
    },
    Done {
        result: LoopResult,
    },
}

impl From<LoopAction> for KernelAction {
    fn from(action: LoopAction) -> Self {
        match action {
            LoopAction::AwaitingResume => {
                panic!("AwaitingResume must not be converted to KernelAction")
            }
            LoopAction::CallLLM { context, tools } => Self::CallProvider { context, tools },
            LoopAction::ExecuteTools { calls } => Self::ExecuteTool { calls },
            LoopAction::EvaluateMilestone {
                phase_id,
                criteria,
                verifier,
                required_evidence,
            } => Self::EvaluateMilestone {
                phase_id,
                criteria,
                verifier,
                required_evidence,
            },
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
        /// W1-1 cache-awareness: the message index at which this compression invalidated the
        /// prompt cache prefix (if any). `None` = prefix-safe. SDK/telemetry can use this to
        /// quantify "tokens saved vs cache rebuild cost". Additive ABI field with default.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        invalidates_prefix_at: Option<usize>,
    },
    Renewed {
        sprint: u32,
    },
    /// K1: a boundary sweep of the knowledge partition applied deferred upserts and/or dropped
    /// marked entries. `removed_keys` lists keyed removals (unkeyed drops count only in
    /// `tokens_freed`); an upsert-only sweep has empty `removed_keys`.
    KnowledgeSwept {
        turn: u32,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        removed_keys: Vec<String>,
        tokens_freed: u32,
    },
    /// K2: the knowledge partition exceeds its configured budget share. Fired at most once per
    /// cache generation; the over-budget unpinned entries are already marked for the next
    /// boundary sweep. Pinned/skill weight that cannot be evicted keeps the warning standing.
    KnowledgeBudgetExceeded {
        turn: u32,
        used: u32,
        budget: u32,
    },
    Rollbacked {
        turn: u32,
        checkpoint_history_len: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<RollbackReason>,
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
    /// Evidence collected by the verifier during milestone evaluation.
    MilestoneEvidence {
        turn: u32,
        phase_id: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        evidence: Vec<String>,
    },
    /// Checkpoint taken at the start of a turn transaction (before LLM call).
    CheckpointTaken {
        turn: u32,
        history_len: u32,
    },
    /// O6: the repeat fuse tripped — the same turn signature (non-meta tool name AND args) was
    /// re-issued `count`x consecutively. `action` = `"deny"` (turn rolled back, directive note fed
    /// back) or `"terminate"` (run ends `no_progress` after one final report turn). Additive ABI.
    RepeatFuseTripped {
        turn: u32,
        signature: String,
        count: u32,
        action: String,
    },
    /// O4: the turn-end criteria gate fired — the model tried to finish while acceptance criteria
    /// stand; the kernel injected one self-check turn before accepting `Completed`. Additive ABI.
    CriteriaGateFired {
        turn: u32,
        criteria: Vec<String>,
    },
    /// Kernel process table changed for a spawned sub-agent.
    AgentProcessChanged {
        turn: u32,
        agent_id: String,
        parent_session_id: String,
        role: String,
        isolation: String,
        context_inheritance: String,
        state: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        permitted_capability_ids: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_termination: Option<String>,
    },
    /// W0-ABI: a workflow batch was spawned — each node's spawn descriptor (agent id + goal +
    /// role/isolation/inheritance) so the SDK can run the kernel-generated nodes.
    WorkflowBatchSpawned {
        turn: u32,
        nodes: Vec<crate::orchestration::workflow::WorkflowSpawnInfo>,
        /// G4 budget-as-signal: the workflow's remaining headroom under the active quota at spawn
        /// time, so a coordinator node can scale its next submission. Additive: omitted when no
        /// resource quota is installed (nothing to report).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        budget: Option<crate::orchestration::workflow::WorkflowBudget>,
    },
    /// W0-ABI: a workflow finished (all nodes terminal, or stalled by a gated dependency).
    WorkflowCompleted {
        turn: u32,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        completed: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        failed: Vec<String>,
    },
    /// #2-B: a high-urgency `InterruptNow` signal preempted in-flight work. The kernel has already
    /// marked these agents `Done(UserAbort)` and reclaimed the root to reason about the interrupt; the
    /// SDK must ABORT the listed in-flight child runs and discard their results (do NOT feed their
    /// `SubAgentCompleted`). Additive variant (`agent_preempted`) — byte-identical for SDKs that never
    /// receive it.
    AgentPreempted {
        turn: u32,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        agent_ids: Vec<String>,
        reason: String,
    },
    /// A tool call needs user approval (governance `AskUser`). Not blocked by the
    /// kernel — the SDK must obtain approval before executing the named call.
    ToolGated {
        turn: u32,
        call_id: String,
        tool: String,
        reason: String,
    },
    /// An inbound signal was routed by the in-kernel attention policy.
    SignalDisposed {
        turn: u32,
        signal_id: String,
        disposition: String,
        queue_depth: u32,
    },
    /// A budget axis (turns / tokens / wall-time) was exhausted.
    BudgetExceeded { turn: u32, budget: String },
    /// Loop entered `Suspended` state (awaiting human approval or sub-agent).
    Suspended {
        turn: u32,
        reason: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pending_calls: Vec<String>,
    },
    /// Loop resumed from `Suspended` state.
    Resumed {
        turn: u32,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        approved: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        denied: Vec<String>,
    },
    /// Working memory archived for long-term storage (page-out decision).
    PageOut {
        turn: u32,
        action: KernelPressureAction,
        rho_after: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        archived: Vec<Message>,
        tier_hint: String,
    },
    /// Kernel requests SDK to fetch long-term memory for a meta-tool call.
    PageInRequested {
        turn: u32,
        call_id: String,
        tool: String,
        query: String,
        top_k: u32,
    },
    /// Memory entry written successfully (Phase 7).
    MemoryWritten {
        turn: u32,
        memory_id: String,
        memory_kind: String,
        size_bytes: u32,
    },
    /// Memory validation failed (Phase 7).
    MemoryValidationFailed {
        turn: u32,
        memory_id: String,
        error: String,
    },
    /// Memory query request (Phase 7).
    MemoryQueried {
        turn: u32,
        query_context: String,
        requested_k: usize,
        requires_async_response: bool,
    },
    /// Large tool result spooled (Layer 1).
    LargeResultSpooled {
        turn: u32,
        call_id: String,
        tool: String,
        original_size: u32,
        preview_size: u32,
        spool_ref: Option<String>,
    },
}

/// Transaction-boundary observations emitted by the kernel.
///
/// A turn transaction lifecycle looks like:
///   `CheckpointTaken` (before LLM call) → … → `Rollbacked` (if fatal) or
///   implicit commit (clean `ToolCompleted` + turn increment).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TransactionObservation {
    CheckpointTaken { turn: u32, history_len: u32 },
    Rollbacked {
        turn: u32,
        checkpoint_history_len: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<crate::runtime::session::RollbackReason>,
    },
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

    /// L1 (RunGroup): this vehicle's cumulative sub-agent spawns this run, read back by the SDK at run
    /// end to charge the group ledger (so the next member's cumulative spawn cap is seeded correctly).
    pub fn local_subagents_spawned(&self) -> u32 {
        self.sm.local_subagents_spawned()
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
            KernelInputEvent::SkillActivated { name } => {
                // B1: record the activation (B2 reads it in emit_call_llm to narrow tools).
                // The returned `changed` flag is the epoch boundary for D's cache re-anchor.
                self.sm.ctx.activate_skill(name);
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::SetStableCoreTools { tool_ids } => {
                self.sm.ctx.set_stable_core_tools(tool_ids.into_iter().map(Into::into));
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
            KernelInputEvent::SetTokenizer { .. } => {
                // Local BPE tokenisers are no longer used — accuracy comes from
                // observed_input_tokens reported by the provider API (P0-1 Step 2).
                // char_approx is always used for pre-flight truncation estimates.
                self.sm.ctx.engine = ContextTokenEngine::char_approx();
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
            KernelInputEvent::AddKnowledgeMessage { content, tokens, key, pinned } => {
                // P1-B2 cache contract: the knowledge partition renders into the cached system[1]
                // block. Appending here is the right home for *stable* reference material (skill
                // defs, durable artifacts) — it's append-only, so the existing prefix stays
                // byte-stable, and a fresh append costs only a one-time system[1] re-cache. Do NOT
                // route *per-turn* retrievals (a memory/knowledge lookup that changes every turn)
                // through here: each would rewrite the cached block and invalidate it plus the
                // history cache every turn. Volatile per-turn context belongs on the signal/tail
                // path (`push_signal` → state_turn), which is uncached *and* high-attention (P1-F).
                //
                // K1: a `key` gives the entry identity — a same-key push stages a boundary-deferred
                // upsert instead of appending a duplicate (the cache contract above is why the swap
                // waits for the next compaction/renewal boundary).
                self.sm.ctx.push_knowledge_entry(
                    key.map(compact_str::CompactString::from),
                    Message::system(content),
                    tokens.max(1),
                    pinned,
                );
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::RemoveKnowledge { key } => {
                self.sm.ctx.remove_knowledge(&key);
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
            KernelInputEvent::LoadGovernancePolicy {
                default_action,
                rules,
                vetoed_tools,
                rate_limits,
                constraints,
            } => {
                self.sm.set_governance(build_governance_pipeline(
                    default_action,
                    rules,
                    vetoed_tools,
                    rate_limits,
                    constraints,
                ));
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::ConfigureRun { config } => {
                // K2: apply a bundle of run-setup config in one event (tools / governance / attention /
                // quota / scheduler / toggles), replacing the ~10 separate `Set*` / `Load*` events the
                // SDK used to fire one-by-one. Each field is optional; an absent field is left untouched.
                // The individual events remain for runtime mutation (skill mount, mid-run budget change).
                // Each branch delegates to exactly the method its granular event uses, so the two paths
                // can never diverge.
                let RunConfig {
                    tools,
                    available_skills,
                    stable_core_tools,
                    memory_enabled,
                    knowledge_enabled,
                    plan_tool_enabled,
                    tokenizer,
                    governance,
                    attention_max_queue_size,
                    scheduler_max_wall_ms,
                    resource_quota,
                    group_tokens_base,
                    group_spawns_base,
                    repeat_fuse,
                    criteria_gate,
                    knowledge_budget_ratio,
                } = config;
                if let Some(tools) = tools {
                    self.sm.tools = tools;
                }
                if let Some(skills) = available_skills {
                    self.sm.ctx.set_available_skills(skills);
                }
                if let Some(ids) = stable_core_tools {
                    self.sm.ctx.set_stable_core_tools(ids.into_iter().map(Into::into));
                }
                if let Some(enabled) = memory_enabled {
                    self.sm.ctx.set_memory_enabled(enabled);
                }
                if let Some(enabled) = knowledge_enabled {
                    self.sm.ctx.set_knowledge_enabled(enabled);
                }
                if let Some(enabled) = plan_tool_enabled {
                    self.sm.ctx.set_plan_tool_enabled(enabled);
                }
                if tokenizer.is_some() {
                    self.sm.ctx.engine = ContextTokenEngine::char_approx();
                }
                if let Some(g) = governance {
                    self.sm.set_governance(build_governance_pipeline(
                        g.default_action,
                        g.rules,
                        g.vetoed_tools,
                        g.rate_limits,
                        g.constraints,
                    ));
                }
                if let Some(max_queue) = attention_max_queue_size {
                    self.sm.set_attention(max_queue as usize);
                }
                if let Some(ms) = scheduler_max_wall_ms {
                    self.sm.set_wall_budget(Some(ms));
                }
                if let Some(quota) = resource_quota {
                    self.sm.set_resource_quota(quota);
                }
                if let Some(base) = group_tokens_base {
                    self.sm.seed_group_budget(base);
                }
                if let Some(base) = group_spawns_base {
                    self.sm.seed_group_spawns(base);
                }
                if let Some(fuse) = repeat_fuse {
                    self.sm.set_repeat_fuse(fuse);
                }
                if let Some(enabled) = criteria_gate {
                    self.sm.set_criteria_gate(enabled);
                }
                if let Some(ratio) = knowledge_budget_ratio {
                    self.sm.ctx.config.knowledge_budget_ratio = ratio;
                }
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::SetAttentionPolicy { max_queue_size } => {
                self.sm.set_attention(max_queue_size as usize);
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::PageIn { entries } => {
                self.sm.apply_page_in(&entries);
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
            KernelInputEvent::Resume { approved_calls, denied_calls } => {
                let action = self.sm.resume_from_suspend(approved_calls, denied_calls);
                if matches!(action, LoopAction::AwaitingResume) {
                    return KernelStep::empty(self.sm.take_observations());
                }
                return KernelStep::single(action, self.sm.take_observations());
            }
            KernelInputEvent::SetSchedulerBudget { max_wall_ms } => {
                self.sm.set_wall_budget(max_wall_ms);
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::SetResourceQuota { quota } => {
                self.sm.set_resource_quota(quota);
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::SetCriteriaGate { enabled } => {
                self.sm.set_criteria_gate(enabled);
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::SetKnowledgeBudget { ratio } => {
                self.sm.ctx.config.knowledge_budget_ratio = ratio;
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::SetRepeatFuse { enabled, deny_after, terminate_after } => {
                let mut cfg = self.sm.repeat_fuse_config();
                if let Some(e) = enabled { cfg.enabled = e; }
                if let Some(d) = deny_after { cfg.deny_after = d; }
                if let Some(t) = terminate_after { cfg.terminate_after = t; }
                self.sm.set_repeat_fuse(cfg);
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::ProviderResult {
                message,
                observed_input_tokens,
                observed_output_tokens: _,
                now_ms,
                stop_reason,
            } => {
                if let Some(tokens) = observed_input_tokens {
                    self.sm.ctx.set_observed_prompt_tokens(tokens);
                }
                // Feed the clock before the governance gate fires inside `feed`, so the
                // rate limiter sees a real timestamp (no-op when no policy is loaded).
                if let Some(ms) = now_ms {
                    self.sm.set_observed_time(ms);
                }
                // Stash stop_reason so `feed` can detect an output-cap truncation and drive recovery.
                self.sm.set_pending_stop_reason(stop_reason);
                self.sm.feed(LoopEvent::LLMResponse { message })
            }
            KernelInputEvent::ToolResults { results } => {
                self.sm.feed(LoopEvent::ToolResults { results })
            }
            KernelInputEvent::ProviderError { message } => {
                // Reactive recovery is a kernel decision: classify + bounded compact-and-retry,
                // returning the next action (retry or honest terminal) through the common tail.
                self.sm.recover_from_provider_error(&message)
            }
            KernelInputEvent::Signal { signal } => match self.sm.signal_event(signal) {
                Some(action) => action,
                // Non-actionable disposition (queued / observed / ignored / dropped):
                // no provider call this step, just the SignalDisposed observation.
                None => return KernelStep::empty(self.sm.take_observations()),
            },
            KernelInputEvent::MilestoneResult { result } => {
                self.sm.feed(LoopEvent::MilestoneResult { result })
            }
            KernelInputEvent::SpawnSubAgent {
                spec,
                parent_session_id,
            } => {
                let action = self.sm.spawn_sub_agent(spec, &parent_session_id);
                if matches!(action, LoopAction::AwaitingResume) {
                    return KernelStep::empty(self.sm.take_observations());
                }
                return KernelStep::single(action, self.sm.take_observations());
            }
            KernelInputEvent::LoadWorkflow {
                spec,
                parent_session_id,
                resumed_completed,
                resumed_submissions,
            } => {
                // K1: self-bootstrap the run if the host never fired `StartRun` (stateless
                // `runWorkflow` caller). Parity with the agent-reachable `SubmitWorkflow`, which already
                // bootstraps. Idempotent no-op once the root task has left `Ready`.
                self.sm.ensure_started_for_workflow(&spec);
                let action = if resumed_completed.is_empty() && resumed_submissions.is_empty() {
                    self.sm.load_workflow(spec, &parent_session_id)
                } else {
                    self.sm.load_workflow_resumed(
                        spec,
                        &parent_session_id,
                        &resumed_submissions,
                        &resumed_completed,
                    )
                };
                if matches!(action, LoopAction::AwaitingResume) {
                    return KernelStep::empty(self.sm.take_observations());
                }
                return KernelStep::single(action, self.sm.take_observations());
            }
            KernelInputEvent::SubAgentCompleted { result } => {
                self.sm.feed(LoopEvent::SubAgentCompleted { result })
            }
            KernelInputEvent::SubmitWorkflowNodes {
                nodes,
                submitter_agent_id,
            } => {
                let action = self
                    .sm
                    .submit_workflow_nodes(nodes, submitter_agent_id.as_deref());
                if matches!(action, LoopAction::AwaitingResume) {
                    return KernelStep::empty(self.sm.take_observations());
                }
                return KernelStep::single(action, self.sm.take_observations());
            }
            KernelInputEvent::SubmitWorkflow {
                spec,
                parent_session_id,
                submitter_agent_id,
            } => {
                let action = self.sm.submit_workflow(
                    spec,
                    &parent_session_id,
                    submitter_agent_id.as_deref(),
                );
                if matches!(action, LoopAction::AwaitingResume) {
                    return KernelStep::empty(self.sm.take_observations());
                }
                return KernelStep::single(action, self.sm.take_observations());
            }
            KernelInputEvent::SetMemoryPolicy {
                memory_path,
                stale_warning_days,
                retrieval_top_k,
                validation_enabled,
                max_content_bytes,
                max_name_length,
            } => {
                // Phase 7: install the memory policy. The kernel enforces validation_enabled +
                // retrieval_top_k + size/name overrides at the WriteMemory/QueryMemory traps;
                // memory_path / stale_warning_days are carried for the SDK's recall I/O.
                self.sm.set_memory_policy(crate::mm::memory::MemoryPolicy {
                    memory_path,
                    stale_warning_days,
                    retrieval_top_k,
                    validation_enabled,
                    max_content_bytes,
                    max_name_length,
                });
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::WriteMemory { memory } => {
                // Phase 7: Validate memory write request.
                // Kernel validates; SDK performs I/O.
                use crate::mm::memory::validate_memory_write;
                let turn = self.sm.turn;
                // M2: route the write through the syscall trap so the resource quota (write-rate
                // limit) applies. A rate-limited / denied write surfaces as a validation failure
                // (the write does not happen) and short-circuits before validation.
                let disposition = self
                    .sm
                    .gate_syscall(&crate::syscall::Syscall::WriteMemory(memory.clone()));
                if !disposition.is_allowed() {
                    let error = match disposition {
                        crate::syscall::Disposition::RateLimited { retry_after_ms } => {
                            format!("memory write rate limited; retry after {retry_after_ms}ms")
                        }
                        crate::syscall::Disposition::Deny { reason, .. } => {
                            format!("memory write denied: {reason}")
                        }
                        _ => "memory write not permitted".to_string(),
                    };
                    self.sm.observations.push(
                        KernelObservation::MemoryValidationFailed {
                            turn,
                            memory_id: memory.metadata.name.clone(),
                            error,
                        },
                    );
                    return KernelStep::empty(self.sm.take_observations());
                }
                // Validate honoring any installed memory policy: a policy with validation disabled
                // admits the write outright; a policy with size/name overrides validates against
                // those; no policy uses the default rules (pre-policy behavior).
                let validation_result = match self.sm.memory_policy() {
                    Some(p) if !p.validation_enabled => Ok(()),
                    Some(p) => p.validation().validate(&memory),
                    None => validate_memory_write(&memory),
                };
                match validation_result {
                    Ok(()) => {
                        // Emit observation for SDK to perform I/O
                        self.sm.observations.push(KernelObservation::MemoryWritten {
                            turn,
                            memory_id: memory.metadata.name.clone(),
                            memory_kind: memory.metadata.kind.map(|k| k.label()).unwrap_or_else(|| {
                                crate::mm::memory::MemoryKind::infer_from_metadata(&memory.metadata).label()
                            }).to_string(),
                            size_bytes: memory.content.len() as u32,
                        });
                    }
                    Err(err) => {
                        // Emit validation error observation
                        use crate::mm::memory::MemoryValidationError;
                        let error_msg = match err {
                            MemoryValidationError::MissingRequiredField { field } => format!("Missing required field: {}", field),
                            MemoryValidationError::ContentTooLarge { size, limit } => format!("Content too large: {} bytes (limit: {})", size, limit),
                            MemoryValidationError::ForbiddenPattern { pattern, reason } => format!("Forbidden pattern '{}': {}", pattern, reason),
                            MemoryValidationError::InvalidKind { kind } => format!("Invalid kind: {}", kind),
                            MemoryValidationError::NameTooLong { length, limit } => format!("Name too long: {} chars (limit: {})", length, limit),
                        };
                        self.sm.observations.push(KernelObservation::MemoryValidationFailed {
                            turn,
                            memory_id: memory.metadata.name.clone(),
                            error: error_msg,
                        });
                    }
                }
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::QueryMemory { query } => {
                // Phase 7: Query memory for context.
                // Kernel emits observation; SDK responds asynchronously.
                let turn = self.sm.turn;
                // An installed policy caps retrieval breadth: requested_k = min(query.top_k, policy).
                let requested_k = match self.sm.memory_policy() {
                    Some(p) => p.clamp_top_k(query.top_k),
                    None => query.top_k,
                };
                self.sm.observations.push(KernelObservation::MemoryQueried {
                    turn,
                    query_context: query.current_context.clone(),
                    requested_k,
                    requires_async_response: true,
                });
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::MemoryRetrievalResult { .. } => {
                // SDK acknowledgment after async retrieval; no further kernel action.
                return KernelStep::empty(self.sm.take_observations());
            }
            KernelInputEvent::Timeout => self.sm.feed(LoopEvent::Timeout),
        };
        if matches!(action, LoopAction::AwaitingResume) {
            return KernelStep::empty(self.sm.take_observations());
        }
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
            observed_input_tokens: None,
            observed_output_tokens: None,
            stop_reason: None,
            now_ms: None,
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
    fn skill_activated_input_records_active_skill() {
        // P1-B B1: the SkillActivated event (serde `skill_activated`) records the active skill and,
        // via the catalog's declared tools, yields a narrowing filter — without itself acting.
        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        let mut debug = SkillMetadata::new("debug", "Debug helper");
        debug.allowed_tools = vec!["read".into(), "grep".into()];
        runtime.step(KernelInput::new(KernelInputEvent::SetAvailableSkills {
            skills: vec![debug],
        }));

        let step = runtime.step(KernelInput::new(KernelInputEvent::SkillActivated {
            name: "debug".to_string(),
        }));

        assert!(step.actions.is_empty(), "activation is config, not an action");
        assert!(runtime.state_machine().ctx.active_skills.contains("debug"));
        let filter = runtime.state_machine().ctx.active_skill_tool_filter().unwrap();
        assert_eq!(filter.len(), 2);
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
    fn add_knowledge_message_enters_knowledge_partition() {
        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        let step = runtime.step(KernelInput::new(KernelInputEvent::AddKnowledgeMessage {
            content: "skill: debug".to_string(),
            tokens: 10,
            key: None,
            pinned: false,
        }));

        assert!(step.actions.is_empty());
        assert_eq!(
            runtime.state_machine().ctx.partitions.knowledge.len(),
            1
        );
    }

    #[test]
    fn knowledge_budget_exceeded_observed_in_live_loop() {
        // K2: the per-turn budget check runs in the LLMResponse path; an over-budget knowledge
        // partition (40 tokens > 100 × 0.25 default) surfaces as a KnowledgeBudgetExceeded
        // observation. Raising the ratio via SetKnowledgeBudget silences it.
        let mut runtime = KernelRuntime::new(LoopPolicy {
            max_tokens: 100,
            ..LoopPolicy::default()
        });
        runtime.step(KernelInput::new(KernelInputEvent::AddKnowledgeMessage {
            content: "reference".to_string(),
            tokens: 40,
            key: None,
            pinned: false,
        }));
        run_with_tool_call(&mut runtime, "search");
        // The check runs at the turn boundary (ToolResults handling), not on the proposal.
        let step = runtime.step(KernelInput::new(KernelInputEvent::ToolResults {
            results: vec![ToolResult {
                call_id: "call-1".into(),
                output: crate::types::message::Content::Text("ok".into()),
                is_error: false,
                is_fatal: false,
                error_kind: None,
                token_count: None,
            }],
        }));
        assert!(step.observations.iter().any(|o| matches!(
            o,
            KernelObservation::KnowledgeBudgetExceeded { used: 40, budget: 25, .. }
        )));

        // Runtime knob: a generous ratio ⇒ under budget ⇒ no warning on the next turn.
        let mut runtime2 = KernelRuntime::new(LoopPolicy {
            max_tokens: 100,
            ..LoopPolicy::default()
        });
        runtime2.step(KernelInput::new(KernelInputEvent::SetKnowledgeBudget { ratio: 0.9 }));
        runtime2.step(KernelInput::new(KernelInputEvent::AddKnowledgeMessage {
            content: "reference".to_string(),
            tokens: 40,
            key: None,
            pinned: false,
        }));
        run_with_tool_call(&mut runtime2, "search");
        let step2 = runtime2.step(KernelInput::new(KernelInputEvent::ToolResults {
            results: vec![ToolResult {
                call_id: "call-1".into(),
                output: crate::types::message::Content::Text("ok".into()),
                is_error: false,
                is_fatal: false,
                error_kind: None,
                token_count: None,
            }],
        }));
        assert!(!step2.observations.iter().any(|o| matches!(
            o,
            KernelObservation::KnowledgeBudgetExceeded { .. }
        )));
    }

    #[test]
    fn keyed_add_knowledge_dedupes_and_remove_marks() {
        // K1 event-level: a same-key AddKnowledgeMessage stages an upsert (one entry, original
        // bytes rendered mid-generation); RemoveKnowledge marks for the boundary sweep. Both
        // decoded from the serde wire shape SDKs feed.
        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        for content in ["v1", "v2"] {
            let ev: KernelInputEvent = serde_json::from_value(serde_json::json!({
                "kind": "add_knowledge_message",
                "content": content,
                "tokens": 5,
                "key": "skill:debug",
            }))
            .unwrap();
            runtime.step(KernelInput::new(ev));
        }
        let knowledge = &runtime.state_machine().ctx.partitions.knowledge;
        assert_eq!(knowledge.len(), 1);
        assert_eq!(
            knowledge.messages().next().and_then(|m| m.content.as_text()),
            Some("v1"),
            "upsert deferred to boundary — original bytes still rendered"
        );

        let ev: KernelInputEvent = serde_json::from_value(serde_json::json!({
            "kind": "remove_knowledge",
            "key": "skill:debug",
        }))
        .unwrap();
        runtime.step(KernelInput::new(ev));
        assert!(runtime.state_machine().ctx.partitions.knowledge.entries[0].evict_at_boundary);
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

    #[test]
    fn spawn_sub_agent_input_registers_process() {
        use crate::types::agent::{AgentIdentity, AgentRole, AgentRunSpec};

        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        runtime.step(KernelInput::new(KernelInputEvent::StartRun {
            task: RuntimeTask::new("parent task"),
            run_spec: None,
        }));
        runtime.state_machine_mut().take_observations();

        let spec = AgentRunSpec::new(
            AgentIdentity::sub_agent("worker", "worker-session"),
            AgentRole::Implement,
            "do work",
        );
        let step = runtime.step(KernelInput::new(KernelInputEvent::SpawnSubAgent {
            spec,
            parent_session_id: "parent-session".to_string(),
        }));

        assert!(step.actions.is_empty());
        assert!(step.observations.iter().any(|o| matches!(
            o,
            KernelObservation::AgentProcessChanged {
                agent_id,
                parent_session_id,
                state,
                ..
            } if agent_id == "worker" && parent_session_id == "parent-session" && state == "running"
        )));
        assert_eq!(
            runtime
                .state_machine()
                .agent_process("worker")
                .expect("process")
                .parent_session_id
                .as_str(),
            "parent-session"
        );
        assert!(step.observations.iter().any(|o| matches!(
            o,
            KernelObservation::Suspended { reason, .. } if reason == "sub_agent_await"
        )));
        assert!(runtime.state_machine().is_suspended());
        assert!(matches!(
            runtime.state_machine().wait_reason(),
            Some(crate::scheduler::tcb::WaitReason::SubAgentJoin(_))
        ));
    }

    #[test]
    fn set_resource_quota_input_denies_spawn_over_quota() {
        use crate::governance::quota::ResourceQuota;
        use crate::types::agent::{AgentIdentity, AgentRole, AgentRunSpec};

        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        // Quota flows in through the same versioned JSON event ABI as governance/scheduler config.
        let step = runtime.step(KernelInput::new(KernelInputEvent::SetResourceQuota {
            quota: ResourceQuota { max_spawn_depth: Some(0), ..ResourceQuota::default() },
        }));
        assert!(step.actions.is_empty(), "config input yields no actions");

        runtime.step(KernelInput::new(KernelInputEvent::StartRun {
            task: RuntimeTask::new("parent task"),
            run_spec: None,
        }));
        runtime.state_machine_mut().take_observations();

        let spec = AgentRunSpec::new(
            AgentIdentity::sub_agent("worker", "worker-session"),
            AgentRole::Implement,
            "do work",
        );
        let step = runtime.step(KernelInput::new(KernelInputEvent::SpawnSubAgent {
            spec,
            parent_session_id: "parent-session".to_string(),
        }));

        // Denied spawn rolls the turn back to another reasoning pass — no process registered,
        // not suspended on a sub-agent join.
        assert!(matches!(
            step.actions.as_slice(),
            [KernelAction::CallProvider { .. }]
        ));
        assert!(!step.observations.iter().any(|o| matches!(
            o,
            KernelObservation::AgentProcessChanged { agent_id, .. } if agent_id == "worker"
        )));
        assert!(runtime.state_machine().agent_process("worker").is_none());
        assert!(!runtime.state_machine().is_suspended());
    }

    #[test]
    fn group_budget_base_enforces_shared_token_cap() {
        use crate::types::message::{Content, Message, ToolCall, ToolResult};

        // Drive one tool-calling turn under a 100-token run cap, with `group_base` already spent by
        // other members of the governance domain. The token-budget axis is checked after the tool
        // results, against `group_base + local`.
        fn run_one_turn(group_base: Option<u64>) -> KernelStep {
            let mut runtime = KernelRuntime::new(LoopPolicy {
                max_total_tokens: 100,
                ..LoopPolicy::default()
            });
            runtime.step(KernelInput::new(KernelInputEvent::ConfigureRun {
                config: RunConfig { group_tokens_base: group_base, ..RunConfig::default() },
            }));
            runtime.step(KernelInput::new(KernelInputEvent::StartRun {
                task: RuntimeTask::new("task"),
                run_spec: None,
            }));
            let mut msg = Message::assistant("");
            msg.token_count = Some(10); // this vehicle's local spend this turn
            msg.tool_calls.push(ToolCall {
                id: "c1".into(),
                name: "echo".into(),
                arguments: serde_json::json!({}),
            });
            runtime.step(KernelInput::new(KernelInputEvent::ProviderResult {
                message: msg,
                observed_input_tokens: None,
                observed_output_tokens: None,
                stop_reason: None,
                now_ms: None,
            }));
            runtime.step(KernelInput::new(KernelInputEvent::ToolResults {
                results: vec![ToolResult {
                    call_id: "c1".into(),
                    output: Content::Text("ok".into()),
                    is_error: false,
                    is_fatal: false,
                    error_kind: None,
                    token_count: None,
                }],
            }))
        }

        let exceeded = |step: &KernelStep| {
            step.observations.iter().any(|o| {
                matches!(o, KernelObservation::BudgetExceeded { budget, .. } if budget == "token_budget")
            })
        };

        // Group already spent 95; this vehicle's 10 pushes the domain to 105 > 100 → shared cap fires.
        assert!(
            exceeded(&run_one_turn(Some(95))),
            "group token budget must span the whole domain"
        );
        // N=1 / no group (base 0): local 10 is far under the cap → pre-L1 behavior unchanged.
        assert!(
            !exceeded(&run_one_turn(None)),
            "no group seed ⇒ per-vehicle budget, well under cap"
        );
    }

    #[test]
    fn group_spawns_base_enforces_cumulative_spawn_cap() {
        use crate::governance::quota::ResourceQuota;
        use crate::types::agent::{AgentIdentity, AgentRole, AgentRunSpec};

        // Cumulative cap of 2 sub-agents across the domain. Other members already spawned 2 (seeded),
        // so this vehicle's very first spawn is denied — the cap spans the whole group.
        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        runtime.step(KernelInput::new(KernelInputEvent::ConfigureRun {
            config: RunConfig {
                resource_quota: Some(ResourceQuota {
                    max_total_subagents: Some(2),
                    ..ResourceQuota::default()
                }),
                group_spawns_base: Some(2),
                ..RunConfig::default()
            },
        }));
        runtime.step(KernelInput::new(KernelInputEvent::StartRun {
            task: RuntimeTask::new("task"),
            run_spec: None,
        }));
        runtime.state_machine_mut().take_observations();

        let spec = AgentRunSpec::new(
            AgentIdentity::sub_agent("worker", "worker-session"),
            AgentRole::Implement,
            "do work",
        );
        let step = runtime.step(KernelInput::new(KernelInputEvent::SpawnSubAgent {
            spec,
            parent_session_id: "parent-session".to_string(),
        }));

        // Denied: domain already at the cumulative cap → rolled back, no process registered.
        assert!(matches!(
            step.actions.as_slice(),
            [KernelAction::CallProvider { .. }]
        ));
        assert!(runtime.state_machine().agent_process("worker").is_none());
        assert_eq!(runtime.local_subagents_spawned(), 0);
    }

    #[test]
    fn default_runtime_leaves_spawn_unquota_ed() {
        use crate::types::agent::{AgentIdentity, AgentRole, AgentRunSpec};

        // No SetResourceQuota event => pre-M2 behavior: spawn is unconditionally admitted.
        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        runtime.step(KernelInput::new(KernelInputEvent::StartRun {
            task: RuntimeTask::new("parent task"),
            run_spec: None,
        }));
        runtime.state_machine_mut().take_observations();

        let spec = AgentRunSpec::new(
            AgentIdentity::sub_agent("worker", "worker-session"),
            AgentRole::Implement,
            "do work",
        );
        runtime.step(KernelInput::new(KernelInputEvent::SpawnSubAgent {
            spec,
            parent_session_id: "parent-session".to_string(),
        }));
        assert!(runtime.state_machine().agent_process("worker").is_some());
        assert!(runtime.state_machine().is_suspended());
    }

    /// Wire-format lock for `agent_process_changed` multi-word enum values. The kernel stringifies
    /// `isolation`/`context_inheritance` as debug-lowercase (`readonly`/`systemonly`), which is NOT
    /// the same as serde snake_case (`read_only`/`system_only`) — and no golden fixture covers these
    /// variants. This pins the current bytes so the observation refactor cannot silently change them.
    #[test]
    fn agent_process_changed_locks_multiword_wire_form() {
        use crate::types::agent::{AgentIdentity, AgentIsolation, AgentRole, AgentRunSpec};

        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        runtime.step(KernelInput::new(KernelInputEvent::StartRun {
            task: RuntimeTask::new("parent task"),
            run_spec: None,
        }));
        runtime.state_machine_mut().take_observations();

        // Verify role → SystemOnly inheritance; explicit ReadOnly isolation. Both are multi-word.
        let spec = AgentRunSpec::new(
            AgentIdentity::sub_agent("worker", "worker-session"),
            AgentRole::Verify,
            "do work",
        )
        .with_isolation(AgentIsolation::ReadOnly);
        let step = runtime.step(KernelInput::new(KernelInputEvent::SpawnSubAgent {
            spec,
            parent_session_id: "parent-session".to_string(),
        }));

        let obs = step
            .observations
            .iter()
            .find(|o| matches!(o, KernelObservation::AgentProcessChanged { .. }))
            .expect("agent_process_changed observation");
        let json = serde_json::to_value(obs).unwrap();
        assert_eq!(json["isolation"], "readonly", "isolation must stay debug-lowercase");
        assert_eq!(
            json["context_inheritance"], "systemonly",
            "context_inheritance must stay debug-lowercase"
        );
        assert_eq!(json["role"], "verify");
        assert_eq!(json["state"], "running");
    }

    // ── M-memory-policy: set_memory_policy is enforced at the WriteMemory / QueryMemory traps ──

    fn write_memory(runtime: &mut KernelRuntime, name: &str, content: &str) -> KernelStep {
        use crate::mm::memory::{MemoryMetadata, MemoryWriteRequest};
        runtime.step(KernelInput::new(KernelInputEvent::WriteMemory {
            memory: MemoryWriteRequest {
                metadata: MemoryMetadata {
                    name: name.to_string(),
                    description: "desc".to_string(),
                    ..Default::default()
                },
                content: content.to_string(),
            },
        }))
    }

    #[test]
    fn memory_policy_validation_disabled_admits_forbidden_write() {
        // "代码模式:" is a forbidden pattern under default validation; disabling validation admits it.
        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        runtime.step(KernelInput::new(KernelInputEvent::SetMemoryPolicy {
            memory_path: String::new(),
            stale_warning_days: 2,
            retrieval_top_k: 5,
            validation_enabled: false,
            max_content_bytes: None,
            max_name_length: None,
        }));
        let step = write_memory(&mut runtime, "note", "代码模式: foo");
        assert!(step
            .observations
            .iter()
            .any(|o| matches!(o, KernelObservation::MemoryWritten { .. })));
        assert!(!step
            .observations
            .iter()
            .any(|o| matches!(o, KernelObservation::MemoryValidationFailed { .. })));
    }

    #[test]
    fn default_runtime_validates_forbidden_write() {
        // No policy installed => default validation rejects the forbidden pattern (pre-policy behavior).
        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        let step = write_memory(&mut runtime, "note", "代码模式: foo");
        assert!(step
            .observations
            .iter()
            .any(|o| matches!(o, KernelObservation::MemoryValidationFailed { .. })));
    }

    #[test]
    fn memory_policy_size_override_rejects_oversized_write() {
        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        runtime.step(KernelInput::new(KernelInputEvent::SetMemoryPolicy {
            memory_path: String::new(),
            stale_warning_days: 2,
            retrieval_top_k: 5,
            validation_enabled: true,
            max_content_bytes: Some(8),
            max_name_length: None,
        }));
        let step = write_memory(&mut runtime, "note", "this content is well over eight bytes");
        let failed = step.observations.iter().find_map(|o| match o {
            KernelObservation::MemoryValidationFailed { error, .. } => Some(error.clone()),
            _ => None,
        });
        assert!(failed.is_some_and(|e| e.contains("too large")));
    }

    #[test]
    fn memory_policy_clamps_retrieval_top_k() {
        use crate::mm::memory::MemoryQuery;
        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        runtime.step(KernelInput::new(KernelInputEvent::SetMemoryPolicy {
            memory_path: String::new(),
            stale_warning_days: 2,
            retrieval_top_k: 3,
            validation_enabled: true,
            max_content_bytes: None,
            max_name_length: None,
        }));
        let step = runtime.step(KernelInput::new(KernelInputEvent::QueryMemory {
            query: MemoryQuery { top_k: 50, ..Default::default() },
        }));
        let requested = step.observations.iter().find_map(|o| match o {
            KernelObservation::MemoryQueried { requested_k, .. } => Some(*requested_k),
            _ => None,
        });
        assert_eq!(requested, Some(3));
    }

    #[test]
    fn default_runtime_uses_requested_top_k_verbatim() {
        use crate::mm::memory::MemoryQuery;
        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        let step = runtime.step(KernelInput::new(KernelInputEvent::QueryMemory {
            query: MemoryQuery { top_k: 50, ..Default::default() },
        }));
        let requested = step.observations.iter().find_map(|o| match o {
            KernelObservation::MemoryQueried { requested_k, .. } => Some(*requested_k),
            _ => None,
        });
        assert_eq!(requested, Some(50));
    }

    #[test]
    fn provider_result_now_ms_drives_wall_time_budget() {
        let mut runtime = KernelRuntime::new(LoopPolicy {
            max_wall_ms: Some(10),
            ..LoopPolicy::default()
        });
        runtime.step(KernelInput::new(KernelInputEvent::StartRun {
            task: RuntimeTask::new("ship it"),
            run_spec: None,
        }));
        let mut msg = Message::assistant("");
        msg.tool_calls.push(ToolCall {
            id: "call-1".into(),
            name: "echo".into(),
            arguments: serde_json::json!({}),
        });
        runtime.step(KernelInput::new(KernelInputEvent::ProviderResult {
            message: msg,
            observed_input_tokens: None,
            observed_output_tokens: None,
            stop_reason: None,
            now_ms: Some(100),
        }));
        let step = runtime.step(KernelInput::new(KernelInputEvent::ToolResults {
            results: vec![ToolResult {
                call_id: "call-1".into(),
                output: crate::types::message::Content::Text("ok".into()),
                is_error: false,
                is_fatal: false,
                error_kind: None,
                token_count: None,
            }],
        }));

        assert!(matches!(
            step.actions.as_slice(),
            [KernelAction::CallProvider { tools, .. }] if tools.is_empty()
        ));
    }

    // ─── Governance gate ───────────────────────────────────────────────────

    fn assistant_calling(tool: &str) -> Message {
        let mut msg = Message::assistant("");
        msg.tool_calls.push(ToolCall {
            id: "call-1".into(),
            name: tool.into(),
            arguments: serde_json::json!({}),
        });
        msg
    }

    /// Feed a tool-calling response and return the resulting step.
    fn run_with_tool_call(runtime: &mut KernelRuntime, tool: &str) -> KernelStep {
        run_with_tool_call_named(runtime, tool, "call-1")
    }

    fn run_with_tool_call_named(
        runtime: &mut KernelRuntime,
        tool: &str,
        call_id: &str,
    ) -> KernelStep {
        runtime.step(KernelInput::new(KernelInputEvent::StartRun {
            task: RuntimeTask::new("do the thing"),
            run_spec: None,
        }));
        runtime.state_machine_mut().take_observations();
        runtime.step(KernelInput::new(KernelInputEvent::ProviderResult {
            message: assistant_calling(tool),
            observed_input_tokens: None,
            observed_output_tokens: None,
            stop_reason: None,
            now_ms: None,
        }))
    }

    #[test]
    fn governance_deny_blocks_tool_and_reprompts() {
        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        runtime.step(KernelInput::new(KernelInputEvent::LoadGovernancePolicy {
            default_action: Some(PolicyAction::Allow),
            rules: vec![PolicyRule {
                tool_pattern: "danger.*".to_string(),
                action: PolicyAction::Deny,
            }],
            vetoed_tools: vec![],
            rate_limits: vec![],
            constraints: vec![],
        }));

        let step = run_with_tool_call(&mut runtime, "danger.delete");

        // Denied call must NOT reach ExecuteTool; the turn rolls back and re-prompts.
        assert!(
            matches!(step.actions.as_slice(), [KernelAction::CallProvider { .. }]),
            "denied tool should roll back and re-call provider, got {:?}",
            step.actions
        );
        assert!(
            step.observations
                .iter()
                .any(|o| matches!(o, KernelObservation::Rollbacked { .. })),
            "expected a Rollbacked observation for the denied turn",
        );
    }

    #[test]
    fn configure_run_bundle_applies_governance_equivalently_to_load_governance_policy() {
        // K2: the consolidated `ConfigureRun` bundle must apply governance identically to the granular
        // `LoadGovernancePolicy` event — a deny rule blocks the matching tool and re-prompts.
        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        runtime.step(KernelInput::new(KernelInputEvent::ConfigureRun {
            config: RunConfig {
                tools: Some(vec![]),
                governance: Some(GovernanceConfig {
                    default_action: Some(PolicyAction::Allow),
                    rules: vec![PolicyRule {
                        tool_pattern: "danger.*".to_string(),
                        action: PolicyAction::Deny,
                    }],
                    ..GovernanceConfig::default()
                }),
                attention_max_queue_size: Some(32),
                ..RunConfig::default()
            },
        }));

        let step = run_with_tool_call(&mut runtime, "danger.delete");

        assert!(
            matches!(step.actions.as_slice(), [KernelAction::CallProvider { .. }]),
            "bundle-configured deny should roll back and re-call provider, got {:?}",
            step.actions
        );
        assert!(
            step.observations
                .iter()
                .any(|o| matches!(o, KernelObservation::Rollbacked { .. })),
            "expected a Rollbacked observation for the bundle-denied turn",
        );
    }

    #[test]
    fn configure_run_round_trips_over_the_abi() {
        // The bundle must survive the versioned JSON ABI (replayable / session-loggable) like every
        // other host event.
        let event = KernelInputEvent::ConfigureRun {
            config: RunConfig {
                resource_quota: Some(crate::governance::quota::ResourceQuota {
                    max_concurrent_subagents: Some(2),
                    ..Default::default()
                }),
                scheduler_max_wall_ms: Some(60_000),
                plan_tool_enabled: Some(true),
                ..RunConfig::default()
            },
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let parsed: KernelInputEvent = serde_json::from_str(&json).expect("deserialize");
        assert!(matches!(parsed, KernelInputEvent::ConfigureRun { .. }));
    }

    #[test]
    fn governance_ask_user_suspends_until_resume() {
        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        runtime.step(KernelInput::new(KernelInputEvent::LoadGovernancePolicy {
            default_action: Some(PolicyAction::Allow),
            rules: vec![PolicyRule {
                tool_pattern: "sensitive.*".to_string(),
                action: PolicyAction::AskUser,
            }],
            vetoed_tools: vec![],
            rate_limits: vec![],
            constraints: vec![],
        }));

        let step = run_with_tool_call(&mut runtime, "sensitive.read");

        assert!(
            step.actions.is_empty(),
            "AskUser should suspend without ExecuteTool, got {:?}",
            step.actions
        );
        assert!(
            step.observations.iter().any(|o| matches!(
                o,
                KernelObservation::ToolGated { tool, .. } if tool == "sensitive.read"
            )),
            "expected a ToolGated observation for the AskUser call",
        );
        assert!(
            step.observations.iter().any(|o| matches!(
                o,
                KernelObservation::Suspended { reason, .. } if reason == "ask_user"
            )),
            "expected a Suspended observation",
        );

        let resumed = runtime.step(KernelInput::new(KernelInputEvent::Resume {
            approved_calls: vec!["call-1".to_string()],
            denied_calls: vec![],
        }));
        assert!(
            matches!(resumed.actions.as_slice(), [KernelAction::ExecuteTool { .. }]),
            "resume with approval should emit ExecuteTool, got {:?}",
            resumed.actions
        );
        assert!(
            resumed.observations.iter().any(|o| matches!(
                o,
                KernelObservation::Resumed { approved, denied, .. }
                if approved == &["call-1"] && denied.is_empty()
            )),
        );
    }

    #[test]
    fn governance_ask_user_resume_all_denied_feeds_tool_results() {
        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        runtime.step(KernelInput::new(KernelInputEvent::LoadGovernancePolicy {
            default_action: Some(PolicyAction::Allow),
            rules: vec![PolicyRule {
                tool_pattern: "sensitive.*".to_string(),
                action: PolicyAction::AskUser,
            }],
            vetoed_tools: vec![],
            rate_limits: vec![],
            constraints: vec![],
        }));
        run_with_tool_call(&mut runtime, "sensitive.read");
        runtime.state_machine_mut().take_observations();

        let step = runtime.step(KernelInput::new(KernelInputEvent::Resume {
            approved_calls: vec![],
            denied_calls: vec!["call-1".to_string()],
        }));
        assert!(
            matches!(step.actions.as_slice(), [KernelAction::CallProvider { .. }]),
            "all denied should re-prompt provider, got {:?}",
            step.actions
        );
    }

    #[test]
    fn no_governance_policy_executes_all_tools() {
        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        let step = run_with_tool_call(&mut runtime, "danger.delete");

        // Without a policy the gate is a no-op — behavior is unchanged.
        assert!(matches!(
            step.actions.as_slice(),
            [KernelAction::ExecuteTool { .. }]
        ));
        assert!(
            !step
                .observations
                .iter()
                .any(|o| matches!(o, KernelObservation::ToolGated { .. })),
        );
    }

    fn tool_ok(call_id: &str) -> ToolResult {
        ToolResult {
            call_id: call_id.into(),
            output: crate::types::message::Content::Text("ok".to_string()),
            is_error: false,
            is_fatal: false,
            error_kind: None,
            token_count: None,
        }
    }

    #[test]
    fn governance_rate_limit_blocks_second_call() {
        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        runtime.step(KernelInput::new(KernelInputEvent::LoadGovernancePolicy {
            default_action: Some(PolicyAction::Allow),
            rules: vec![],
            vetoed_tools: vec![],
            rate_limits: vec![RateLimitSpec {
                tool: "fetch".to_string(),
                max_calls: 1,
                window_ms: 60_000,
            }],
            constraints: vec![],
        }));
        runtime.step(KernelInput::new(KernelInputEvent::StartRun {
            task: RuntimeTask::new("fetch twice"),
            run_spec: None,
        }));
        runtime.state_machine_mut().take_observations();

        // First call within the window — allowed.
        let s1 = runtime.step(KernelInput::new(KernelInputEvent::ProviderResult {
            message: assistant_calling("fetch"),
            observed_input_tokens: None,
            observed_output_tokens: None,
            stop_reason: None,
            now_ms: Some(1_000),
        }));
        assert!(
            matches!(s1.actions.as_slice(), [KernelAction::ExecuteTool { .. }]),
            "first call should execute, got {:?}",
            s1.actions
        );

        // Close the turn so the kernel re-prompts the provider.
        runtime.step(KernelInput::new(KernelInputEvent::ToolResults {
            results: vec![tool_ok("call-1")],
        }));
        runtime.state_machine_mut().take_observations();

        // Second call to the same tool within the window — rate limited → rollback.
        let s2 = runtime.step(KernelInput::new(KernelInputEvent::ProviderResult {
            message: assistant_calling("fetch"),
            observed_input_tokens: None,
            observed_output_tokens: None,
            stop_reason: None,
            now_ms: Some(1_001),
        }));
        assert!(
            matches!(s2.actions.as_slice(), [KernelAction::CallProvider { .. }]),
            "rate-limited call should roll back and re-call provider, got {:?}",
            s2.actions
        );
        assert!(
            s2.observations
                .iter()
                .any(|o| matches!(o, KernelObservation::Rollbacked { .. })),
            "expected a Rollbacked observation for the rate-limited turn",
        );
    }

    #[test]
    fn governance_constraint_required_param_denies() {
        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        runtime.step(KernelInput::new(KernelInputEvent::LoadGovernancePolicy {
            default_action: Some(PolicyAction::Allow),
            rules: vec![],
            vetoed_tools: vec![],
            rate_limits: vec![],
            constraints: vec![ConstraintSpec::Required {
                tool: "write".to_string(),
                path: "path".to_string(),
            }],
        }));

        // assistant_calling emits empty args `{}` → required "path" is missing → deny.
        let step = run_with_tool_call(&mut runtime, "write");
        assert!(
            matches!(step.actions.as_slice(), [KernelAction::CallProvider { .. }]),
            "missing required param should roll back, got {:?}",
            step.actions
        );
        assert!(
            step.observations
                .iter()
                .any(|o| matches!(o, KernelObservation::Rollbacked { .. })),
            "expected a Rollbacked observation for the constraint violation",
        );
    }

    // ─── In-kernel signal routing (attention policy) ────────────────────────

    fn signal(urgency: crate::types::signal::Urgency, summary: &str) -> crate::types::signal::RuntimeSignal {
        use crate::types::signal::{RuntimeSignal, SignalSource, SignalType};
        RuntimeSignal::new(SignalSource::Gateway, SignalType::Alert, urgency, summary)
    }

    fn started_runtime_with_attention(max_queue: u32) -> KernelRuntime {
        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        runtime.step(KernelInput::new(KernelInputEvent::SetAttentionPolicy {
            max_queue_size: max_queue,
        }));
        runtime.step(KernelInput::new(KernelInputEvent::StartRun {
            task: RuntimeTask::new("watch for signals"),
            run_spec: None,
        }));
        runtime.state_machine_mut().take_observations();
        runtime
    }

    #[test]
    fn attention_policy_critical_signal_interrupts() {
        use crate::types::signal::Urgency;
        let mut runtime = started_runtime_with_attention(8);
        let step = runtime.step(KernelInput::new(KernelInputEvent::Signal {
            signal: signal(Urgency::Critical, "fire"),
        }));
        assert!(
            matches!(step.actions.as_slice(), [KernelAction::CallProvider { .. }]),
            "critical signal should drive a provider call, got {:?}",
            step.actions
        );
        assert!(step.observations.iter().any(|o| matches!(
            o,
            KernelObservation::SignalDisposed { disposition, .. } if disposition == "interrupt_now"
        )));
    }

    #[test]
    fn attention_policy_normal_signal_queues_without_action() {
        use crate::types::signal::Urgency;
        let mut runtime = started_runtime_with_attention(8);
        let step = runtime.step(KernelInput::new(KernelInputEvent::Signal {
            signal: signal(Urgency::Normal, "job"),
        }));
        assert!(
            step.actions.is_empty(),
            "normal signal should queue without a provider call, got {:?}",
            step.actions
        );
        assert!(step.observations.iter().any(|o| matches!(
            o,
            KernelObservation::SignalDisposed { disposition, queue_depth, .. }
            if disposition == "queue" && *queue_depth == 1
        )));
    }

    #[test]
    fn attention_policy_full_queue_drops() {
        use crate::types::signal::Urgency;
        let mut runtime = started_runtime_with_attention(1);
        runtime.step(KernelInput::new(KernelInputEvent::Signal {
            signal: signal(Urgency::Normal, "first"),
        }));
        let step = runtime.step(KernelInput::new(KernelInputEvent::Signal {
            signal: signal(Urgency::Normal, "second"),
        }));
        assert!(step.observations.iter().any(|o| matches!(
            o,
            KernelObservation::SignalDisposed { disposition, .. } if disposition == "dropped"
        )));
    }

    #[test]
    #[test]
    fn page_in_populates_knowledge_partition() {
        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        runtime.step(KernelInput::new(KernelInputEvent::SetMemoryEnabled {
            enabled: true,
        }));
        let before = runtime.state_machine().ctx.partitions.knowledge.len();
        runtime.step(KernelInput::new(KernelInputEvent::PageIn {
            entries: vec![crate::mm::PageInEntry {
                content: "[memory] prior fix".to_string(),
                tokens: Some(10),
                source: Some("memory".to_string()),
                key: None,
                pinned: false,
            }],
        }));
        let after = runtime.state_machine().ctx.partitions.knowledge.len();
        assert!(after > before, "page-in should add knowledge messages");
    }

    #[test]
    fn memory_tool_does_not_emit_page_in_requested() {
        // The automatic PageInRequested producer for live memory/knowledge tool calls was retired:
        // a memory-tool result now flows to `history` via the normal tool-result path only, so it
        // decays with the compression pyramid instead of living forever in `knowledge`.
        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        runtime.step(KernelInput::new(KernelInputEvent::SetMemoryEnabled {
            enabled: true,
        }));
        runtime.step(KernelInput::new(KernelInputEvent::StartRun {
            task: RuntimeTask::new("test"),
            run_spec: None,
        }));
        runtime.state_machine_mut().take_observations();

        let step = run_with_tool_call(&mut runtime, "memory");
        assert!(!step.observations.iter().any(|o| matches!(o, KernelObservation::PageInRequested { .. })));
    }

    #[test]
    fn load_workflow_input_drives_dag_to_completion() {
        use crate::orchestration::workflow::fanout_synthesize;
        use crate::types::result::{LoopResult, SubAgentResult, TerminationReason};

        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        runtime.step(KernelInput::new(KernelInputEvent::StartRun {
            task: RuntimeTask::new("parent task"),
            run_spec: None,
        }));
        runtime.state_machine_mut().take_observations();

        // Exercise the full serde round-trip of LoadWorkflow + WorkflowSpec over the ABI.
        let spec =
            fanout_synthesize(vec![RuntimeTask::new("w0"), RuntimeTask::new("w1")], RuntimeTask::new("synth"));
        let event = KernelInputEvent::LoadWorkflow {
            spec,
            parent_session_id: "sess".to_string(),
            resumed_completed: Vec::new(),
            resumed_submissions: Vec::new(),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let parsed: KernelInputEvent = serde_json::from_str(&json).expect("deserialize");

        let step = runtime.step(KernelInput::new(parsed));
        // First batch carries both workers' goals so the SDK can run them.
        let batch = step
            .observations
            .iter()
            .find_map(|o| match o {
                KernelObservation::WorkflowBatchSpawned { nodes, .. } => Some(nodes.clone()),
                _ => None,
            })
            .expect("workflow_batch_spawned");
        assert_eq!(batch.len(), 2);
        let goals: Vec<&str> = batch.iter().map(|n| n.goal.as_str()).collect();
        assert!(goals.contains(&"w0") && goals.contains(&"w1"));
        assert_eq!(batch[0].agent_id, "wf-node0");
        assert_eq!(batch[0].isolation, "read_only"); // fanout workers are Explore → read_only

        let complete = |runtime: &mut KernelRuntime, id: &str| {
            runtime.step(KernelInput::new(KernelInputEvent::SubAgentCompleted {
                result: SubAgentResult {
                    agent_id: compact_str::CompactString::new(id),
                    result: LoopResult {
                        termination: TerminationReason::Completed,
                        final_message: None,
                        turns_used: 1,
                        total_tokens_used: 1,
                        loop_continue: None,
                        classify_branch: None,
                        tournament_winner: None,
                    },
                },
            }))
        };

        complete(&mut runtime, "wf-node0");
        // After both workers, synth becomes the next batch.
        let step = complete(&mut runtime, "wf-node1");
        assert!(step.observations.iter().any(|o| matches!(
            o,
            KernelObservation::WorkflowBatchSpawned { nodes, .. }
                if nodes.len() == 1 && nodes[0].agent_id == "wf-node2"
        )));

        // Synth completes → workflow finishes.
        let step = complete(&mut runtime, "wf-node2");
        assert!(step.observations.iter().any(|o| matches!(
            o,
            KernelObservation::WorkflowCompleted { completed, .. } if completed.len() == 3
        )));
    }

    #[test]
    fn load_workflow_self_bootstraps_with_no_prior_start_run() {
        // K1: a stateless `runWorkflow` caller fires `LoadWorkflow` with NO preceding `StartRun`. The
        // host path now self-bootstraps the run (parity with the agent-reachable `SubmitWorkflow`), so
        // the DAG installs and drives to completion exactly as the started-path test above.
        use crate::orchestration::workflow::fanout_synthesize;
        use crate::types::result::{LoopResult, SubAgentResult, TerminationReason};

        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        // NOTE: deliberately no StartRun here — that is the whole point of K1.

        let spec =
            fanout_synthesize(vec![RuntimeTask::new("w0"), RuntimeTask::new("w1")], RuntimeTask::new("synth"));
        let step = runtime.step(KernelInput::new(KernelInputEvent::LoadWorkflow {
            spec,
            parent_session_id: "sess".to_string(),
            resumed_completed: Vec::new(),
            resumed_submissions: Vec::new(),
        }));
        let batch = step
            .observations
            .iter()
            .find_map(|o| match o {
                KernelObservation::WorkflowBatchSpawned { nodes, .. } => Some(nodes.clone()),
                _ => None,
            })
            .expect("workflow_batch_spawned even without a prior StartRun");
        assert_eq!(batch.len(), 2);

        let complete = |runtime: &mut KernelRuntime, id: &str| {
            runtime.step(KernelInput::new(KernelInputEvent::SubAgentCompleted {
                result: SubAgentResult {
                    agent_id: compact_str::CompactString::new(id),
                    result: LoopResult {
                        termination: TerminationReason::Completed,
                        final_message: None,
                        turns_used: 1,
                        total_tokens_used: 1,
                        loop_continue: None,
                        classify_branch: None,
                        tournament_winner: None,
                    },
                },
            }))
        };
        complete(&mut runtime, "wf-node0");
        complete(&mut runtime, "wf-node1");
        let step = complete(&mut runtime, "wf-node2");
        assert!(step.observations.iter().any(|o| matches!(
            o,
            KernelObservation::WorkflowCompleted { completed, .. } if completed.len() == 3
        )));
    }

    #[test]
    fn submit_workflow_nodes_input_appends_a_node_over_the_abi() {
        // R3-1: exercise the full serde round-trip of SubmitWorkflowNodes + WorkflowNode over the
        // ABI, and confirm the appended node spawns as a workflow batch mid-run.
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;
        use crate::types::result::{LoopResult, SubAgentResult, TerminationReason};

        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        runtime.step(KernelInput::new(KernelInputEvent::StartRun {
            task: RuntimeTask::new("parent task"),
            run_spec: None,
        }));
        runtime.state_machine_mut().take_observations();

        // A single-node workflow: wf-node0 spawns first.
        let spec = WorkflowSpec::new(vec![WorkflowNode::new(
            RuntimeTask::new("root"),
            AgentRole::Implement,
        )]);
        runtime.step(KernelInput::new(KernelInputEvent::LoadWorkflow {
            spec,
            parent_session_id: "sess".to_string(),
            resumed_completed: Vec::new(),
            resumed_submissions: Vec::new(),
        }));
        runtime.state_machine_mut().take_observations();

        // Submit a node over the ABI while wf-node0 runs (full serde round-trip).
        let event = KernelInputEvent::SubmitWorkflowNodes {
            nodes: vec![WorkflowNode::new(RuntimeTask::new("more"), AgentRole::Implement)],
            submitter_agent_id: None,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let parsed: KernelInputEvent = serde_json::from_str(&json).expect("deserialize");
        let step = runtime.step(KernelInput::new(parsed));
        // The appended node spawns as wf-node1 in a workflow batch.
        assert!(step.observations.iter().any(|o| matches!(
            o,
            KernelObservation::WorkflowBatchSpawned { nodes, .. }
                if nodes.len() == 1 && nodes[0].agent_id == "wf-node1" && nodes[0].goal == "more"
        )));

        let complete = |runtime: &mut KernelRuntime, id: &str| {
            runtime.step(KernelInput::new(KernelInputEvent::SubAgentCompleted {
                result: SubAgentResult {
                    agent_id: compact_str::CompactString::new(id),
                    result: LoopResult {
                        termination: TerminationReason::Completed,
                        final_message: None,
                        turns_used: 1,
                        total_tokens_used: 1,
                        loop_continue: None,
                        classify_branch: None,
                        tournament_winner: None,
                    },
                },
            }))
        };
        complete(&mut runtime, "wf-node0");
        // The workflow finishes only after the submitted node also completes (2 nodes total).
        let step = complete(&mut runtime, "wf-node1");
        assert!(step.observations.iter().any(|o| matches!(
            o,
            KernelObservation::WorkflowCompleted { completed, .. } if completed.len() == 2
        )));
    }

    #[test]
    fn submit_workflow_input_bootstraps_a_dag_over_the_abi() {
        // M5/G1: a top-level agent authors a whole spec over the ABI (full serde round-trip of
        // SubmitWorkflow + WorkflowSpec) with no workflow active → the kernel bootstraps and drives it.
        use crate::orchestration::workflow::{WorkflowNode, WorkflowSpec};
        use crate::types::agent::AgentRole;
        use crate::types::result::{LoopResult, SubAgentResult, TerminationReason};

        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        runtime.step(KernelInput::new(KernelInputEvent::StartRun {
            task: RuntimeTask::new("parent task"),
            run_spec: None,
        }));
        runtime.state_machine_mut().take_observations();

        // No LoadWorkflow first — the agent itself authors the spec.
        let spec = WorkflowSpec::new(vec![WorkflowNode::new(
            RuntimeTask::new("authored root"),
            AgentRole::Implement,
        )]);
        let event = KernelInputEvent::SubmitWorkflow {
            spec,
            parent_session_id: "sess".to_string(),
            submitter_agent_id: None,
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let parsed: KernelInputEvent = serde_json::from_str(&json).expect("deserialize");
        let step = runtime.step(KernelInput::new(parsed));
        // The authored node bootstraps as wf-node0 in a workflow batch.
        assert!(step.observations.iter().any(|o| matches!(
            o,
            KernelObservation::WorkflowBatchSpawned { nodes, .. }
                if nodes.len() == 1 && nodes[0].agent_id == "wf-node0" && nodes[0].goal == "authored root"
        )));

        let step = runtime.step(KernelInput::new(KernelInputEvent::SubAgentCompleted {
            result: SubAgentResult {
                agent_id: compact_str::CompactString::new("wf-node0"),
                result: LoopResult {
                    termination: TerminationReason::Completed,
                    final_message: None,
                    turns_used: 1,
                    total_tokens_used: 1,
                    loop_continue: None,
                    classify_branch: None,
                    tournament_winner: None,
                },
            },
        }));
        assert!(step.observations.iter().any(|o| matches!(
            o,
            KernelObservation::WorkflowCompleted { completed, .. } if completed.len() == 1
        )));
    }

    #[test]
    fn load_workflow_resumes_from_completed_nodes() {
        use crate::orchestration::workflow::fanout_synthesize;

        let mut runtime = KernelRuntime::new(LoopPolicy::default());
        runtime.step(KernelInput::new(KernelInputEvent::StartRun {
            task: RuntimeTask::new("parent task"),
            run_spec: None,
        }));
        runtime.state_machine_mut().take_observations();

        // Resume a 2-worker fanout where worker 0 already completed before the interruption.
        let spec =
            fanout_synthesize(vec![RuntimeTask::new("w0"), RuntimeTask::new("w1")], RuntimeTask::new("synth"));
        let step = runtime.step(KernelInput::new(KernelInputEvent::LoadWorkflow {
            spec,
            parent_session_id: "sess".to_string(),
            resumed_completed: vec!["wf-node0".to_string()],
            resumed_submissions: Vec::new(),
        }));

        // Only the remaining worker is re-spawned (node 0 is not re-run).
        let batch = step
            .observations
            .iter()
            .find_map(|o| match o {
                KernelObservation::WorkflowBatchSpawned { nodes, .. } => Some(nodes.clone()),
                _ => None,
            })
            .expect("workflow_batch_spawned");
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].agent_id, "wf-node1");
    }
}
