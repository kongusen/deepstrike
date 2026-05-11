use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct HarnessRequest {
    pub goal: String,
    pub criteria: Vec<String>,
    pub extensions: Option<serde_json::Value>,
}

impl HarnessRequest {
    pub fn new(goal: impl Into<String>) -> Self {
        Self { goal: goal.into(), criteria: Vec::new(), extensions: None }
    }
}

#[derive(Debug, Clone)]
pub struct HarnessOutcome {
    pub result: String,
    pub passed: bool,
    pub iterations: u32,
    pub total_tokens: u64,
    pub status: String,
    pub feedback: Option<String>,
}

#[async_trait]
pub trait QualityGate: Send + Sync {
    async fn evaluate(&self, request: &HarnessRequest, outcome: &HarnessOutcome) -> crate::Result<bool>;
}

#[async_trait]
pub trait Harness: Send + Sync {
    async fn run(&self, request: HarnessRequest) -> crate::Result<HarnessOutcome>;
}
