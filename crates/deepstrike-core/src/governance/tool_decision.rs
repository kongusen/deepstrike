use crate::types::message::ToolCall;
use crate::types::policy::{CallerContext, GovernanceVerdict};

/// Explicit stage in the tool-decision path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolDecisionStage {
    Classifier,
    Hook,
    Permission,
    Veto,
    RateLimit,
    Constraint,
    Audit,
}

impl ToolDecisionStage {
    pub fn as_str(self) -> &'static str {
        match self {
            ToolDecisionStage::Classifier => "classifier",
            ToolDecisionStage::Hook => "hook",
            ToolDecisionStage::Permission => "permission",
            ToolDecisionStage::Veto => "veto",
            ToolDecisionStage::RateLimit => "rate_limit",
            ToolDecisionStage::Constraint => "constraint",
            ToolDecisionStage::Audit => "audit",
        }
    }
}

/// One stage output with provenance.
#[derive(Debug, Clone)]
pub struct ToolDecision {
    pub stage: ToolDecisionStage,
    pub verdict: GovernanceVerdict,
}

impl ToolDecision {
    pub fn allow(stage: ToolDecisionStage) -> Self {
        Self {
            stage,
            verdict: GovernanceVerdict::Allow,
        }
    }

    pub fn deny(stage: ToolDecisionStage, reason: impl Into<String>) -> Self {
        Self {
            stage,
            verdict: GovernanceVerdict::Deny {
                stage: stage.as_str(),
                reason: reason.into(),
            },
        }
    }

    pub fn ask_user(stage: ToolDecisionStage, reason: impl Into<String>) -> Self {
        Self {
            stage,
            verdict: GovernanceVerdict::AskUser {
                reason: reason.into(),
            },
        }
    }
}

/// Context passed to tool-decision stages.
pub struct ToolDecisionContext<'a> {
    pub call: &'a ToolCall,
    pub caller: &'a CallerContext,
}

/// Stateless reducer for tool-decision stages.
///
/// Important invariant: deny is monotonic. A later allow cannot override an
/// earlier deny, so hooks can enrich or restrict behavior without bypassing
/// configured safety rules.
pub struct ToolDecisionPipeline;

impl ToolDecisionPipeline {
    pub fn reduce(decisions: &[ToolDecision]) -> GovernanceVerdict {
        for decision in decisions {
            if let GovernanceVerdict::Deny { .. } = &decision.verdict {
                return decision.verdict.clone();
            }
        }

        for decision in decisions {
            if let GovernanceVerdict::AskUser { .. } = &decision.verdict {
                return decision.verdict.clone();
            }
        }

        for decision in decisions {
            if let GovernanceVerdict::RateLimited { .. } = &decision.verdict {
                return decision.verdict.clone();
            }
        }

        GovernanceVerdict::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deny_is_monotonic_over_later_allow() {
        let verdict = ToolDecisionPipeline::reduce(&[
            ToolDecision::deny(ToolDecisionStage::Permission, "blocked by settings"),
            ToolDecision::allow(ToolDecisionStage::Hook),
        ]);

        assert!(matches!(
            verdict,
            GovernanceVerdict::Deny {
                stage: "permission",
                ..
            }
        ));
    }

    #[test]
    fn ask_user_survives_when_no_deny_exists() {
        let verdict = ToolDecisionPipeline::reduce(&[
            ToolDecision::allow(ToolDecisionStage::Classifier),
            ToolDecision::ask_user(ToolDecisionStage::Permission, "needs approval"),
            ToolDecision::allow(ToolDecisionStage::Hook),
        ]);

        assert!(matches!(verdict, GovernanceVerdict::AskUser { .. }));
    }

    #[test]
    fn all_allow_reduces_to_allow() {
        let verdict = ToolDecisionPipeline::reduce(&[
            ToolDecision::allow(ToolDecisionStage::Classifier),
            ToolDecision::allow(ToolDecisionStage::Permission),
        ]);

        assert!(matches!(verdict, GovernanceVerdict::Allow));
    }
}
