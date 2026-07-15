use super::*;
pub const KERNEL_ABI_VERSION: u32 = 2;
pub const KERNEL_SNAPSHOT_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KernelLifecycle {
    Created,
    Configured,
    Running,
    Suspended,
    Completed,
    Cancelled,
    Failed,
}

impl KernelLifecycle {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Failed)
    }
}

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
/// pattern/predicate matching stays in the SDK execution layer).
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
    pub operation_id: String,
    pub event_id: String,
    pub observed_at_ms: u64,
    pub event: KernelInputEvent,
}

impl KernelInput {
    /// Build an in-process input for callers that do not cross a durable wire boundary.
    /// Wire hosts should use [`Self::correlated`] with their durable identities.
    pub fn new(event: KernelInputEvent) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static LOCAL_EVENT_SEQUENCE: AtomicU64 = AtomicU64::new(1);
        let event_seq = LOCAL_EVENT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        Self::correlated(
            "local-operation",
            format!("local-event-{event_seq}"),
            0,
            event,
        )
    }

    pub fn correlated(
        operation_id: impl Into<String>,
        event_id: impl Into<String>,
        observed_at_ms: u64,
        event: KernelInputEvent,
    ) -> Self {
        Self {
            version: KERNEL_ABI_VERSION,
            operation_id: operation_id.into(),
            event_id: event_id.into(),
            observed_at_ms,
            event,
        }
    }
}

/// Outcome of staging one kernel input before the host's durable commit boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KernelPreparationStatus {
    /// A new accepted transition is staged and must be committed or aborted with `prepare_token`.
    Prepared,
    /// The exact event was already committed; no new durable transaction is required.
    Replayed,
    /// The input was rejected and did not stage any runtime state.
    Rejected,
}

/// Host-visible description of a staged transition. The candidate runtime state remains opaque
/// inside [`KernelRuntime`](super::KernelRuntime); hosts persist `input` plus `step` before using the
/// one-shot token to publish it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KernelPreparedStep {
    pub status: KernelPreparationStatus,
    /// Committed runtime generation used to plan this outcome.
    pub base_generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepare_token: Option<String>,
    pub input: KernelInput,
    pub step: KernelStep,
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

/// Host-selectable reliability policy. These values bound retained replay
/// state and retry ladders; omitted fields keep the kernel defaults.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KernelReliabilityConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_replay_capacity: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_effect_replay_capacity: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_recovery_attempts: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_recovery_attempts: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_effect_retry_attempts: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spool_threshold_bytes: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spool_preview_bytes: Option<u32>,
    /// Maximum accepted ABI inputs retained for a portable snapshot rebuild.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_input_limit: Option<usize>,
    /// Maximum canonical JSON bytes accepted for one ABI input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_input_bytes: Option<usize>,
    /// Maximum canonical JSON bytes retained by the portable snapshot journal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_journal_bytes_limit: Option<usize>,
}

/// Read-only runtime resource projection. Hosts use this for admission and monitoring; mutating
/// kernel state still requires a versioned input transaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelDiagnostics {
    pub lifecycle: KernelLifecycle,
    pub next_step_seq: u64,
    pub accepted_input_count: usize,
    pub accepted_input_bytes: usize,
    pub snapshot_input_limit: usize,
    pub snapshot_journal_bytes_limit: usize,
    pub max_input_bytes: usize,
    pub snapshot_overflowed: bool,
    pub recorded_event_count: usize,
    pub completed_effect_count: usize,
    pub pending_effect_count: usize,
}

/// Portable runtime checkpoint. State is rebuilt from accepted public ABI transactions rather
/// than serializing private scheduler structs, so internal refactors do not change this schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KernelSnapshotV2 {
    pub snapshot_version: u32,
    pub abi_version: u32,
    pub initial_policy: KernelSnapshotPolicyV2,
    pub lifecycle: KernelLifecycle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    pub next_step_seq: u64,
    pub snapshot_input_limit: usize,
    pub max_input_bytes: usize,
    pub snapshot_journal_bytes_limit: usize,
    pub accepted_input_bytes: usize,
    #[serde(default)]
    pub accepted_inputs: Vec<KernelInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_step: Option<KernelStep>,
}

/// JSON-portable scheduler policy. The 64-bit axes use decimal strings so JavaScript hosts do not
/// lose precision while parsing and re-encoding a checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelSnapshotPolicyV2 {
    pub max_tokens: u32,
    pub max_turns: u32,
    pub max_total_tokens: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_wall_ms: Option<String>,
}

impl From<&SchedulerBudget> for KernelSnapshotPolicyV2 {
    fn from(policy: &SchedulerBudget) -> Self {
        Self {
            max_tokens: policy.max_tokens,
            max_turns: policy.max_turns,
            max_total_tokens: policy.max_total_tokens.to_string(),
            max_wall_ms: policy.max_wall_ms.map(|value| value.to_string()),
        }
    }
}

impl TryFrom<&KernelSnapshotPolicyV2> for SchedulerBudget {
    type Error = String;

    fn try_from(policy: &KernelSnapshotPolicyV2) -> Result<Self, Self::Error> {
        Ok(Self {
            max_tokens: policy.max_tokens,
            max_turns: policy.max_turns,
            max_total_tokens: policy.max_total_tokens.parse().map_err(|_| {
                "snapshot max_total_tokens must be a u64 decimal string".to_string()
            })?,
            max_wall_ms: policy
                .max_wall_ms
                .as_deref()
                .map(str::parse)
                .transpose()
                .map_err(|_| "snapshot max_wall_ms must be a u64 decimal string".to_string())?,
        })
    }
}

/// K2: a bundle of run-setup configuration carried by the [`KernelInputEvent::ConfigureRun`] event.
/// Each field maps 1:1 to a granular `Set*` / `Load*` event; `None`/absent leaves that aspect untouched.
/// This is the host-side analogue of the SDK's `applyKernelPolicies` — one event for the whole setup.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reliability: Option<KernelReliabilityConfig>,
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
    /// Stable, replayable context behavior. Ratios use integer ppm on the ABI wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_policy: Option<crate::context::policy::ContextPolicyV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduler_max_wall_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_quota: Option<crate::governance::quota::ResourceQuota>,
    /// RunGroup admission result. The kernel enforces these as local hard limits and reports
    /// terminal usage against the same opaque reservation identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_grant: Option<BudgetGrant>,
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
    /// Entropy watch: opt-in threshold alerting over the per-turn session-entropy score
    /// (see `SetEntropyWatch`). Absent ⇒ kernel default (disabled; sampling itself is
    /// unconditional and unaffected).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entropy_watch: Option<crate::scheduler::entropy::EntropyWatchConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetGrant {
    pub reservation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagents: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rounds: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancellationReason {
    User,
    Deadline,
    LeaseLost,
    HostShutdown,
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
            ConstraintSpec::Enum { tool, path, values } => {
                (tool, path, ConstraintRule::Enum(values))
            }
            ConstraintSpec::Range {
                tool,
                path,
                min,
                max,
            } => (tool, path, ConstraintRule::Range { min, max }),
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
        /// K3: auto-deactivate after this many turns (`None` = permanent, the default). On expiry
        /// the toolset re-widens and the skill's knowledge pin is boundary-swept — same path as
        /// an explicit `SkillDeactivated`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lease_turns: Option<u32>,
    },
    /// K3: host-driven skill deactivation (there is deliberately NO model-facing unload — it
    /// invites thrash). The toolset re-widens at the next provider call (an epoch event, same
    /// cache cost class as activation); the `skill:<name>` knowledge pin drops at the next
    /// boundary sweep. Errs-open: not-active is a no-op.
    SkillDeactivated {
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
    /// Continue a run reconstructed from preloaded history. Approval resolution
    /// uses the correlated `ApprovalResult` event instead.
    Resume,
    ApprovalResult {
        effect_id: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        approved_calls: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        denied_calls: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// Result of a host-owned workflow spawn batch. Every requested agent must
    /// appear in exactly one of `started_agent_ids` or `failures`.
    WorkflowSpawnResult {
        effect_id: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        started_agent_ids: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        failures: Vec<WorkflowSpawnFailure>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    PreemptResult {
        effect_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    MemoryPersistResult {
        effect_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    MemoryQueryResult {
        effect_id: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        entries: Vec<crate::mm::PageInEntry>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    LargeResultSpoolResult {
        effect_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spool_ref: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    PageOutArchiveResult {
        effect_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        archive_ref: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    /// K2: set the knowledge-budget ratio at runtime (granular sibling of
    /// `RunConfig::knowledge_budget_ratio`). `0.0` disables the cap.
    SetKnowledgeBudget {
        ratio: f64,
    },
    /// O4: enable/disable the turn-end criteria gate (default enabled; no-op for runs without
    /// criteria). Additive ABI.
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
    /// Entropy watch: tune the opt-in threshold alerting over the per-turn session-entropy
    /// score (defaults: disabled, threshold=0.65, hysteresis=0.1, cooldown_turns=4,
    /// notify_model=false). Each field is optional — an absent field keeps the current
    /// value. Sampling itself is unconditional and unaffected. Additive ABI.
    SetEntropyWatch {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enabled: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        threshold: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hysteresis: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cooldown_turns: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notify_model: Option<bool>,
    },
    /// Adjust the wall-clock budget at runtime (e.g. to extend or set a deadline
    /// after a run has already started). Additive: omit to keep the value from
    /// `SchedulerBudget` passed at construction.
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
        effect_id: String,
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
        effect_id: String,
        results: Vec<ToolResult>,
    },
    /// Reactive recovery entry point: the SDK's provider stream failed. The kernel classifies the
    /// error (context-overflow vs other) and runs the bounded compact-and-retry recovery ladder,
    /// returning `CallProvider` to retry with a freshly compacted context or `Done` to terminate.
    /// The runners forward the raw provider error text and dispatch the result, instead of each
    /// owning the classify + compact + retry + give-up policy. Additive ABI: a brand-new variant,
    /// byte-identical on the wire for SDKs that never send it.
    ProviderError {
        effect_id: String,
        message: String,
    },
    DeliverSignal {
        delivery_id: String,
        attempt: u32,
        signal: RuntimeSignal,
    },
    MilestoneResult {
        effect_id: String,
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
        /// R3-1: base graph index recorded for each submission batch (parallel to
        /// `resumed_submissions`); absent/short = legacy order-only replay.
        #[serde(default)]
        resumed_submission_bases: Vec<u32>,
        /// W-1: recovered completions WITH their result-borne control signals (classify branch /
        /// loop stop), so a resumed classifier re-prunes and a semantic loop stop is honored.
        /// Additive: SDKs that only send `resumed_completed` (bare ids) get signal-less replay.
        /// When both fields name the same agent id, this one wins.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        resumed_results: Vec<crate::orchestration::workflow::ResumedCompletion>,
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
    /// Privileged host control: commit a host-driven run (for example a standalone workflow)
    /// after its kernel-owned work has completed. This supersedes any pending provider effect and
    /// produces the ordinary `done` effect and terminal usage report.
    CompleteRun,
    /// Host cancellation fact. The host has already stopped external I/O; the kernel commits the
    /// deterministic terminal transition and clears every pending effect/wait state.
    CancelOperation {
        operation_id: String,
        reason: CancellationReason,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pending_call_ids: Vec<String>,
    },
}

fn default_stale_days() -> u32 {
    2
}
fn default_top_k() -> usize {
    5
}
fn default_validation_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KernelStep {
    pub version: u32,
    pub operation_id: String,
    pub input_event_id: String,
    pub step_seq: u64,
    pub actions: Vec<KernelAction>,
    pub observations: Vec<KernelObservation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub faults: Vec<KernelFault>,
}

impl KernelStep {
    pub(super) fn empty(
        operation_id: String,
        input_event_id: String,
        step_seq: u64,
        observations: Vec<KernelObservation>,
    ) -> Self {
        Self {
            version: KERNEL_ABI_VERSION,
            operation_id,
            input_event_id,
            step_seq,
            actions: Vec::new(),
            observations,
            faults: Vec::new(),
        }
    }

    pub(super) fn single(
        operation_id: String,
        input_event_id: String,
        step_seq: u64,
        action: LoopAction,
        observations: Vec<KernelObservation>,
    ) -> Self {
        let effect_id = format!("{operation_id}:step:{step_seq}:effect:0");
        Self {
            version: KERNEL_ABI_VERSION,
            operation_id,
            input_event_id: input_event_id.clone(),
            step_seq,
            actions: vec![KernelAction::from_loop(effect_id, input_event_id, action)],
            observations,
            faults: Vec::new(),
        }
    }

    pub(super) fn fault(
        operation_id: String,
        input_event_id: String,
        step_seq: u64,
        fault: KernelFault,
    ) -> Self {
        Self {
            version: KERNEL_ABI_VERSION,
            operation_id,
            input_event_id,
            step_seq,
            actions: Vec::new(),
            observations: Vec::new(),
            faults: vec![fault],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KernelFaultCode {
    VersionMismatch,
    OperationMismatch,
    InvalidLifecycle,
    InvalidConfig,
    ResourceLimitExceeded,
    DuplicateEventConflict,
    UnexpectedEffectResult,
    TransactionConflict,
    SnapshotIncompatible,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelFault {
    pub code: KernelFaultCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KernelAction {
    pub effect_id: String,
    pub causation_id: String,
    #[serde(flatten)]
    pub effect: KernelEffect,
}

impl KernelAction {
    fn from_loop(effect_id: String, causation_id: String, action: LoopAction) -> Self {
        Self {
            effect_id,
            causation_id,
            effect: action.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkflowSpawnFailure {
    pub agent_id: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KernelEffect {
    CallProvider {
        context: RenderedContext,
        tools: Vec<ToolSchema>,
    },
    ExecuteTool {
        calls: Vec<ToolCall>,
    },
    RequestApproval {
        requests: Vec<crate::scheduler::state_machine::ApprovalRequest>,
    },
    SpawnWorkflow {
        nodes: Vec<crate::orchestration::workflow::WorkflowSpawnInfo>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        budget: Option<crate::orchestration::workflow::WorkflowBudget>,
    },
    PreemptSubAgents {
        agent_ids: Vec<String>,
        reason: String,
    },
    PersistMemory {
        memory: crate::mm::memory::MemoryWriteRequest,
    },
    QueryMemory {
        query: crate::mm::memory::MemoryQuery,
        requested_k: usize,
    },
    SpoolLargeResult {
        call_id: String,
        tool: String,
        output: String,
        original_size: u32,
        preview_size: u32,
    },
    ArchivePageOut {
        turn: u32,
        action: KernelPressureAction,
        summary: Option<String>,
        archived: Vec<Message>,
        tier: String,
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

impl From<LoopAction> for KernelEffect {
    fn from(action: LoopAction) -> Self {
        match action {
            LoopAction::AwaitingResume => {
                panic!("AwaitingResume must not be converted to KernelEffect")
            }
            LoopAction::CallLLM { context, tools } => Self::CallProvider { context, tools },
            LoopAction::ExecuteTools { calls } => Self::ExecuteTool { calls },
            LoopAction::RequestApproval { requests } => Self::RequestApproval { requests },
            LoopAction::SpawnWorkflow { nodes, budget } => Self::SpawnWorkflow { nodes, budget },
            LoopAction::PreemptSubAgents { agent_ids, reason } => {
                Self::PreemptSubAgents { agent_ids, reason }
            }
            LoopAction::PersistMemory { memory } => Self::PersistMemory { memory },
            LoopAction::QueryMemory { query, requested_k } => {
                Self::QueryMemory { query, requested_k }
            }
            LoopAction::SpoolLargeResult {
                call_id,
                tool,
                output,
                original_size,
                preview_size,
            } => Self::SpoolLargeResult {
                call_id,
                tool,
                output,
                original_size,
                preview_size,
            },
            LoopAction::ArchivePageOut {
                turn,
                action,
                summary,
                archived,
                tier,
            } => Self::ArchivePageOut {
                turn,
                action,
                summary,
                archived,
                tier,
            },
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
    /// Synchronous in-kernel compaction fact. Archived content is carried only by
    /// `ArchivePageOut`; it never rides an observation into host I/O.
    Compressed {
        #[serde(default)]
        turn: u32,
        action: KernelPressureAction,
        rho_after: f64,
        summary: Option<String>,
        archived_count: u32,
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
    /// Session-entropy sample at a completed turn boundary (the heartbeat watch source).
    /// One per completed turn, unconditional — like `CheckpointTaken`. The component
    /// vector is the contract; `score` is a versioned default fold (`score_version`).
    /// See `scheduler::entropy`. Additive ABI.
    EntropySample {
        turn: u32,
        score: f64,
        score_version: u32,
        rho: f64,
        repeat_pressure: f64,
        failure_rate: f64,
        rollbacks_in_window: u32,
        window_turns: u32,
    },
    /// The opt-in entropy watch tripped: `score` crossed `threshold` while armed and
    /// cooled down (`EntropyWatchConfig`). Correlate components via the same-turn
    /// `EntropySample`. Additive ABI.
    EntropyAlert {
        turn: u32,
        score: f64,
        threshold: f64,
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
    /// The host could not resolve a workflow spawn effect. No node is recorded
    /// as started; the same logical batch remains pending for retry.
    WorkflowSpawnFailed {
        turn: u32,
        error: String,
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
    AgentPreemptFailed {
        turn: u32,
        agent_ids: Vec<String>,
        reason: String,
        error: String,
    },
    /// ③ loop-agent pacing: the kernel adjudicated a `pace` proposal for this round.
    RoundPaced {
        turn: u32,
        round: u32,
        decision: crate::types::result::PaceDecision,
    },
    /// R3-1: a runtime node submission was appended to the in-flight DAG at `base`
    /// (the graph length before the append). The SDK records `base` on the
    /// `workflow_nodes_submitted` session event so resume can re-apply the batch at
    /// the exact original indices (gap-filling any interleaved runtime children).
    WorkflowNodesSubmitted {
        turn: u32,
        base: u32,
        count: u32,
        /// W-N3: the submitting node's agent id (`None` = host/bootstrap). Persisted so resume can
        /// DROP batches whose submitter re-runs (it will re-submit) instead of duplicating them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        submitter: Option<String>,
    },
    /// A tool call needs user approval (governance `AskUser`). Not blocked by the
    /// kernel — the SDK must obtain approval before executing the named call.
    ToolGated {
        turn: u32,
        call_id: String,
        tool: String,
        reason: String,
    },
    /// A leased inbound signal delivery was routed by the in-kernel attention policy.
    SignalDeliveryDisposed {
        turn: u32,
        operation_id: String,
        delivery_id: String,
        attempt: u32,
        signal_id: String,
        disposition: String,
        queue_depth: u32,
    },
    /// A budget axis (turns / tokens / wall-time) was exhausted.
    BudgetExceeded {
        turn: u32,
        budget: String,
        operation_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reservation_id: Option<String>,
    },
    /// Terminal local usage for one reservation. Emitted exactly once per operation.
    BudgetUsageReported {
        operation_id: String,
        reservation_id: String,
        tokens: u64,
        subagents: u32,
        rounds: u32,
    },
    /// A host cancellation was committed. Emitted exactly once by the accepted cancellation step.
    OperationCancelled {
        turn: u32,
        operation_id: String,
        reason: CancellationReason,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pending_call_ids: Vec<String>,
    },
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
    ApprovalResolutionFailed {
        turn: u32,
        error: String,
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
    MemoryWriteFailed {
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
    MemoryQueryFailed {
        turn: u32,
        query_context: String,
        error: String,
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
    LargeResultSpoolFailed {
        turn: u32,
        call_id: String,
        tool: String,
        error: String,
    },
    PageOutArchived {
        turn: u32,
        action: KernelPressureAction,
        summary: Option<String>,
        tier: String,
        message_count: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        archive_ref: Option<String>,
    },
    PageOutArchiveFailed {
        turn: u32,
        action: KernelPressureAction,
        tier: String,
        message_count: u32,
        error: String,
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
