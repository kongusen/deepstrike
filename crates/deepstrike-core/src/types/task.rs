use serde::{Deserialize, Serialize};

/// Parallelism hint for task scheduling and executor enforcement.
///
/// - `Orchestrate`: serial, produces contracts; runs one at a time
/// - `Implement`: serial, DAG chain; `TaskGraph::ready_tasks` enforces max 1 running
/// - `Retrieve`: parallelisable; no mutual exclusion between retrieve tasks
/// - `Verify`: parallelisable, but each task must run in an isolated agent context
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskLane {
    Orchestrate,
    #[default]
    Implement,
    Retrieve,
    Verify,
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
