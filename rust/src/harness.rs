use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct Criterion {
    pub text: String,
    pub required: bool,
    pub weight: f32,
}

impl Criterion {
    pub fn required(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            required: true,
            weight: 1.0,
        }
    }

    pub fn optional(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            required: false,
            weight: 1.0,
        }
    }

    pub fn with_weight(mut self, w: f32) -> Self {
        self.weight = w;
        self
    }
}

#[derive(Debug, Clone)]
pub struct CriterionResult {
    pub criterion: String,
    pub passed: bool,
    pub score: f32,
    pub feedback: String,
}

#[derive(Debug, Clone)]
pub struct HarnessRequest {
    pub goal: String,
    pub criteria: Vec<Criterion>,
    pub extensions: Option<serde_json::Value>,
}

impl HarnessRequest {
    pub fn new(goal: impl Into<String>) -> Self {
        Self {
            goal: goal.into(),
            criteria: Vec::new(),
            extensions: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct HarnessOutcome {
    pub result: String,
    pub passed: bool,
    pub iterations: u32,
    pub total_tokens: u64,
    pub status: String,
    pub overall_score: f32,
    pub feedback: Option<String>,
    pub details: Vec<CriterionResult>,
}

#[derive(Debug, Clone)]
pub struct Verdict {
    pub passed: bool,
    pub overall_score: f32,
    pub feedback: String,
    pub details: Vec<CriterionResult>,
}

#[derive(Debug, Clone)]
pub enum HarnessEvent {
    Token(String),
    ToolCall {
        id: String,
        name: String,
    },
    ToolResult {
        call_id: String,
        content: String,
        is_error: bool,
    },
    Supervising,
    Revising {
        verdict: Verdict,
    },
    Done {
        verdict: Verdict,
        iterations: u32,
        total_tokens: u64,
        status: String,
    },
    MaxAttemptsReached,
}

#[async_trait]
pub trait QualityGate: Send + Sync {
    async fn evaluate(
        &self,
        request: &HarnessRequest,
        outcome: &HarnessOutcome,
    ) -> crate::Result<bool>;
}

#[async_trait]
pub trait Harness: Send + Sync {
    async fn run(&self, request: HarnessRequest) -> crate::Result<HarnessOutcome>;
}
