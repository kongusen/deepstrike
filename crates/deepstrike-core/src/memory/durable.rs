use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::types::error::{DeepStrikeError, Result};
use crate::types::message::Message;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionData {
    pub session_id: String,
    pub agent_id: String,
    pub messages: Vec<Message>,
    pub metadata: serde_json::Value,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub session_id: String,
    pub agent_id: String,
    pub message_count: usize,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}

pub trait SessionStore: Send + Sync {
    fn save(&self, session_id: &str, data: &SessionData) -> Result<()>;
    fn load(&self, session_id: &str) -> Result<Option<SessionData>>;
    fn list(&self, agent_id: &str) -> Result<Vec<SessionMeta>>;
    fn delete(&self, session_id: &str) -> Result<()>;
}

pub struct InMemoryStore {
    data: std::sync::Mutex<HashMap<String, SessionData>>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self {
            data: std::sync::Mutex::new(HashMap::new()),
        }
    }
}

impl Default for InMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionStore for InMemoryStore {
    fn save(&self, session_id: &str, data: &SessionData) -> Result<()> {
        self.data
            .lock()
            .map_err(|e| DeepStrikeError::InvalidConfig(e.to_string()))?
            .insert(session_id.to_string(), data.clone());
        Ok(())
    }

    fn load(&self, session_id: &str) -> Result<Option<SessionData>> {
        Ok(self
            .data
            .lock()
            .map_err(|e| DeepStrikeError::InvalidConfig(e.to_string()))?
            .get(session_id)
            .cloned())
    }

    fn list(&self, agent_id: &str) -> Result<Vec<SessionMeta>> {
        let guard = self
            .data
            .lock()
            .map_err(|e| DeepStrikeError::InvalidConfig(e.to_string()))?;
        Ok(guard
            .values()
            .filter(|d| d.agent_id == agent_id)
            .map(|d| SessionMeta {
                session_id: d.session_id.clone(),
                agent_id: d.agent_id.clone(),
                message_count: d.messages.len(),
                created_at_ms: d.created_at_ms,
                updated_at_ms: d.updated_at_ms,
            })
            .collect())
    }

    fn delete(&self, session_id: &str) -> Result<()> {
        self.data
            .lock()
            .map_err(|e| DeepStrikeError::InvalidConfig(e.to_string()))?
            .remove(session_id);
        Ok(())
    }
}
