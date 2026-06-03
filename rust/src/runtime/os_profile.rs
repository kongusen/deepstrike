use deepstrike_core::runtime::kernel::{
    ConstraintSpec, KernelInputEvent, PolicyAction, PolicyRule, RateLimitSpec,
};

use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttentionPolicy {
    pub max_queue_size: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchedulerBudget {
    pub max_wall_ms: Option<u64>,
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
        }
    }

    pub fn into_kernel_event(self) -> KernelInputEvent {
        KernelInputEvent::LoadGovernancePolicy {
            default_action: self.default_action,
            rules: self.rules,
            vetoed_tools: self.vetoed_tools,
            rate_limits: self.rate_limits,
            constraints: self.constraints,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NativeOsProfile {
    pub id: &'static str,
    pub attention_policy: AttentionPolicy,
    pub governance_policy: GovernancePolicy,
}

#[derive(Debug, Clone)]
pub enum OsProfile {
    Native,
    Concrete(NativeOsProfile),
}

pub const DEFAULT_NATIVE_ATTENTION_POLICY: AttentionPolicy = AttentionPolicy {
    max_queue_size: Some(64),
};

pub fn default_native_governance_policy() -> GovernancePolicy {
    GovernancePolicy::allow_all()
}

pub fn os_profile(profile: Option<OsProfile>) -> NativeOsProfile {
    match profile.unwrap_or(OsProfile::Native) {
        OsProfile::Native => NativeOsProfile {
            id: "native",
            attention_policy: DEFAULT_NATIVE_ATTENTION_POLICY,
            governance_policy: default_native_governance_policy(),
        },
        OsProfile::Concrete(profile) => profile,
    }
}

pub fn assert_native_profile(profile: Option<OsProfile>) -> Result<NativeOsProfile> {
    let resolved = os_profile(profile);
    if resolved.id != "native" {
        return Err(Error::Other(format!("Unsupported OS profile: {}", resolved.id)));
    }
    if let Some(max_queue_size) = resolved.attention_policy.max_queue_size {
        if max_queue_size == 0 {
            return Err(Error::Other(
                "Invalid native OS profile: AttentionPolicy max_queue_size must be positive"
                    .to_string(),
            ));
        }
    }
    Ok(resolved)
}
