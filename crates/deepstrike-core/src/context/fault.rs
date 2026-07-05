use serde::{Deserialize, Serialize};

/// Errors/Faults that can occur during Context VM execution or replay.
#[derive(Debug, Clone, thiserror::Error, Serialize, Deserialize)]
pub enum ContextFault {
    #[error("Prompt exceeds maximum token limit: budget={budget}, actual={actual}")]
    PromptTooLong { budget: u32, actual: u32 },
    #[error("Missing archive chunk {seq} for session {session_id}")]
    MissingArchive { session_id: String, seq: u64 },
    #[error("Invalid replay at turn {turn}: {reason}")]
    InvalidReplay { turn: u32, reason: String },
}

/// FNV-1a 64-bit — the render layer's cache-stability test fingerprint dialect.
#[cfg(test)]
pub(crate) fn stable_hash(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
