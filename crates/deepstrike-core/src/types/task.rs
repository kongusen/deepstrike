use serde::{Deserialize, Serialize};

/// Freeform classification label carried on a task, round-tripped transparently.
///
/// This is a pass-through label for host-side bookkeeping — the kernel attaches
/// NO scheduling semantics to it (`TaskGraph::ready_tasks` filters by status
/// only). Callers may use any value (e.g. `"prd-fill"`, `"eval"`); the constants
/// below are conventional names, not gates.
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
