use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    Default,
    Plan,
    Auto,
}

#[derive(Debug, Clone)]
pub struct Permission {
    pub tool: String,
    pub action: String,
    pub allowed: bool,
    pub requires_approval: bool,
    pub note: String,
}

#[derive(Debug, Clone)]
pub struct PermissionDecision {
    pub allowed: bool,
    pub reason: String,
    pub requires_approval: bool,
    pub matched_permission: Option<Permission>,
}

pub struct PermissionManager {
    mode: PermissionMode,
    permissions: HashMap<String, Permission>,
}

impl PermissionManager {
    pub fn new(mode: PermissionMode) -> Self {
        Self {
            mode,
            permissions: Default::default(),
        }
    }

    pub fn grant(&mut self, resource: impl Into<String>, action: impl Into<String>) {
        let tool = resource.into();
        let action = action.into();
        let key = format!("{tool}:{action}");
        self.permissions.insert(
            key,
            Permission {
                tool,
                action,
                allowed: true,
                requires_approval: false,
                note: String::new(),
            },
        );
    }

    pub fn grant_with_approval(
        &mut self,
        resource: impl Into<String>,
        action: impl Into<String>,
        note: impl Into<String>,
    ) {
        let tool = resource.into();
        let action = action.into();
        let key = format!("{tool}:{action}");
        self.permissions.insert(
            key,
            Permission {
                tool,
                action,
                allowed: true,
                requires_approval: true,
                note: note.into(),
            },
        );
    }

    pub fn revoke(&mut self, resource: &str, action: &str) {
        let key = format!("{resource}:{action}");
        self.permissions.insert(
            key,
            Permission {
                tool: resource.to_string(),
                action: action.to_string(),
                allowed: false,
                requires_approval: false,
                note: String::new(),
            },
        );
    }

    fn match_permission(&self, tool: &str, action: &str) -> Option<Permission> {
        for key in [
            format!("{tool}:{action}"),
            format!("{tool}:*"),
            format!("*:{action}"),
            format!("*:*"),
        ] {
            if let Some(p) = self.permissions.get(&key) {
                return Some(p.clone());
            }
        }
        None
    }

    pub fn evaluate(&self, resource: &str, action: &str) -> PermissionDecision {
        match self.mode {
            PermissionMode::Auto => PermissionDecision {
                allowed: true,
                reason: "AUTO mode".into(),
                requires_approval: false,
                matched_permission: None,
            },
            PermissionMode::Plan => PermissionDecision {
                allowed: false,
                reason: "PLAN mode blocks all".into(),
                requires_approval: false,
                matched_permission: None,
            },
            PermissionMode::Default => match self.match_permission(resource, action) {
                None => PermissionDecision {
                    allowed: false,
                    reason: "not granted".into(),
                    requires_approval: false,
                    matched_permission: None,
                },
                Some(p) if !p.allowed => PermissionDecision {
                    allowed: false,
                    reason: if p.note.is_empty() {
                        "permission denied".into()
                    } else {
                        p.note.clone()
                    },
                    requires_approval: false,
                    matched_permission: Some(p),
                },
                Some(p) if p.requires_approval => PermissionDecision {
                    allowed: false,
                    reason: if p.note.is_empty() {
                        "requires approval".into()
                    } else {
                        p.note.clone()
                    },
                    requires_approval: true,
                    matched_permission: Some(p),
                },
                Some(p) => PermissionDecision {
                    allowed: true,
                    reason: "granted".into(),
                    requires_approval: false,
                    matched_permission: Some(p),
                },
            },
        }
    }
}
