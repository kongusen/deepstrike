use serde::{Deserialize, Serialize};

/// A long-term memory entry as it crosses the FFI boundary (query results,
/// dream-pipeline curation payloads). Storage and retrieval are SDK concerns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub text: String,
    pub score: f64,
    pub metadata: serde_json::Value,
}
