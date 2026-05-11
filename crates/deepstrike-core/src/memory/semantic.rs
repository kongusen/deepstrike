use std::collections::{HashSet, VecDeque};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::types::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub text: String,
    pub score: f64,
    pub metadata: serde_json::Value,
}

pub trait SemanticMemory: Send + Sync {
    fn query(&self, text: &str, top_k: usize) -> Result<Vec<MemoryEntry>>;
    fn store(&self, entry: MemoryEntry) -> Result<()>;
}

const MAX_ENTRIES: usize = 10_000;

pub struct InMemorySemanticStore {
    entries: Mutex<VecDeque<MemoryEntry>>,
}

impl InMemorySemanticStore {
    pub fn new() -> Self {
        Self { entries: Mutex::new(VecDeque::new()) }
    }

    fn jaccard(a: &str, b: &str) -> f64 {
        let sa: HashSet<&str> = a.split_whitespace().collect();
        let sb: HashSet<&str> = b.split_whitespace().collect();
        let inter = sa.intersection(&sb).count();
        let union = sa.union(&sb).count();
        if union == 0 { 0.0 } else { inter as f64 / union as f64 }
    }
}

impl Default for InMemorySemanticStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SemanticMemory for InMemorySemanticStore {
    fn store(&self, entry: MemoryEntry) -> Result<()> {
        let mut entries = self.entries.lock().unwrap();
        if entries.len() >= MAX_ENTRIES {
            entries.pop_front();
        }
        entries.push_back(entry);
        Ok(())
    }

    fn query(&self, text: &str, top_k: usize) -> Result<Vec<MemoryEntry>> {
        let entries = self.entries.lock().unwrap();
        let mut scored: Vec<(f64, &MemoryEntry)> = entries
            .iter()
            .map(|e| (Self::jaccard(text, &e.text), e))
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
        Ok(scored.into_iter().take(top_k).map(|(score, e)| MemoryEntry { score, ..e.clone() }).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_returns_top_k_by_jaccard() {
        let store = InMemorySemanticStore::new();
        store.store(MemoryEntry { text: "foo bar baz".into(), score: 0.0, metadata: serde_json::Value::Null }).unwrap();
        store.store(MemoryEntry { text: "hello world".into(), score: 0.0, metadata: serde_json::Value::Null }).unwrap();
        let results = store.query("foo bar", 1).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].text.contains("foo"));
    }
}
