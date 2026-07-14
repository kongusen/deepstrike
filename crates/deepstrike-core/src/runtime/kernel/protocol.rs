use super::*;
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
    /// ③ loop-agent: rounds completed across the loop before this run (seeds the
    /// pacing trap's max_rounds coercion). Additive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_rounds_base: Option<u32>,
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
    Resume {
        // COMPAT(sched-resume-generic): old SDKs send `{kind:"resume"}` with no
        // fields — serde(default) deserialises to empty vecs. Change to required
        // once all SDKs supply approved/denied explicitly.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        approved_calls: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        denied_calls: Vec<String>,
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
    Timeout,
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
    pub actions: Vec<KernelAction>,
    pub observations: Vec<KernelObservation>,
}

impl KernelStep {
    pub(super) fn empty(observations: Vec<KernelObservation>) -> Self {
        Self {
            version: KERNEL_ABI_VERSION,
            actions: Vec::new(),
            observations,
        }
    }

    pub(super) fn single(action: LoopAction, observations: Vec<KernelObservation>) -> Self {
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
    /// One compaction = one observation. `archived` non-empty ⇒ content left working
    /// context (what the retired separate PageOut observation used to duplicate);
    /// `tier_hint` then names the recommended long-term tier for the archived batch.
    Compressed {
        #[serde(default)]
        turn: u32,
        action: KernelPressureAction,
        rho_after: f64,
        summary: Option<String>,
        archived: Vec<Message>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tier_hint: Option<String>,
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
    /// An inbound signal was routed by the in-kernel attention policy.
    SignalDisposed {
        turn: u32,
        signal_id: String,
        disposition: String,
        queue_depth: u32,
    },
    /// A budget axis (turns / tokens / wall-time) was exhausted.
    BudgetExceeded {
        turn: u32,
        budget: String,
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
