use serde::{Deserialize, Serialize};

use crate::types::message::Message;

/// A completed session's transcript as fed into the dream pipeline over FFI.
/// Persistence is an SDK concern; the kernel only analyzes the payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionData {
    pub session_id: String,
    pub agent_id: String,
    pub messages: Vec<Message>,
    pub metadata: serde_json::Value,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
}
