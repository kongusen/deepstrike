use serde::{Deserialize, Serialize};

/// A single verifiable acceptance criterion within a contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptanceCriterion {
    /// Stable identifier used in ContractCheckResult.
    pub id: String,
    /// Human-readable statement of what correct looks like.
    pub text: String,
    /// If true, failure here fails the whole contract regardless of score.
    pub required: bool,
    /// Contribution to the overall weighted score [0.0–1.0].
    pub weight: f32,
    /// If true the SDK can check this deterministically (e.g. word count, schema).
    pub machine_checkable: bool,
}

impl AcceptanceCriterion {
    pub fn new(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            text: text.into(),
            required: true,
            weight: 1.0,
            machine_checkable: false,
        }
    }

    pub fn optional(mut self) -> Self {
        self.required = false;
        self
    }

    pub fn with_weight(mut self, weight: f32) -> Self {
        self.weight = weight.clamp(0.0, 1.0);
        self
    }

    pub fn machine_checkable(mut self) -> Self {
        self.machine_checkable = true;
        self
    }
}

/// First-class contract type: defines what correct looks like before execution starts.
///
/// A `VerificationContract` is injected into the executor's `system` partition
/// (Priority::Critical) so it survives context renewal and compression.
/// The verifier receives the contract alongside the artifact and checks each
/// criterion independently, without access to the executor's implementation history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationContract {
    /// Stable identifier — also used as the skill name on successful extraction.
    pub id: String,
    /// The goal this contract governs. Copied into the executor's context.
    pub goal: String,
    /// Ordered list of acceptance criteria.
    pub acceptance: Vec<AcceptanceCriterion>,
    /// Patterns the executor must avoid. Checked by the verifier.
    pub anti_patterns: Vec<String>,
    /// Artifacts that must be present before the verifier runs (e.g. "report text").
    pub evidence_required: Vec<String>,
}

impl VerificationContract {
    pub fn new(id: impl Into<String>, goal: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            goal: goal.into(),
            acceptance: Vec::new(),
            anti_patterns: Vec::new(),
            evidence_required: Vec::new(),
        }
    }

    pub fn with_criterion(mut self, criterion: AcceptanceCriterion) -> Self {
        self.acceptance.push(criterion);
        self
    }

    pub fn with_anti_pattern(mut self, pattern: impl Into<String>) -> Self {
        self.anti_patterns.push(pattern.into());
        self
    }

    pub fn with_evidence(mut self, evidence: impl Into<String>) -> Self {
        self.evidence_required.push(evidence.into());
        self
    }

    /// Renders the contract as a markdown block suitable for injection into the system partition.
    pub fn format_for_system_prompt(&self) -> String {
        let mut out = format!(
            "## Verification Contract: {}\n\nGoal: {}\n\n",
            self.id, self.goal
        );

        out.push_str("### Acceptance Criteria\n");
        for (i, c) in self.acceptance.iter().enumerate() {
            let req = if c.required {
                "[REQUIRED]"
            } else {
                "[OPTIONAL]"
            };
            out.push_str(&format!(
                "{}. {} {} (id: `{}`, weight: {:.1})\n",
                i + 1,
                req,
                c.text,
                c.id,
                c.weight,
            ));
        }

        if !self.anti_patterns.is_empty() {
            out.push_str("\n### Anti-Patterns (must avoid)\n");
            for p in &self.anti_patterns {
                out.push_str(&format!("- {p}\n"));
            }
        }

        if !self.evidence_required.is_empty() {
            out.push_str("\n### Required Evidence\n");
            for e in &self.evidence_required {
                out.push_str(&format!("- {e}\n"));
            }
        }

        out
    }

    /// Derives a flat `Vec<String>` of criterion texts for use with the existing
    /// `EvalPipeline` / `HarnessLoop` criteria API.
    pub fn to_criteria_strings(&self) -> Vec<String> {
        self.acceptance.iter().map(|c| c.text.clone()).collect()
    }
}
