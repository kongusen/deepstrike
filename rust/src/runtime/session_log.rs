use std::collections::HashMap;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use deepstrike_core::runtime::event_log::{Primitive, primitive_for_kind};
use deepstrike_core::runtime::session::SessionEvent;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::AsyncWriteExt;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEntry {
    pub seq: u64,
    pub event: SessionEvent,
}

#[async_trait]
pub trait SessionLog: Send + Sync {
    async fn append(&self, session_id: &str, event: SessionEvent) -> Result<u64, std::io::Error>;
    async fn read(
        &self,
        session_id: &str,
        from_seq: u64,
        primitive_filter: Option<Primitive>,
    ) -> Result<Vec<SessionEntry>, std::io::Error>;
    async fn latest_seq(&self, session_id: &str) -> Result<i64, std::io::Error>;
}

pub struct InMemorySessionLog {
    store: tokio::sync::Mutex<HashMap<String, Vec<SessionEntry>>>,
}

impl Default for InMemorySessionLog {
    fn default() -> Self {
        Self {
            store: tokio::sync::Mutex::new(HashMap::new()),
        }
    }
}

impl InMemorySessionLog {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SessionLog for InMemorySessionLog {
    async fn append(&self, session_id: &str, event: SessionEvent) -> Result<u64, std::io::Error> {
        let mut store = self.store.lock().await;
        let entries = store.entry(session_id.to_string()).or_default();
        let seq = entries.len() as u64;
        entries.push(SessionEntry { seq, event });
        Ok(seq)
    }

    async fn read(
        &self,
        session_id: &str,
        from_seq: u64,
        primitive_filter: Option<Primitive>,
    ) -> Result<Vec<SessionEntry>, std::io::Error> {
        let store = self.store.lock().await;
        Ok(store
            .get(session_id)
            .map(|entries| {
                entries
                    .iter()
                    .filter(|e| {
                        if e.seq < from_seq {
                            return false;
                        }
                        if let Some(pf) = primitive_filter {
                            if primitive_for_kind(e.event.kind_str()) != pf {
                                return false;
                            }
                        }
                        true
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn latest_seq(&self, session_id: &str) -> Result<i64, std::io::Error> {
        let store = self.store.lock().await;
        Ok(store
            .get(session_id)
            .map(|e| e.len() as i64 - 1)
            .unwrap_or(-1))
    }
}

/// Single-writer per session. Not safe for concurrent writers across processes.
pub struct FileSessionLog {
    dir: PathBuf,
    seq_counters: tokio::sync::Mutex<HashMap<String, u64>>,
}

impl FileSessionLog {
    pub fn new(dir: impl AsRef<Path>) -> Self {
        Self {
            dir: dir.as_ref().to_path_buf(),
            seq_counters: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    fn path(&self, session_id: &str) -> PathBuf {
        self.dir.join(format!("{session_id}.jsonl"))
    }

    async fn next_seq(&self, session_id: &str) -> Result<u64, std::io::Error> {
        let mut counters = self.seq_counters.lock().await;
        if !counters.contains_key(session_id) {
            let existing = self.read(session_id, 0, None).await?;
            counters.insert(session_id.to_string(), existing.len() as u64);
        }
        let seq = counters.get(session_id).copied().unwrap_or(0);
        counters.insert(session_id.to_string(), seq + 1);
        Ok(seq)
    }
}

#[async_trait]
impl SessionLog for FileSessionLog {
    async fn append(&self, session_id: &str, event: SessionEvent) -> Result<u64, std::io::Error> {
        fs::create_dir_all(&self.dir).await?;
        let seq = self.next_seq(session_id).await?;
        let line = serde_json::json!({ "seq": seq, "event": event });
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path(session_id))
            .await?;
        file.write_all(format!("{line}\n").as_bytes()).await?;
        Ok(seq)
    }

    async fn read(
        &self,
        session_id: &str,
        from_seq: u64,
        primitive_filter: Option<Primitive>,
    ) -> Result<Vec<SessionEntry>, std::io::Error> {
        let path = self.path(session_id);
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(path).await?;
        let mut results = Vec::new();
        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let raw: serde_json::Value = serde_json::from_str(line)?;
            let seq = raw["seq"].as_u64().unwrap_or(0);
            if seq < from_seq {
                continue;
            }
            let event: SessionEvent = serde_json::from_value(raw["event"].clone())?;
            if let Some(pf) = primitive_filter {
                if primitive_for_kind(event.kind_str()) != pf {
                    continue;
                }
            }
            results.push(SessionEntry { seq, event });
        }
        Ok(results)
    }

    async fn latest_seq(&self, session_id: &str) -> Result<i64, std::io::Error> {
        let entries = self.read(session_id, 0, None).await?;
        Ok(entries.len() as i64 - 1)
    }
}
