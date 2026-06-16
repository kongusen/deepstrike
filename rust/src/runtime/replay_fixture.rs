//! Fixture helpers for `ReplayProvider`.
//!
//! Rust port of node/src/runtime/replay-fixture.ts. Walks `llm_completed` events from a
//! recorded session log and returns the ordered list of assistant Messages that
//! `ReplayProvider` consumes.

use deepstrike_core::runtime::session::SessionEvent;
use deepstrike_core::types::message::Message;

use super::session_log::SessionEntry;

/// Walk `SessionEntry`s (the wrapped `{ seq, event }` shape `SessionLog::read()` returns)
/// and produce the ordered list of assistant messages.
pub fn extract_recorded_messages_from_entries(entries: &[SessionEntry]) -> Vec<Message> {
    entries
        .iter()
        .filter_map(|e| message_from_event(&e.event))
        .collect()
}

/// Walk bare `SessionEvent`s — useful when reading a `serde_json::Value` log file rather
/// than a `SessionLog`.
pub fn extract_recorded_messages(events: &[SessionEvent]) -> Vec<Message> {
    events.iter().filter_map(message_from_event).collect()
}

fn message_from_event(event: &SessionEvent) -> Option<Message> {
    if let SessionEvent::LlmCompleted { message, .. } = event {
        Some(message.clone())
    } else {
        None
    }
}
