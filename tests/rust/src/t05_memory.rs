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
