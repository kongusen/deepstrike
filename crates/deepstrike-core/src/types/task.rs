use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeTask {
    pub goal: String,
    pub criteria: Vec<String>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl RuntimeTask {
    pub fn new(goal: impl Into<String>) -> Self {
        Self {
            goal: goal.into(),
            criteria: Vec::new(),
            metadata: serde_json::Value::Null,
        }
    }

    pub fn with_criteria(mut self, criteria: Vec<String>) -> Self {
        self.criteria = criteria;
        self
    }
}
