//! Pins the Rust `SessionEvent` wire shape to the shared registry `contracts/session-events.json`.
//!
//! For every kind the registry assigns to `rust`, the registry sample must decode, re-encode to
//! exactly the sample (field names and nested shapes), and each optional field must be omittable
//! and omitted when absent. The kind list itself is checked against serde's own variant list, so
//! a variant added without a registry entry fails here.

use std::collections::BTreeSet;

use serde_json::Value;

use super::SessionEvent;

const REGISTRY: &str = include_str!("../../../../contracts/session-events.json");

fn rust_events() -> Vec<Value> {
    let registry: Value = serde_json::from_str(REGISTRY).expect("registry parses");
    registry["events"]
        .as_array()
        .expect("registry events")
        .iter()
        .filter(|event| {
            event["sdks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|sdk| sdk == "rust")
        })
        .cloned()
        .collect()
}

fn fields(event: &Value, key: &str) -> Vec<String> {
    event[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|field| field.as_str().unwrap().to_string())
        .collect()
}

/// Nested values Rust does not keep in memory, so they cannot survive a decode/encode cycle.
/// Each entry is `(kind, JSON pointer)`; everything else must round-trip byte-for-byte.
const LOSSY_PATHS: &[(&str, &str)] = &[
    // Advisory per-result token count; `ToolResult` has no field for it.
    ("tool_completed", "/results/0/token_count"),
];

fn without_lossy(kind: &str, mut value: Value) -> Value {
    for (lossy_kind, pointer) in LOSSY_PATHS {
        if *lossy_kind != kind {
            continue;
        }
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        if let Some(Value::Object(map)) = value.pointer_mut(parent) {
            map.remove(key);
        }
    }
    value
}

#[test]
fn kinds_match_the_registry_and_serde() {
    let registry: BTreeSet<String> = rust_events()
        .iter()
        .map(|event| event["kind"].as_str().unwrap().to_string())
        .collect();
    let declared: BTreeSet<String> = SessionEvent::KINDS
        .iter()
        .map(|kind| kind.to_string())
        .collect();
    assert_eq!(
        declared.len(),
        SessionEvent::KINDS.len(),
        "KINDS has duplicates"
    );
    assert_eq!(
        declared, registry,
        "SessionEvent::KINDS vs the registry's rust kinds"
    );

    // serde lists every variant tag when it meets an unknown one: the authoritative variant set.
    let error = serde_json::from_value::<SessionEvent>(serde_json::json!({ "kind": "__probe__" }))
        .unwrap_err()
        .to_string();
    let variants: BTreeSet<String> = error
        .split('`')
        .skip(3)
        .step_by(2)
        .map(str::to_string)
        .collect();
    assert_eq!(
        variants, declared,
        "serde variants vs SessionEvent::KINDS ({error})"
    );
}

#[test]
fn samples_round_trip_exactly() {
    for event in rust_events() {
        let kind = event["kind"].as_str().unwrap();
        let sample = event["sample"].clone();
        let decoded: SessionEvent = serde_json::from_value(sample.clone())
            .unwrap_or_else(|error| panic!("{kind}: registry sample does not decode: {error}"));
        assert_eq!(decoded.kind_str(), kind);
        let encoded = serde_json::to_value(&decoded).unwrap();
        assert_eq!(
            without_lossy(kind, encoded),
            without_lossy(kind, sample),
            "{kind}: re-encoded sample differs from the registry sample"
        );
    }
}

#[test]
fn optional_fields_are_omittable_and_omitted() {
    for event in rust_events() {
        let kind = event["kind"].as_str().unwrap();
        for field in fields(&event, "optional") {
            let mut sample = event["sample"].clone();
            sample.as_object_mut().unwrap().remove(&field);
            let decoded: SessionEvent = serde_json::from_value(sample).unwrap_or_else(|error| {
                panic!("{kind}: optional {field} is required in Rust: {error}")
            });
            let encoded = serde_json::to_value(&decoded).unwrap();
            assert!(
                encoded.get(&field).is_none(),
                "{kind}: absent optional {field} is written as {}",
                encoded[&field]
            );
        }
        for field in fields(&event, "required") {
            let encoded = serde_json::to_value(
                serde_json::from_value::<SessionEvent>(event["sample"].clone()).unwrap(),
            )
            .unwrap();
            assert!(
                encoded.get(&field).is_some(),
                "{kind}: required {field} is not written"
            );
        }
    }
}

#[test]
fn legacy_rust_events_upgrade_to_the_shared_shape() {
    let legacy = serde_json::json!({
        "kind": "llm_completed",
        "turn": 2,
        "message": {
            "role": "assistant",
            "content": "reading",
            "tool_calls": [{ "id": "c1", "name": "read_file", "arguments": { "path": "a.txt" } }]
        },
        "provider_replay": { "protocol": "anthropic-messages", "native_blocks": [{ "type": "text" }] }
    });
    let event = SessionEvent::from_wire_value(legacy).unwrap();
    let SessionEvent::LlmCompleted {
        message,
        wire_evidence,
        ..
    } = &event
    else {
        panic!("expected llm_completed");
    };
    assert_eq!(message.content.as_text(), Some("reading"));
    assert_eq!(message.tool_calls[0].arguments["path"], "a.txt");
    let evidence = wire_evidence.as_ref().unwrap();
    assert_eq!(evidence.protocol, "anthropic-messages");
    assert!(evidence.request_fingerprint.is_none());
    assert!(event.provider_replay().is_some());
    let encoded = serde_json::to_value(&event).unwrap();
    assert_eq!(
        encoded["tool_calls"][0]["arguments"],
        "{\"path\":\"a.txt\"}"
    );
    assert!(encoded.get("message").is_none() && encoded.get("provider_replay").is_none());

    let legacy = serde_json::json!({
        "kind": "tool_completed",
        "turn": 2,
        "results": [{ "call_id": "c1", "output": "body", "is_error": false }]
    });
    let encoded = serde_json::to_value(SessionEvent::from_wire_value(legacy).unwrap()).unwrap();
    assert_eq!(
        encoded["results"][0]["content"]["blocks"][0]["text"],
        "body"
    );

    let legacy = serde_json::json!({
        "kind": "workflow_batch_spawned",
        "turn": 1,
        "nodes": [{ "agent_id": "wf-node0" }, { "agent_id": "wf-node1" }]
    });
    let encoded = serde_json::to_value(SessionEvent::from_wire_value(legacy).unwrap()).unwrap();
    assert_eq!(encoded["node_count"], 2);
    assert_eq!(
        encoded["node_ids"],
        serde_json::json!(["wf-node0", "wf-node1"])
    );

    let legacy = serde_json::json!({
        "kind": "workflow_nodes_submitted", "turn": 3, "base": 1, "count": 2, "submitter": null
    });
    let event = SessionEvent::from_wire_value(legacy).unwrap();
    let SessionEvent::KernelObservation {
        turn,
        observation_kind,
        raw,
    } = event
    else {
        panic!("expected kernel_observation");
    };
    assert_eq!(
        (turn, observation_kind.as_str()),
        (3, "workflow_nodes_submitted")
    );
    assert_eq!(raw["count"], 2);

    let legacy =
        serde_json::json!({ "kind": "workflow_completed", "turn": 4, "node_outcomes": [] });
    let encoded = serde_json::to_value(SessionEvent::from_wire_value(legacy).unwrap()).unwrap();
    assert_eq!(encoded["total_nodes"], 0);
}
