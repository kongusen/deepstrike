use deepstrike_core::memory::working::WorkingMemory;
use deepstrike_core::types::signal::{RuntimeSignal, SignalSource, SignalType, Urgency};
use compact_str::CompactString;

// ─── SDK-level WorkingMemory (deepstrike_sdk) ───────────────────────────────

#[test]
fn sdk_working_memory_set_get_clear() {
    let mut mem = deepstrike_sdk::WorkingMemory::default();
    mem.set("step", 1);
    assert_eq!(mem.get("step"), Some(&serde_json::json!(1)));
    mem.set("name", "test");
    assert_eq!(mem.get("name"), Some(&serde_json::json!("test")));
    mem.clear();
    assert!(mem.get("step").is_none());
    assert!(mem.get("name").is_none());
}

#[test]
fn sdk_working_memory_overwrite() {
    let mut mem = deepstrike_sdk::WorkingMemory::default();
    mem.set("key", 1);
    mem.set("key", 2);
    assert_eq!(mem.get("key"), Some(&serde_json::json!(2)));
}

#[test]
fn sdk_working_memory_get_missing_returns_none() {
    let mem = deepstrike_sdk::WorkingMemory::default();
    assert!(mem.get("nonexistent").is_none());
}

// ─── Kernel WorkingMemory ───────────────────────────────────────────────────

#[test]
fn kernel_working_memory_default() {
    let wm = WorkingMemory::new();
    assert!(wm.pending_signals.is_empty());
    assert!(wm.tool_cache.is_empty());
    assert!(wm.scratch.is_empty());
}

#[test]
fn kernel_working_memory_cache_tool_result() {
    let mut wm = WorkingMemory::new();
    wm.cache_tool_result(CompactString::new("c1"), CompactString::new("result-abc"));
    assert_eq!(wm.get_cached("c1").map(|s| s.as_str()), Some("result-abc"));
    assert!(wm.get_cached("c2").is_none());
}

#[test]
fn kernel_working_memory_add_and_drain_signals() {
    let mut wm = WorkingMemory::new();
    let sig = RuntimeSignal::new(SignalSource::Cron, SignalType::Event, Urgency::Normal, "tick");
    wm.add_signal(sig);
    assert_eq!(wm.pending_signals.len(), 1);

    let drained = wm.drain_signals();
    assert_eq!(drained.len(), 1);
    assert!(wm.pending_signals.is_empty());
}

#[test]
fn kernel_working_memory_drain_empty() {
    let mut wm = WorkingMemory::new();
    let drained = wm.drain_signals();
    assert!(drained.is_empty());
}

#[test]
fn kernel_working_memory_clear() {
    let mut wm = WorkingMemory::new();
    wm.cache_tool_result(CompactString::new("c1"), CompactString::new("v"));
    wm.scratch.insert("key".into(), serde_json::json!("val"));
    wm.add_signal(RuntimeSignal::new(SignalSource::Cron, SignalType::Event, Urgency::Normal, "s"));
    wm.clear();
    assert!(wm.pending_signals.is_empty());
    assert!(wm.tool_cache.is_empty());
    assert!(wm.scratch.is_empty());
}

#[test]
fn kernel_working_memory_scratch() {
    let mut wm = WorkingMemory::new();
    wm.scratch.insert("count".into(), serde_json::json!(42));
    assert_eq!(wm.scratch.get("count"), Some(&serde_json::json!(42)));
}
