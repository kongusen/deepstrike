use async_trait::async_trait;

/// Inject run-scoped evidence (RAG, API responses) before the first LLM call.
#[async_trait]
pub trait KnowledgeSource: Send + Sync {
    async fn retrieve(&self, goal: &str, top_k: usize) -> crate::Result<Vec<String>>;
    /// One-time warmup called before the first run (load index, open connection, etc.).
    async fn init(&self) -> crate::Result<()>;
}
