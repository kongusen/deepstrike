use deepstrike_core::runtime::kernel::{
    ConstraintSpec, KernelInputEvent, PolicyAction, PolicyRule, RateLimitSpec, SignalPolicyConfig,
    SIGNAL_POLICY_VERSION,
};
pub use deepstrike_core::scheduler::policy::SchedulerPolicyConfig;

use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalPolicy {
    pub queue_max: u32,
    pub ttl_ms: Option<u64>,
    pub deadline_escalation: Option<bool>,
}

impl SignalPolicy {
    pub(crate) fn into_kernel(self) -> SignalPolicyConfig {
        SignalPolicyConfig {
            version: SIGNAL_POLICY_VERSION,
            queue_max: self.queue_max,
            ttl_ms: self.ttl_ms,
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
    /// I5: when true (default), the runner pre-filters denied tools from the schema. Mirrors Node.
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

    pub fn into_kernel_event(self) -> KernelInputEvent {
        KernelInputEvent::LoadGovernancePolicy {
            default_action: self.default_action,
            rules: self.rules,
            vetoed_tools: self.vetoed_tools,
            rate_limits: self.rate_limits,
            constraints: self.constraints,
            // Rust SDK does not surface the deny-mode experiment yet; kernel default (rollback).
            deny_mode: None,
        }
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

/// I5: bucket tool schemas into allowed/denied per policy. Pure. Mirrors Node `governanceFilterSchema`.
pub fn governance_filter_schema(
    tools: &[deepstrike_core::types::message::ToolSchema],
    policy: &GovernancePolicy,
) -> (
    Vec<deepstrike_core::types::message::ToolSchema>,
    Vec<String>,
) {
    let mut allowed = Vec::with_capacity(tools.len());
    let mut denied = Vec::new();
    let matches = |pat: &str, name: &str| -> bool {
        if pat == name {
            return true;
        }
        if let Some(prefix) = pat.strip_suffix('*') {
            return name.starts_with(prefix);
        }
        false
    };
    for tool in tools {
        let name = tool.name.as_str();
        if policy.vetoed_tools.iter().any(|v| v == name) {
            denied.push(name.to_string());
            continue;
        }
        let mut action = policy.default_action.clone().unwrap_or(PolicyAction::Allow);
        for r in &policy.rules {
            if matches(&r.tool_pattern, name) {
                action = r.action.clone();
            }
        }
        if matches!(action, PolicyAction::Deny) {
            denied.push(name.to_string());
        } else {
            allowed.push(tool.clone());
        }
    }
    (allowed, denied)
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
