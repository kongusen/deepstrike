pub use deepstrike_core::runtime::kernel::wire::{
    ParamConstraint as ConstraintSpec, PolicyAction, PolicyRule, RateLimitSpec,
};
use deepstrike_core::runtime::kernel::wire::{SignalPolicy as WireSignalPolicy, WireU64};
pub use deepstrike_core::scheduler::policy::SchedulerPolicyConfig;

use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalPolicy {
    pub queue_max: u32,
    pub ttl_ms: Option<u64>,
    pub deadline_escalation: Option<bool>,
}

impl SignalPolicy {
    pub(crate) fn into_kernel(self) -> WireSignalPolicy {
        WireSignalPolicy {
            queue_max: self.queue_max,
            ttl_ms: self.ttl_ms.map(WireU64::new),
            deadline_escalation: self.deadline_escalation,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryWriteRateLimit {
    pub max_writes: u32,
    pub window_ms: u64,
}

impl From<MemoryWriteRateLimit> for (u32, u64) {
    fn from(limit: MemoryWriteRateLimit) -> Self {
        (limit.max_writes, limit.window_ms)
    }
}

#[derive(Debug, Clone)]
pub struct GovernancePolicy {
    pub default_action: Option<PolicyAction>,
    pub rules: Vec<PolicyRule>,
    pub vetoed_tools: Vec<String>,
    pub rate_limits: Vec<RateLimitSpec>,
    pub constraints: Vec<ConstraintSpec>,
    /// I5: when true (default), the kernel withholds statically denied tools (vetoes and `deny`
    /// rules, evaluated exactly as the call gate evaluates them) from the provider surface and
    /// names them once in the knowledge slot. Mirrors Node.
    pub surface_denied_in_system: bool,
}

impl GovernancePolicy {
    pub fn allow_all() -> Self {
        Self {
            default_action: None,
            rules: vec![PolicyRule {
                tool_pattern: "*".to_string(),
                action: PolicyAction::Allow,
            }],
            vetoed_tools: vec![],
            rate_limits: vec![],
            constraints: vec![],
            surface_denied_in_system: true,
        }
    }

    pub(crate) fn into_host_fact(self) -> serde_json::Value {
        serde_json::json!({
            "kind": "load_governance_policy",
            "default_action": self.default_action,
            "rules": self.rules,
            "vetoed_tools": self.vetoed_tools,
            "rate_limits": self.rate_limits,
            "constraints": self.constraints,
            "hide_denied_tools": self.surface_denied_in_system,
        })
    }

    /// A §13.2 live-policy patch replacing the governance posture, for
    /// [`crate::RuntimeRunner::apply_policy_patch`].
    pub fn into_policy_patch(self) -> serde_json::Value {
        let mut policy = self.into_host_fact();
        let object = policy.as_object_mut().expect("the host fact is an object");
        object.remove("kind");
        if object
            .get("default_action")
            .is_some_and(serde_json::Value::is_null)
        {
            object.remove("default_action");
        }
        serde_json::json!({ "kind": "replace_governance_policy", "policy": policy })
    }
}

#[derive(Debug, Clone)]
pub struct NativeOsProfile {
    pub id: &'static str,
    pub signal_policy: SignalPolicy,
    pub governance_policy: GovernancePolicy,
}

#[derive(Debug, Clone)]
pub enum OsProfile {
    Native,
    Concrete(NativeOsProfile),
}

pub const DEFAULT_NATIVE_SIGNAL_POLICY: SignalPolicy = SignalPolicy {
    queue_max: 64,
    ttl_ms: None,
    deadline_escalation: None,
};

pub fn default_native_governance_policy() -> GovernancePolicy {
    GovernancePolicy::allow_all()
}

pub fn os_profile(profile: Option<OsProfile>) -> NativeOsProfile {
    match profile.unwrap_or(OsProfile::Native) {
        OsProfile::Native => NativeOsProfile {
            id: "native",
            signal_policy: DEFAULT_NATIVE_SIGNAL_POLICY,
            governance_policy: default_native_governance_policy(),
        },
        OsProfile::Concrete(profile) => profile,
    }
}

pub fn assert_native_profile(profile: Option<OsProfile>) -> Result<NativeOsProfile> {
    let resolved = os_profile(profile);
    if resolved.id != "native" {
        return Err(Error::Other(format!(
            "Unsupported OS profile: {}",
            resolved.id
        )));
    }
    if resolved.signal_policy.queue_max == 0 {
        return Err(Error::Other(
            "Invalid native OS profile: SignalPolicy queue_max must be positive".to_string(),
        ));
    }
    if matches!(resolved.signal_policy.ttl_ms, Some(0)) {
        return Err(Error::Other(
            "Invalid native OS profile: SignalPolicy ttl_ms must be positive when present"
                .to_string(),
        ));
    }
    Ok(resolved)
}
