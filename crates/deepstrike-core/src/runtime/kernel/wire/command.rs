//! Host control plane (spec §7.5).

use serde::{Deserialize, Serialize};

use super::root::{CapabilityGrant, CapabilityRef, KnowledgeEntry};
use super::scalar::{CallId, WireU64};

/// Live commands a host may issue against a running operation.
///
/// Two rules shape this union:
///
/// 1. **No boot config through the control plane.** Setup-only configuration travels once,
///    through `ConfigureOperation`; a live command exists only where a real callsite proved a
///    running operation must change.
/// 2. **Authority is explicit.** `UpdateTask` here is the *host's* plan update. The model's
///    `update_task` is a P1 syscall whose causation the kernel derives from the provider effect
///    (§7.6) — the two must not share a wire variant, because sharing one is exactly how the
///    current ABI lost the ability to tell a host plan edit from a model tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostCommand {
    Cancel(CancelCommand),
    ForceCompact(ForceCompactCommand),
    UpdateTask(UpdateTaskCommand),
    ApplyCapabilityPatch(ApplyCapabilityPatchCommand),
    ApplyKnowledgeMutation(ApplyKnowledgeMutationCommand),
    /// DEC-9: the host seeding entries into the knowledge partition. Named apart from the P1
    /// syscall `PageIn { handle_id }` on purpose — the two are opposite directions and must never
    /// share a name again.
    SeedKnowledge(SeedKnowledgeCommand),
    /// §13.2 · skill activation/deactivation. Host-side only for deactivation: there is
    /// deliberately no model-facing unload (it invites thrash), and the model's *activation* is
    /// the P1 `ActivateSkill` syscall, whose authority the kernel derives rather than accepts.
    ApplySkillActivation(ApplySkillActivationCommand),
    ApplyPolicyPatch(ApplyPolicyPatchCommand),
    UpdateDeadline(UpdateDeadlineCommand),
}

/// Cancellation. The operation id lives in the envelope and is **not** repeated here: the
/// historical duplicate field forced every SDK to special-case cancel when building an input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelCommand {
    pub reason: CancellationReason,
    /// Logical calls the host wants abandoned. One id namespace only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_call_ids: Vec<CallId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancellationReason {
    User,
    Deadline,
    LeaseLost,
    HostShutdown,
}

/// Empty payload rather than a unit variant so `deny_unknown_fields` still applies.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForceCompactCommand {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateTaskCommand {
    pub update: TaskUpdate,
}

/// A partial edit of the task state. Every field is optional; absent ⇒ unchanged.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_step: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scratchpad: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_on: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preserved_refs: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directives: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyCapabilityPatchCommand {
    pub patch: CapabilityPatch,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityPatch {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mount: Vec<CapabilityGrant>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unmount: Vec<CapabilityRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyKnowledgeMutationCommand {
    pub mutation: KnowledgeMutation,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeMutation {
    /// Keyed upsert; entries without a key append.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub upsert: Vec<KnowledgeEntry>,
    /// Keys to drop at the next boundary. Errs open: an unknown key is a no-op.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SeedKnowledgeCommand {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<KnowledgeEntry>,
}

/// §13.2 · skill activation state. Both directions in one command so a swap is atomic.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplySkillActivationCommand {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub activate: Vec<SkillActivation>,
    /// Skill names to deactivate. Errs open: not-active is a no-op.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deactivate: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillActivation {
    pub name: String,
    /// Auto-deactivate after this many turns. `None` ⇒ until explicitly deactivated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_turns: Option<u32>,
}

/// Optimistic policy update. The revision is mandatory: a patch without one silently overwrites
/// whatever another writer just installed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyPolicyPatchCommand {
    pub expected_revision: WireU64,
    pub patch: LivePolicyPatch,
}

/// The closed set of policies that may change while an operation runs (§13.2).
///
/// Everything absent from this union is boot-only by construction. Optimistic concurrency belongs
/// to [`ApplyPolicyPatchCommand::expected_revision`], not to a policy-format discriminator.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LivePolicyPatch {
    ReplaceSignalPolicy(ReplaceSignalPolicy),
    ReplaceGovernancePolicy(ReplaceGovernancePolicy),
    TightenResourceQuota(TightenResourceQuota),
    ReplaceRecoveryPolicy(ReplaceRecoveryPolicy),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplaceSignalPolicy {
    pub policy: SignalPolicy,
}

/// Signal routing policy. Replaced atomically — a partial signal policy is not a thing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignalPolicy {
    pub queue_max: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_ms: Option<WireU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_escalation: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplaceGovernancePolicy {
    pub policy: GovernancePolicy,
}

/// Syscall-gate policy — the complete posture, replaced atomically.
///
/// The same type is the operation's initial governance (§13.1) and the payload of its live
/// governance mutation (§13.2). There is one wire shape; authority is carried by the input class
/// that transports it — `ConfigureOperation` once, or a revision-guarded
/// [`ApplyPolicyPatchCommand`] afterwards.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GovernancePolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_action: Option<PolicyAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<PolicyRule>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub vetoed_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rate_limits: Vec<RateLimitSpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<ParamConstraint>,
}

/// Per-tool rolling-window rate limit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateLimitSpec {
    pub tool: String,
    pub max_calls: u32,
    pub window_ms: WireU64,
}

/// Structural constraint on a tool call's parameters. Pattern/predicate matching stays in the host
/// execution layer — the kernel only enforces shapes it can replay identically.
///
/// The addressed field is `param_path`, not `path`: it points inside the call's *arguments*, and
/// a bare `path` in a kernel input is exactly the host filesystem fact §7.4 keeps out of the ABI.
///
/// [`RangeParam`] carries its bounds as **fixed-point micro-units** rather than `f64`: a range
/// check is an authoritative branch, and §7.1.1 keeps language-default floats out of those.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ParamConstraint {
    Required(RequiredParam),
    Enum(EnumParam),
    Range(RangeParam),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequiredParam {
    pub tool: String,
    pub param_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnumParam {
    pub tool: String,
    pub param_path: String,
    pub values: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RangeParam {
    pub tool: String,
    pub param_path: String,
    /// Inclusive lower bound in micro-units (`1_500_000` ⇒ `1.5`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_micros: Option<i64>,
    /// Inclusive upper bound in micro-units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_micros: Option<i64>,
}

impl ParamConstraint {
    pub fn tool(&self) -> &str {
        match self {
            Self::Required(c) => &c.tool,
            Self::Enum(c) => &c.tool,
            Self::Range(c) => &c.tool,
        }
    }

    pub fn param_path(&self) -> &str {
        match self {
            Self::Required(c) => &c.param_path,
            Self::Enum(c) => &c.param_path,
            Self::Range(c) => &c.param_path,
        }
    }

    /// Structural self-consistency. A constraint that can never hold is a configuration error,
    /// not a permanently-denying gate discovered on the first tool call.
    pub fn validate(&self) -> Result<(), String> {
        if self.tool().is_empty() {
            return Err("governance constraint tool must not be empty".to_string());
        }
        if self.param_path().is_empty() {
            return Err("governance constraint param_path must not be empty".to_string());
        }
        match self {
            Self::Required(_) => Ok(()),
            Self::Enum(c) => {
                if c.values.is_empty() {
                    return Err(format!(
                        "enum constraint on {}.{} lists no permitted value, so every call is denied",
                        c.tool, c.param_path
                    ));
                }
                Ok(())
            }
            Self::Range(c) => match (c.min_micros, c.max_micros) {
                (None, None) => Err(format!(
                    "range constraint on {}.{} bounds nothing",
                    c.tool, c.param_path
                )),
                (Some(min), Some(max)) if min > max => Err(format!(
                    "range constraint on {}.{} has min_micros {min} above max_micros {max}",
                    c.tool, c.param_path
                )),
                _ => Ok(()),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyAction {
    Allow,
    Deny,
    AskUser,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyRule {
    pub tool_pattern: String,
    pub action: PolicyAction,
}

/// Quotas only ever shrink at runtime. Growing one is a reservation fact, not a policy patch, and
/// needs its own contract before it exists.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TightenResourceQuota {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent_subagents: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_total_subagents: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_spawn_depth: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_workflow_nodes: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplaceRecoveryPolicy {
    pub policy: RecoveryPolicy,
}

/// Semantic recovery ladders the kernel owns. Host transport retry/backoff stays host-side.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_recovery_attempts: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_recovery_attempts: Option<u8>,
    /// §12.3 · how much journal tail this operation may carry between checkpoints. Boot-only and
    /// frozen in the genesis record, because the bound a run was refused against must survive a
    /// binary whose defaults moved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail_bounds: Option<TailBoundsPolicy>,
}

/// Sparse override of [`TailBounds`](super::config::TailBounds). Each axis is independent: a host
/// that only wants to shorten the record watermark keeps the kernel's byte bounds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TailBoundsPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soft_records: Option<WireU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hard_records: Option<WireU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soft_bytes: Option<WireU64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hard_bytes: Option<WireU64>,
}

/// Absolute deadline for the operation. `None` clears it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateDeadlineCommand {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<WireU64>,
}

// ---------------------------------------------------------------------------------------------
// live policy state (§13.2, DEC-6)
// ---------------------------------------------------------------------------------------------

/// The kernel-side live policy state: the resolved configuration plus the revision that guards
/// mutations of it.
///
/// Two rules live here rather than at each callsite:
///
/// 1. **Optimistic concurrency belongs to the patch, not to a policy.** DEC-6 removed
///    `SIGNAL_POLICY_VERSION` and `SignalPolicyConfig.version`; a signal policy has no version of
///    its own, and the single revision counter below is what two concurrent writers race on.
///    A patch whose `expected_revision` does not match the current one is refused, so the second
///    writer re-reads instead of silently overwriting the first.
/// 2. **Live resource changes only ever shrink.** Growing a quota is a *reservation fact* — it
///    needs an admission decision from whoever owns the budget — not a policy edit, so it has no
///    representation here.
#[derive(Debug, Clone, PartialEq)]
pub struct LivePolicyState {
    revision: WireU64,
    config: super::config::ResolvedOperationConfig,
}

impl LivePolicyState {
    /// Start from the genesis record's resolved configuration at revision 0.
    pub fn new(config: super::config::ResolvedOperationConfig) -> Self {
        Self {
            revision: WireU64::ZERO,
            config,
        }
    }

    /// §12.2 · reinstall the live policy a checkpoint recorded, revision included.
    ///
    /// Distinct from [`Self::new`] on purpose: `new` starts a *fresh* operation at revision 0 with
    /// the genesis configuration, and using it for a restore would silently rewind the revision a
    /// concurrent patch writer is racing on.
    pub fn restore(revision: WireU64, config: super::config::ResolvedOperationConfig) -> Self {
        Self { revision, config }
    }

    pub fn revision(&self) -> WireU64 {
        self.revision
    }

    pub fn config(&self) -> &super::config::ResolvedOperationConfig {
        &self.config
    }

    /// Apply one revision-guarded patch.
    ///
    /// Atomic in the same structural sense as configuration resolution: the next configuration is
    /// built and validated as a whole *before* anything is stored, so a rejected patch leaves both
    /// the configuration and the revision exactly as they were.
    pub fn apply(
        &mut self,
        command: &ApplyPolicyPatchCommand,
    ) -> Result<WireU64, super::envelope::WireRejection> {
        use super::envelope::{WireRejection, WireRejectionKind};

        if command.expected_revision != self.revision {
            return Err(WireRejection::new(
                WireRejectionKind::PolicyViolation,
                format!(
                    "policy revision mismatch: patch expects {}, the operation is at {}; \
                     re-read the policy and rebase the patch",
                    command.expected_revision, self.revision
                ),
            ));
        }

        let next = command.patch.apply_to(&self.config)?;
        self.config = next;
        self.revision = WireU64::new(self.revision.get().saturating_add(1));
        Ok(self.revision)
    }
}

impl LivePolicyPatch {
    /// Produce the configuration this patch would install, or reject it. Pure: it never mutates
    /// the input, which is what makes [`LivePolicyState::apply`] all-or-nothing.
    pub fn apply_to(
        &self,
        current: &super::config::ResolvedOperationConfig,
    ) -> Result<super::config::ResolvedOperationConfig, super::envelope::WireRejection> {
        use super::config::{
            ResolvedRecoveryPolicy, ResolvedSignalPolicy, validate_quota, validate_recovery,
            validate_signal,
        };

        let mut next = current.clone();
        match self {
            Self::ReplaceSignalPolicy(patch) => {
                next.signal_policy = ResolvedSignalPolicy {
                    queue_max: patch.policy.queue_max,
                    ttl_ms: patch.policy.ttl_ms,
                    deadline_escalation: patch
                        .policy
                        .deadline_escalation
                        .unwrap_or(current.signal_policy.deadline_escalation),
                };
                validate_signal(&next.signal_policy)?;
            }
            Self::ReplaceGovernancePolicy(patch) => {
                let policy = &patch.policy;
                next.governance_policy.default_action = policy
                    .default_action
                    .unwrap_or(current.governance_policy.default_action);
                next.governance_policy.rules = policy.rules.clone();
                next.governance_policy.vetoed_tools = policy.vetoed_tools.clone();
                next.governance_policy.rate_limits = policy.rate_limits.clone();
                next.governance_policy.constraints = policy.constraints.clone();
                super::config::validate_governance(
                    &next.governance_policy,
                    current.kernel_limits.collection_limits.governance_rules,
                )?;
            }
            Self::TightenResourceQuota(patch) => {
                next.resource_quota = patch.tighten(&current.resource_quota)?;
                validate_quota(&next.resource_quota)?;
            }
            Self::ReplaceRecoveryPolicy(patch) => {
                // §13.1 · the semantic recovery ladders are live-mutable, the tail bound is not.
                // It is frozen in the genesis record (§5e-5), and a live widen would let a run
                // escape the very bound a `CheckpointRequired` just refused it against.
                if patch.policy.tail_bounds.is_some() {
                    return Err(super::envelope::WireRejection::new(
                        super::envelope::WireRejectionKind::PolicyViolation,
                        "recovery_policy.tail_bounds is boot-only: it is frozen in the genesis \
                         record and cannot be patched live",
                    ));
                }
                next.recovery_policy = ResolvedRecoveryPolicy {
                    provider_recovery_attempts: patch
                        .policy
                        .provider_recovery_attempts
                        .unwrap_or(current.recovery_policy.provider_recovery_attempts),
                    output_recovery_attempts: patch
                        .policy
                        .output_recovery_attempts
                        .unwrap_or(current.recovery_policy.output_recovery_attempts),
                    tail_bounds: current.recovery_policy.tail_bounds,
                };
                validate_recovery(&next.recovery_policy)?;
            }
        }
        Ok(next)
    }
}

impl TightenResourceQuota {
    /// Fold this patch onto the current quota, refusing any axis that would widen it.
    ///
    /// "Uncapped ⇒ capped" is a tightening and is allowed; "capped ⇒ uncapped" is not expressible
    /// (an absent axis means *unchanged* here, not *cleared*), and a larger cap is refused
    /// outright rather than clamped — a silently clamped budget is a budget the host thinks it
    /// raised.
    pub fn tighten(
        &self,
        current: &super::config::ResourceQuota,
    ) -> Result<super::config::ResourceQuota, super::envelope::WireRejection> {
        use super::envelope::{WireRejection, WireRejectionKind};

        fn narrow(
            label: &str,
            requested: Option<u32>,
            current: Option<u32>,
        ) -> Result<Option<u32>, WireRejection> {
            match (requested, current) {
                (None, current) => Ok(current),
                (Some(requested), Some(current)) if requested > current => Err(WireRejection::new(
                    WireRejectionKind::PolicyViolation,
                    format!(
                        "policy patch raises {label} from {current} to {requested}; \
                             a live quota change may only tighten — growing one is a reservation \
                             fact and needs its own contract"
                    ),
                )),
                (Some(requested), _) => Ok(Some(requested)),
            }
        }

        Ok(super::config::ResourceQuota {
            max_concurrent_subagents: narrow(
                "max_concurrent_subagents",
                self.max_concurrent_subagents,
                current.max_concurrent_subagents,
            )?,
            max_total_subagents: narrow(
                "max_total_subagents",
                self.max_total_subagents,
                current.max_total_subagents,
            )?,
            max_spawn_depth: narrow(
                "max_spawn_depth",
                self.max_spawn_depth,
                current.max_spawn_depth,
            )?,
            max_workflow_nodes: narrow(
                "max_workflow_nodes",
                self.max_workflow_nodes,
                current.max_workflow_nodes,
            )?,
            // not a live axis: a rate window is part of the governance posture, and the live form
            // of that is `ReplaceGovernancePolicy`
            memory_writes_per_window: current.memory_writes_per_window.clone(),
        })
    }
}

// ---------------------------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::kernel::wire::config::{
        ConfigDefaults, HostEffectSupport, OperationConfig, ResolvedOperationConfig, ResourceQuota,
    };
    use crate::runtime::kernel::wire::effect::EffectKindTag;
    use crate::runtime::kernel::wire::envelope::WireRejectionKind;
    use serde_json::json;

    fn resolved(quota: Option<ResourceQuota>) -> ResolvedOperationConfig {
        OperationConfig {
            resource_quota: quota,
            // a quota that declares spawn capacity obliges the host to declare it can launch and
            // stop child tasks (DEC-8 config-time cross-validation)
            host_effect_support: HostEffectSupport::new([
                EffectKindTag::CallProvider,
                EffectKindTag::SpawnTasks,
                EffectKindTag::PreemptTasks,
            ]),
            ..OperationConfig::default()
        }
        .resolve(&ConfigDefaults::default())
        .expect("baseline config resolves")
    }

    fn quota() -> ResourceQuota {
        ResourceQuota {
            max_concurrent_subagents: Some(4),
            max_total_subagents: Some(16),
            max_spawn_depth: Some(3),
            max_workflow_nodes: Some(64),
            memory_writes_per_window: None,
        }
    }

    fn patch(expected_revision: u64, patch: LivePolicyPatch) -> ApplyPolicyPatchCommand {
        ApplyPolicyPatchCommand {
            expected_revision: WireU64::new(expected_revision),
            patch,
        }
    }

    fn tighten(quota: TightenResourceQuota) -> LivePolicyPatch {
        LivePolicyPatch::TightenResourceQuota(quota)
    }

    // -----------------------------------------------------------------------------------------
    // DEC-6 · optimistic concurrency lives on the patch, not on a policy
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_patch_at_the_current_revision_applies_and_advances_it() {
        let mut state = LivePolicyState::new(resolved(Some(quota())));
        assert_eq!(state.revision(), WireU64::ZERO);

        let next = state
            .apply(&patch(
                0,
                tighten(TightenResourceQuota {
                    max_concurrent_subagents: Some(2),
                    ..TightenResourceQuota::default()
                }),
            ))
            .expect("a patch at the current revision applies");
        assert_eq!(next, WireU64::new(1));
        assert_eq!(state.revision(), WireU64::new(1));
        assert_eq!(
            state.config().resource_quota.max_concurrent_subagents,
            Some(2)
        );
        // untouched axes survive a partial patch
        assert_eq!(state.config().resource_quota.max_total_subagents, Some(16));
    }

    #[test]
    fn a_stale_revision_is_refused_and_changes_nothing() {
        let mut state = LivePolicyState::new(resolved(Some(quota())));
        state
            .apply(&patch(
                0,
                tighten(TightenResourceQuota {
                    max_spawn_depth: Some(2),
                    ..TightenResourceQuota::default()
                }),
            ))
            .unwrap();

        let before = state.clone();
        // the second writer read revision 0 before the first writer committed
        let rejection = state
            .apply(&patch(
                0,
                tighten(TightenResourceQuota {
                    max_spawn_depth: Some(1),
                    ..TightenResourceQuota::default()
                }),
            ))
            .expect_err("a stale patch must not silently overwrite");
        assert_eq!(rejection.kind, WireRejectionKind::PolicyViolation);
        assert!(rejection.message.contains("revision mismatch"));
        assert_eq!(state, before, "a refused patch left state behind");

        // rebasing onto the new revision is the whole point of refusing
        assert!(
            state
                .apply(&patch(
                    1,
                    tighten(TightenResourceQuota {
                        max_spawn_depth: Some(1),
                        ..TightenResourceQuota::default()
                    })
                ))
                .is_ok()
        );
    }

    #[test]
    fn a_future_revision_is_refused_too() {
        let mut state = LivePolicyState::new(resolved(Some(quota())));
        let rejection = state
            .apply(&patch(9, tighten(TightenResourceQuota::default())))
            .expect_err("a patch from the future is not a valid rebase either");
        assert!(rejection.message.contains("revision mismatch"));
        assert_eq!(state.revision(), WireU64::ZERO);
    }

    #[test]
    fn the_revision_is_mandatory_on_the_wire() {
        assert!(
            serde_json::from_value::<ApplyPolicyPatchCommand>(json!({
                "patch": { "kind": "replace_signal_policy", "policy": { "queue_max": 8 } },
            }))
            .is_err(),
            "a patch without a revision silently overwrites another writer"
        );
    }

    #[test]
    fn no_policy_carries_a_version_of_its_own() {
        // DEC-6 / §16.1: `SIGNAL_POLICY_VERSION` and `SignalPolicyConfig.version` are gone; the
        // revision is `ApplyPolicyPatchCommand::expected_revision` and nothing else.
        assert!(
            serde_json::from_value::<SignalPolicy>(json!({ "queue_max": 8, "version": 1 }))
                .is_err()
        );
        assert!(serde_json::from_value::<GovernancePolicy>(json!({ "version": 1 })).is_err());
        assert!(serde_json::from_value::<RecoveryPolicy>(json!({ "version": 1 })).is_err());
    }

    // -----------------------------------------------------------------------------------------
    // live quota changes only ever tighten
    // -----------------------------------------------------------------------------------------

    #[test]
    fn a_live_quota_change_may_only_tighten() {
        let mut state = LivePolicyState::new(resolved(Some(quota())));
        for (label, widening) in [
            (
                "max_concurrent_subagents",
                TightenResourceQuota {
                    max_concurrent_subagents: Some(99),
                    ..TightenResourceQuota::default()
                },
            ),
            (
                "max_total_subagents",
                TightenResourceQuota {
                    max_total_subagents: Some(99),
                    ..TightenResourceQuota::default()
                },
            ),
            (
                "max_spawn_depth",
                TightenResourceQuota {
                    max_spawn_depth: Some(9),
                    ..TightenResourceQuota::default()
                },
            ),
            (
                "max_workflow_nodes",
                TightenResourceQuota {
                    max_workflow_nodes: Some(999),
                    ..TightenResourceQuota::default()
                },
            ),
        ] {
            let before = state.clone();
            let rejection = state
                .apply(&patch(0, tighten(widening)))
                .expect_err("widening must be refused, not clamped");
            assert!(
                rejection.message.contains("may only tighten"),
                "{label}: unexpected message {}",
                rejection.message
            );
            assert!(rejection.message.contains(label));
            assert_eq!(state, before, "{label}: a refused patch left state behind");
        }
    }

    #[test]
    fn capping_a_previously_uncapped_axis_is_a_tightening() {
        let mut state = LivePolicyState::new(resolved(None));
        assert_eq!(state.config().resource_quota.max_spawn_depth, None);
        state
            .apply(&patch(
                0,
                tighten(TightenResourceQuota {
                    max_spawn_depth: Some(2),
                    ..TightenResourceQuota::default()
                }),
            ))
            .expect("uncapped ⇒ capped narrows the surface");
        assert_eq!(state.config().resource_quota.max_spawn_depth, Some(2));
    }

    #[test]
    fn an_absent_axis_means_unchanged_not_cleared() {
        let mut state = LivePolicyState::new(resolved(Some(quota())));
        state
            .apply(&patch(0, tighten(TightenResourceQuota::default())))
            .unwrap();
        assert_eq!(state.config().resource_quota, quota());
    }

    #[test]
    fn a_patch_that_fails_validation_leaves_the_revision_alone() {
        let mut state = LivePolicyState::new(resolved(Some(quota())));
        let before = state.clone();
        let rejection = state
            .apply(&patch(
                0,
                LivePolicyPatch::ReplaceSignalPolicy(ReplaceSignalPolicy {
                    policy: SignalPolicy {
                        queue_max: 0,
                        ttl_ms: None,
                        deadline_escalation: None,
                    },
                }),
            ))
            .expect_err("a zero-length signal queue drops every signal");
        assert!(rejection.message.contains("queue_max"));
        assert_eq!(state, before);
    }

    #[test]
    fn replacing_the_governance_policy_reuses_the_boot_validator() {
        let mut state = LivePolicyState::new(resolved(None));
        let rejection = state
            .apply(&patch(
                0,
                LivePolicyPatch::ReplaceGovernancePolicy(ReplaceGovernancePolicy {
                    policy: GovernancePolicy {
                        constraints: vec![ParamConstraint::Enum(EnumParam {
                            tool: "write".to_string(),
                            param_path: "mode".to_string(),
                            values: Vec::new(),
                        })],
                        ..GovernancePolicy::default()
                    },
                }),
            ))
            .expect_err("an enum constraint with no permitted value denies every call");
        assert!(rejection.message.contains("no permitted value"));

        state
            .apply(&patch(
                0,
                LivePolicyPatch::ReplaceGovernancePolicy(ReplaceGovernancePolicy {
                    policy: GovernancePolicy {
                        default_action: Some(PolicyAction::Deny),
                        rules: vec![PolicyRule {
                            tool_pattern: "read.*".to_string(),
                            action: PolicyAction::Allow,
                        }],
                        ..GovernancePolicy::default()
                    },
                }),
            ))
            .expect("a well-formed governance replacement applies");
        assert_eq!(
            state.config().governance_policy.default_action,
            PolicyAction::Deny
        );
    }

    #[test]
    fn recovery_ladders_stay_inside_their_ceiling_live_too() {
        let mut state = LivePolicyState::new(resolved(None));
        let rejection = state
            .apply(&patch(
                0,
                LivePolicyPatch::ReplaceRecoveryPolicy(ReplaceRecoveryPolicy {
                    policy: RecoveryPolicy {
                        provider_recovery_attempts: Some(64),
                        output_recovery_attempts: None,
                        tail_bounds: None,
                    },
                }),
            ))
            .expect_err("an unbounded recovery ladder is a livelock");
        assert!(rejection.message.contains("provider_recovery_attempts"));
    }

    /// §12.3 / §5e-5 · the tail bound is frozen by the genesis record. A live patch that tried to
    /// widen it would let a run escape the very limit a `CheckpointRequired` just refused it
    /// against — and one that tried to narrow it would refuse inputs the journal already accepted.
    #[test]
    fn the_tail_bound_is_boot_only_even_though_its_policy_is_live() {
        let mut state = LivePolicyState::new(resolved(None));
        let frozen = state.config().recovery_policy.tail_bounds;

        let rejection = state
            .apply(&patch(
                0,
                LivePolicyPatch::ReplaceRecoveryPolicy(ReplaceRecoveryPolicy {
                    policy: RecoveryPolicy {
                        provider_recovery_attempts: None,
                        output_recovery_attempts: None,
                        tail_bounds: Some(TailBoundsPolicy {
                            hard_records: Some(WireU64::new(1_000_000)),
                            ..TailBoundsPolicy::default()
                        }),
                    },
                }),
            ))
            .expect_err("the tail bound is not live-mutable");
        assert!(rejection.message.contains("boot-only"), "{rejection}");
        assert_eq!(
            state.config().recovery_policy.tail_bounds,
            frozen,
            "a refused patch changes nothing"
        );

        // the rest of the recovery policy is still live, and the bound rides through untouched
        state
            .apply(&patch(
                0,
                LivePolicyPatch::ReplaceRecoveryPolicy(ReplaceRecoveryPolicy {
                    policy: RecoveryPolicy {
                        provider_recovery_attempts: Some(3),
                        output_recovery_attempts: None,
                        tail_bounds: None,
                    },
                }),
            ))
            .expect("the semantic ladders are live-mutable");
        assert_eq!(state.config().recovery_policy.provider_recovery_attempts, 3);
        assert_eq!(state.config().recovery_policy.tail_bounds, frozen);
    }

    // -----------------------------------------------------------------------------------------
    // §13.2 · the live union is closed
    // -----------------------------------------------------------------------------------------

    #[test]
    fn the_live_policy_union_is_closed_to_boot_only_policies() {
        for unlisted in [
            "replace_context_policy",
            "replace_execution_policy",
            "replace_scheduler_policy",
            "replace_payload_policy",
            "replace_feature_policy",
            "set_tools",
            "set_knowledge_budget",
            "set_scheduler_budget",
            "set_memory_policy",
            "set_tokenizer",
        ] {
            let error = serde_json::from_value::<LivePolicyPatch>(json!({ "kind": unlisted }))
                .expect_err("only the four §13.2 patches exist");
            assert!(
                error.to_string().contains("unknown variant"),
                "{unlisted}: {error}"
            );
        }
    }

    #[test]
    fn every_live_command_is_a_13_2_capability() {
        // §13.2 enumerates the live surface. Anything outside it is boot-only by construction.
        let commands = [
            json!({ "kind": "cancel", "reason": "user" }),
            json!({ "kind": "update_deadline", "deadline_ms": "1700000000000" }),
            json!({ "kind": "update_task", "update": { "progress": "halfway" } }),
            json!({ "kind": "apply_capability_patch", "patch": {} }),
            json!({ "kind": "apply_knowledge_mutation", "mutation": {} }),
            json!({ "kind": "force_compact" }),
            json!({ "kind": "apply_skill_activation", "activate": [{ "name": "research" }] }),
            json!({
                "kind": "apply_policy_patch",
                "expected_revision": "0",
                "patch": { "kind": "replace_signal_policy", "policy": { "queue_max": 8 } },
            }),
            json!({ "kind": "seed_knowledge", "entries": [] }),
        ];
        for command in commands {
            serde_json::from_value::<HostCommand>(command.clone())
                .unwrap_or_else(|e| panic!("{command}: {e}"));
        }

        for boot_only in [
            json!({ "kind": "configure_run", "config": {} }),
            json!({ "kind": "set_tools", "tools": [] }),
            json!({ "kind": "load_governance_policy" }),
            json!({ "kind": "set_resource_quota", "quota": {} }),
            json!({ "kind": "set_scheduler_budget", "max_wall_ms": "1" }),
            json!({ "kind": "set_memory_policy", "memory_path": "/tmp" }),
            json!({ "kind": "resume" }),
            json!({ "kind": "spawn_sub_agent" }),
            json!({ "kind": "page_in", "entries": [] }),
        ] {
            assert!(
                serde_json::from_value::<HostCommand>(boot_only.clone()).is_err(),
                "{boot_only} must not decode as a live command"
            );
        }
    }

    #[test]
    fn host_and_model_task_updates_do_not_share_a_wire_variant() {
        use crate::runtime::kernel::wire::syscall::SyscallRequest;

        let update = json!({ "plan": ["a", "b"], "current_step": 1 });
        let host: HostCommand =
            serde_json::from_value(json!({ "kind": "update_task", "update": update })).unwrap();
        let model: SyscallRequest =
            serde_json::from_value(json!({ "kind": "update_task", "update": update })).unwrap();

        // Same mutation payload, two different authority paths — the distinction the retired
        // shared task-update input could not express.
        match (host, model) {
            (HostCommand::UpdateTask(host), SyscallRequest::UpdateTask(model)) => {
                assert_eq!(host.update, model.update);
            }
            other => panic!("unexpected decode: {other:?}"),
        }
    }

    // -----------------------------------------------------------------------------------------
    // governance constraint scalars
    // -----------------------------------------------------------------------------------------

    #[test]
    fn range_constraints_are_fixed_point_not_floats() {
        assert!(
            serde_json::from_value::<ParamConstraint>(json!({
                "kind": "range", "tool": "sample", "param_path": "temperature", "min": 0.0, "max": 1.0,
            }))
            .is_err(),
            "an authoritative range bound must not be a language-default float"
        );
        let parsed: ParamConstraint = serde_json::from_value(json!({
            "kind": "range", "tool": "sample", "param_path": "temperature",
            "min_micros": 0, "max_micros": 1_000_000,
        }))
        .unwrap();
        assert!(parsed.validate().is_ok());
    }

    #[test]
    fn a_range_constraint_that_can_never_hold_is_rejected() {
        for constraint in [
            ParamConstraint::Range(RangeParam {
                tool: "sample".to_string(),
                param_path: "t".to_string(),
                min_micros: None,
                max_micros: None,
            }),
            ParamConstraint::Range(RangeParam {
                tool: "sample".to_string(),
                param_path: "t".to_string(),
                min_micros: Some(2_000_000),
                max_micros: Some(1_000_000),
            }),
            ParamConstraint::Required(RequiredParam {
                tool: String::new(),
                param_path: "t".to_string(),
            }),
        ] {
            assert!(constraint.validate().is_err());
        }
    }

    // -----------------------------------------------------------------------------------------
    // cancel carries no duplicated identity
    // -----------------------------------------------------------------------------------------

    #[test]
    fn cancel_does_not_repeat_the_operation_id() {
        assert!(
            serde_json::from_value::<CancelCommand>(json!({
                "reason": "user", "operation_id": "op-1",
            }))
            .is_err(),
            "the envelope owns the operation id; repeating it forced three SDKs to special-case cancel"
        );
    }
}
