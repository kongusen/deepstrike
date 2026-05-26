//! Runtime event types shared across SDK bindings.
//! I/O (append/read) lives in each language SDK — the kernel stays pure.

pub mod repair;
pub mod session;

pub use repair::{
    effective_provider_replay, pending_tool_calls_from_messages, repair_events,
    repair_events_with_cap, repair_llm_completed, repair_llm_completed_with_cap,
    sanitize_recovery_text, sanitize_recovery_text_bounded, synthesize_provider_replay,
};
pub use session::{ProviderReplay, SessionEvent};
