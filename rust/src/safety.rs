use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode { Default, Plan, Auto }

#[derive(Debug, Clone)]
pub struct PermissionDecision {
    pub allowed: bool,
    pub reason: &'static str,
}

pub struct PermissionManager {
    mode: PermissionMode,
    grants: std::collections::HashMap<String, HashSet<String>>,
}

impl PermissionManager {
    pub fn new(mode: PermissionMode) -> Self {
        Self { mode, grants: Default::default() }
    }

    pub fn grant(&mut self, resource: impl Into<String>, action: impl Into<String>) {
        self.grants.entry(resource.into()).or_default().insert(action.into());
    }

    pub fn revoke(&mut self, resource: &str, action: &str) {
        if let Some(set) = self.grants.get_mut(resource) { set.remove(action); }
    }

    pub fn evaluate(&self, resource: &str, action: &str) -> PermissionDecision {
        match self.mode {
            PermissionMode::Auto => PermissionDecision { allowed: true, reason: "AUTO mode" },
            PermissionMode::Plan => PermissionDecision { allowed: false, reason: "PLAN mode blocks all" },
            PermissionMode::Default => {
                let allowed = self.grants.get(resource)
                    .map(|s| s.contains(action) || s.contains("*"))
                    .unwrap_or(false);
                PermissionDecision { allowed, reason: if allowed { "granted" } else { "not granted" } }
            }
        }
    }
}
