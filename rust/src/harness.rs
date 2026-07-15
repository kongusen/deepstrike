//! Shared attempt-evaluation value types.

pub use deepstrike_core::harness::eval::{Criterion, CriterionResult};

/// Quality judgment is independent of the attempt's runtime termination status.
#[derive(Debug, Clone)]
pub struct Verdict {
    pub passed: bool,
    pub overall_score: f32,
    pub feedback: String,
    pub details: Vec<CriterionResult>,
}
