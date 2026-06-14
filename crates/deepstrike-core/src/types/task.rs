use serde::{Deserialize, Serialize};

/// Freeform parallelism / classification hint for task scheduling.
///
/// The kernel transparently round-trips any string the caller sets.  Well-known
/// constants are provided for the built-in workflow templates:
///
/// - `"orchestrate"` — serial, produces contracts; runs one at a time
/// - `"implement"`   — serial, DAG chain; `TaskGraph::ready_tasks` enforces max 1
/// - `"retrieve"`    — parallelisable; no mutual exclusion
/// - `"verify"`      — parallelisable, isolated agent context
///
/// Callers may use any other value (e.g. `"prd-fill"`, `"eval"`) without kernel
/// changes — the field is a transparent label, not a scheduling gate.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(transparent)]
pub struct TaskLane(pub String);

impl TaskLane {
    pub const ORCHESTRATE: &'static str = "orchestrate";
    pub const IMPLEMENT: &'static str = "implement";
    pub const RETRIEVE: &'static str = "retrieve";
    pub const VERIFY: &'static str = "verify";

    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

impl std::fmt::Display for TaskLane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RuntimeTask {
    pub goal: String,
    pub criteria: Vec<String>,
    #[serde(default)]
    pub metadata: serde_json::Value,
    #[serde(default)]
    pub lane: TaskLane,
}

impl RuntimeTask {
    pub fn new(goal: impl Into<String>) -> Self {
        Self {
            goal: goal.into(),
            criteria: Vec::new(),
            metadata: serde_json::Value::Null,
            lane: TaskLane::default(),
        }
    }

    pub fn with_criteria(mut self, criteria: Vec<String>) -> Self {
        self.criteria = criteria;
        self
    }

    pub fn with_lane(mut self, lane: TaskLane) -> Self {
        self.lane = lane;
        self
    }
}
