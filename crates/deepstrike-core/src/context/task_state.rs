use serde::{Deserialize, Serialize};

/// One entry in the compression log — records what happened at each compression event.
/// All tiers write here; the log is append-only and never overwritten.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressionEntry {
    /// Compression tier label: snip_compact | micro_compact | context_collapse | auto_compact
    pub action: String,
    /// Human-readable summary (tool names, message counts, token counts).
    /// Empty for Snip/Micro which only record truncation stats.
    pub summary: String,
}

/// Persistent task state that lives in the working partition.
/// Survives compression, renewal, and wake/resume cycles because the working
/// partition is `compressible = false`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskState {
    /// Primary objective for this run. Set at `run_started`, immutable thereafter.
    pub goal: String,
    /// Acceptance criteria copied from `RunStarted`.
    pub criteria: Vec<String>,
    /// Ordered plan steps.
    pub plan: Vec<PlanStep>,
    /// Index of the step currently executing (0-based). None before planning.
    pub current_step: Option<usize>,
    /// Free-text progress note updated after each significant action.
    pub progress: String,
    /// Ephemeral scratch space for model use. Cleared on renewal. NOT used by the
    /// compression pipeline (use compression_log instead).
    pub scratchpad: String,
    /// Reasons the current step cannot proceed.
    pub blocked_on: Vec<String>,
    /// Call IDs or artifact hashes that must be preserved from compression.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preserved_refs: Vec<String>,
    /// Append-only log of all compression events. Never overwritten.
    /// Rendered into systemVolatile so the model always sees compression history.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compression_log: Vec<CompressionEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    pub label: String,
    pub done: bool,
}

impl PlanStep {
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            done: false,
        }
    }
}

/// Partial update applied by the SDK or via `update_plan` meta-tool.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TaskUpdate {
    pub plan: Option<Vec<String>>,
    pub current_step: Option<usize>,
    pub progress: Option<String>,
    pub scratchpad: Option<String>,
    pub blocked_on: Option<Vec<String>>,
    pub preserved_refs: Option<Vec<String>>,
}

impl TaskState {
    /// Compact text block for embedding in `system_text`.
    /// Returns an empty string when the task has not been initialised.
    pub fn format_compact(&self) -> String {
        if self.goal.is_empty() && self.plan.is_empty() && self.progress.is_empty() {
            return String::new();
        }

        let mut lines = Vec::new();
        lines.push(format!("[TASK STATE] goal: {}", self.goal));

        if !self.criteria.is_empty() {
            lines.push(format!("criteria: {}", self.criteria.join(" | ")));
        }

        if !self.plan.is_empty() {
            lines.push("plan:".to_string());
            for (i, step) in self.plan.iter().enumerate() {
                let marker = if step.done {
                    "done"
                } else if Some(i) == self.current_step {
                    "active"
                } else {
                    "todo"
                };
                lines.push(format!("  [{}] {}. {}", marker, i + 1, step.label));
            }
        }

        if !self.progress.is_empty() {
            lines.push(format!("progress: {}", self.progress));
        }

        if !self.blocked_on.is_empty() {
            lines.push(format!("blocked_on: {}", self.blocked_on.join(", ")));
        }

        if !self.scratchpad.is_empty() {
            lines.push(format!("scratchpad: {}", self.scratchpad));
        }

        // Render the most recent compression events (cap at 3 to limit token cost).
        if !self.compression_log.is_empty() {
            lines.push("compression_history:".to_string());
            let start = self.compression_log.len().saturating_sub(3);
            for entry in &self.compression_log[start..] {
                if entry.summary.is_empty() {
                    lines.push(format!("  [{}]", entry.action));
                } else {
                    lines.push(format!("  [{}] {}", entry.action, entry.summary));
                }
            }
        }

        lines.join("\n")
    }

    /// Append a compression event to the log. Never overwrites existing entries.
    pub fn log_compression(&mut self, action: &str, summary: String) {
        self.compression_log.push(CompressionEntry {
            action: action.to_string(),
            summary,
        });
    }

    pub fn apply(&mut self, update: TaskUpdate) {
        if let Some(plan) = update.plan {
            self.plan = plan.into_iter().map(PlanStep::new).collect();
        }
        if let Some(step) = update.current_step {
            self.current_step = Some(step);
        }
        if let Some(p) = update.progress {
            self.progress = p;
        }
        if let Some(s) = update.scratchpad {
            self.scratchpad = s;
        }
        if let Some(b) = update.blocked_on {
            self.blocked_on = b;
        }
        if let Some(r) = update.preserved_refs {
            self.preserved_refs = r;
        }
    }

    /// Open steps (not yet done), for renewal handoff.
    pub fn open_steps(&self) -> Vec<String> {
        self.plan
            .iter()
            .filter(|s| !s.done)
            .map(|s| s.label.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_state_compact_is_empty_string() {
        assert_eq!(TaskState::default().format_compact(), "");
    }

    #[test]
    fn goal_only_renders() {
        let ts = TaskState {
            goal: "Build it".to_string(),
            ..Default::default()
        };
        let s = ts.format_compact();
        assert!(s.contains("[TASK STATE] goal: Build it"));
    }

    #[test]
    fn plan_markers_correct() {
        let ts = TaskState {
            goal: "g".to_string(),
            plan: vec![
                PlanStep {
                    label: "step1".to_string(),
                    done: true,
                },
                PlanStep {
                    label: "step2".to_string(),
                    done: false,
                },
                PlanStep {
                    label: "step3".to_string(),
                    done: false,
                },
            ],
            current_step: Some(1),
            ..Default::default()
        };
        let s = ts.format_compact();
        assert!(s.contains("[done] 1. step1"));
        assert!(s.contains("[active] 2. step2"));
        assert!(s.contains("[todo] 3. step3"));
    }

    #[test]
    fn open_steps_excludes_done() {
        let ts = TaskState {
            goal: "g".to_string(),
            plan: vec![
                PlanStep {
                    label: "a".to_string(),
                    done: true,
                },
                PlanStep {
                    label: "b".to_string(),
                    done: false,
                },
            ],
            ..Default::default()
        };
        assert_eq!(ts.open_steps(), vec!["b"]);
    }

    #[test]
    fn apply_updates_fields() {
        let mut ts = TaskState::default();
        ts.apply(TaskUpdate {
            progress: Some("half done".to_string()),
            blocked_on: Some(vec!["waiting for data".to_string()]),
            ..Default::default()
        });
        assert_eq!(ts.progress, "half done");
        assert_eq!(ts.blocked_on, ["waiting for data"]);
    }
}
