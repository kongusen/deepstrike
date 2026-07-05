use crate::types::message::ToolCall;
use crate::types::policy::GovernanceVerdict;

/// A parameter constraint for tool arguments.
///
/// **Scope**: built-in rules cover the structural validation cases
/// (required / range / enum). For pattern matching or custom predicates,
/// do richer matching SDK-side — keeps the kernel free of regex deps
/// and lets the SDK use whatever pattern engine suits its host language.
#[derive(Debug, Clone)]
pub struct ParamConstraint {
    pub tool_name: String,
    pub param_path: String,
    pub rule: ConstraintRule,
}

#[derive(Debug, Clone)]
pub enum ConstraintRule {
    /// Numeric value in range
    Range { min: Option<f64>, max: Option<f64> },
    /// Value must be one of these
    Enum(Vec<String>),
    /// Value must not be empty
    Required,
}

/// Validates tool call arguments against registered constraints.
pub struct ConstraintValidator {
    constraints: Vec<ParamConstraint>,
}

impl ConstraintValidator {
    pub fn new() -> Self {
        Self {
            constraints: Vec::new(),
        }
    }

    pub fn add(&mut self, constraint: ParamConstraint) {
        self.constraints.push(constraint);
     }

    pub fn validate(&self, call: &ToolCall) -> Option<GovernanceVerdict> {
        for c in &self.constraints {
            if c.tool_name != call.name.as_str() {
                continue;
            }
            let value = call
                .arguments
                .pointer(&format!("/{}", c.param_path.replace('.', "/")));

            match &c.rule {
                ConstraintRule::Required => {
                    if value.is_none() || value == Some(&serde_json::Value::Null) {
                        return Some(GovernanceVerdict::Deny {
                            stage: "constraint",
                            reason: format!(
                                "parameter '{}' is required for '{}'",
                                c.param_path, c.tool_name
                            ),
                        });
                    }
                }
                ConstraintRule::Enum(allowed) => {
                    if let Some(val) = value.and_then(|v| v.as_str()) {
                        if !allowed.iter().any(|a| a == val) {
                            return Some(GovernanceVerdict::Deny {
                                stage: "constraint",
                                reason: format!(
                                    "parameter '{}' value '{}' not in allowed: {:?}",
                                    c.param_path, val, allowed
                                ),
                            });
                        }
                    }
                }
                ConstraintRule::Range { min, max } => {
                    if let Some(val) = value.and_then(|v| v.as_f64()) {
                        if let Some(lo) = min {
                            if val < *lo {
                                return Some(GovernanceVerdict::Deny {
                                    stage: "constraint",
                                    reason: format!(
                                        "parameter '{}' value {} below minimum {}",
                                        c.param_path, val, lo
                                    ),
                                });
                            }
                        }
                        if let Some(hi) = max {
                            if val > *hi {
                                return Some(GovernanceVerdict::Deny {
                                    stage: "constraint",
                                    reason: format!(
                                        "parameter '{}' value {} above maximum {}",
                                        c.param_path, val, hi
                                    ),
                                });
                            }
                        }
                    }
                }
            }
        }
        None
    }
}

impl Default for ConstraintValidator {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use compact_str::CompactString;

    fn call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: CompactString::new("c1"),
            name: CompactString::new(name),
            arguments: args,
        }
    }

    #[test]
    fn required_param_missing_denies() {
        let mut v = ConstraintValidator::new();
        v.add(ParamConstraint {
            tool_name: "writefile".into(),
            param_path: "path".into(),
            rule: ConstraintRule::Required,
        });
        let verdict = v.validate(&call("writefile", serde_json::json!({})));
        assert!(matches!(
            verdict,
            Some(GovernanceVerdict::Deny {
                stage: "constraint",
                ..
            })
        ));
    }

    #[test]
    fn enum_rule_rejects_unknown_value() {
        let mut v = ConstraintValidator::new();
        v.add(ParamConstraint {
            tool_name: "set_mode".into(),
            param_path: "mode".into(),
            rule: ConstraintRule::Enum(vec!["read".into(), "write".into()]),
        });
        let verdict = v.validate(&call("set_mode", serde_json::json!({"mode": "exec"})));
        assert!(matches!(verdict, Some(GovernanceVerdict::Deny { .. })));
    }

    #[test]
    fn range_rule_enforces_bounds() {
        let mut v = ConstraintValidator::new();
        v.add(ParamConstraint {
            tool_name: "sleep".into(),
            param_path: "seconds".into(),
            rule: ConstraintRule::Range {
                min: Some(0.0),
                max: Some(10.0),
            },
        });
        assert!(
            v.validate(&call("sleep", serde_json::json!({"seconds": 5})))
                .is_none()
        );
        assert!(
            v.validate(&call("sleep", serde_json::json!({"seconds": 100})))
                .is_some()
        );
    }
}
