//! Kernel observation → durable session event: the shared projection every SDK applies
//! (Node `kernelObservationToSessionEvent`, Python `kernel_observation_to_session_event`), pinned
//! across SDKs by the `kernel-observation-projection.json` conformance fixture.
//!
//! It works on the observation's JSON so an observation with no dedicated session kind is kept
//! verbatim as a `kernel_observation` envelope, exactly as the other SDKs keep it. Absent
//! optional fields are omitted, never written as `null`.

use deepstrike_core::runtime::KernelObservation;
use deepstrike_core::runtime::session::SessionEvent;
use serde_json::{Map, Value, json};

/// Inputs only the host knows: the session-log window a `compressed` event covers.
#[derive(Debug, Clone, Default)]
pub struct ProjectionOptions {
    pub next_archive_start: u64,
    /// Latest session-log seq, `None` when the log is empty.
    pub latest_seq: Option<u64>,
    pub preserved_refs: Vec<String>,
}

/// The four kernel pressure actions a session log records; anything else (`none`) is omitted.
pub fn canonical_compression_action(action: Option<&str>) -> Option<&str> {
    action.filter(|action| {
        matches!(
            *action,
            "snip_compact" | "micro_compact" | "context_collapse" | "auto_compact"
        )
    })
}

/// Project one kernel observation into its canonical session-event JSON, or `None` when the
/// observation produces no session record.
pub fn kernel_observation_to_session_event_value(
    obs: &Value,
    turn: u32,
    opts: &ProjectionOptions,
) -> Option<Value> {
    let event = project(obs, turn, opts)?;
    let Value::Object(event) = event else {
        return Some(event);
    };
    Some(Value::Object(
        event
            .into_iter()
            .filter(|(_, value)| !value.is_null())
            .collect(),
    ))
}

/// Typed projection for the runner. A projected event that does not decode (a kernel/SDK shape
/// drift) is still kept, as the raw `kernel_observation` envelope.
pub fn kernel_observation_to_session_event(
    observation: &KernelObservation,
    turn: u32,
    opts: &ProjectionOptions,
) -> Option<SessionEvent> {
    let obs = serde_json::to_value(observation).ok()?;
    let value = kernel_observation_to_session_event_value(&obs, turn, opts)?;
    serde_json::from_value(value).ok().or_else(|| {
        Some(SessionEvent::KernelObservation {
            turn: obs
                .get("turn")
                .and_then(Value::as_u64)
                .map_or(turn, |t| t as u32),
            observation_kind: obs["kind"].as_str().unwrap_or_default().to_string(),
            raw: obs,
        })
    })
}

/// `obs[key] ?? default` — only an absent or null value falls back.
fn or(obs: &Value, key: &str, default: Value) -> Value {
    match obs.get(key) {
        Some(value) if !value.is_null() => value.clone(),
        _ => default,
    }
}

/// `typeof obs[key] === "string" ? obs[key] : ""`.
fn text(obs: &Value, key: &str) -> Value {
    match obs.get(key) {
        Some(Value::String(value)) => Value::String(value.clone()),
        _ => Value::String(String::new()),
    }
}

fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Number(number)) => number.as_f64() != Some(0.0),
        Some(_) => true,
    }
}

fn action(obs: &Value) -> Value {
    canonical_compression_action(obs.get("action").and_then(Value::as_str))
        .map_or(Value::Null, |action| Value::String(action.to_string()))
}

fn empty_scope() -> Value {
    json!({ "tenant_id": "", "namespace": "" })
}

fn project(obs: &Value, turn: u32, opts: &ProjectionOptions) -> Option<Value> {
    let t = or(obs, "turn", Value::from(turn));
    let kind = obs.get("kind").and_then(Value::as_str).unwrap_or_default();
    let event = match kind {
        "compressed" => {
            let start = opts.next_archive_start;
            let latest = opts.latest_seq?;
            if latest < start {
                return None;
            }
            let summary = obs.get("summary").and_then(Value::as_str);
            json!({
                "kind": "compressed",
                "turn": t,
                "archived_seq_range": [start, latest],
                "action": action(obs),
                "summary": summary,
                "summary_tokens": summary
                    .filter(|summary| !summary.is_empty())
                    .map(|summary| (summary.encode_utf16().count() as u64).div_ceil(4).max(1)),
                "preserved_refs": opts.preserved_refs,
            })
        }
        "renewed" => json!({
            "kind": "context_renewed",
            "turn": t,
            "sprint": or(obs, "sprint", 0.into()),
            "handoff_ref": "",
        }),
        "lifecycle_report" => {
            let mut event = json!({
                "kind": "context_lifecycle_reported",
                "turn": t,
                "knowledge_entries": or(obs, "knowledge_entries", 0.into()),
                "knowledge_referenced": or(obs, "knowledge_referenced", 0.into()),
                "regret": or(obs, "regret", 0.into()),
                "waste_tokens": or(obs, "waste_tokens", 0.into()),
                "cache_loss_tokens": or(obs, "cache_loss_tokens", 0.into()),
                "recalled": or(obs, "recalled", 0.into()),
                "recall_expanded": or(obs, "recall_expanded", 0.into()),
            });
            for field in ["items", "history", "history_items"] {
                if let Some(rows) = obs.get(field).filter(|rows| rows.as_array().is_some_and(|a| !a.is_empty())) {
                    event[field] = rows.clone();
                }
            }
            if let Some(omitted) = obs.get("history_items_omitted").filter(|n| n.as_u64().is_some_and(|n| n > 0)) {
                event["history_items_omitted"] = omitted.clone();
            }
            event
        }
        "rollbacked" => json!({
            "kind": "rollbacked",
            "turn": t,
            "checkpoint_history_len": or(obs, "checkpoint_history_len", 0.into()),
            "reason": obs.get("reason"),
        }),
        "capability_changed" => {
            let mut event = json!({
                "kind": "capability_changed",
                "turn": t,
                "added": or(obs, "added", json!([])),
                "removed": or(obs, "removed", json!([])),
            });
            for key in [
                "change_kind",
                "capability_id",
                "version",
                "mounted_by",
                "mount_reason",
            ] {
                event[key] = or(obs, key, Value::Null);
            }
            event
        }
        "milestone_advanced" => json!({
            "kind": "milestone_advanced",
            "turn": t,
            "phase_id": or(obs, "phase_id", "".into()),
            "capabilities_unlocked": or(obs, "capabilities_unlocked", json!([])),
        }),
        "milestone_blocked" => json!({
            "kind": "milestone_blocked",
            "turn": t,
            "phase_id": or(obs, "phase_id", "".into()),
            "reason": text(obs, "reason"),
        }),
        "checkpoint_taken" => json!({
            "kind": "checkpoint_taken",
            "turn": t,
            "history_len": or(obs, "history_len", 0.into()),
        }),
        "entropy_sample" => json!({
            "kind": "entropy_sample",
            "turn": t,
            "score": or(obs, "score", 0.into()),
            "rho": or(obs, "rho", 0.into()),
            "repeat_pressure": or(obs, "repeat_pressure", 0.into()),
            "failure_rate": or(obs, "failure_rate", 0.into()),
            "rollbacks_in_window": or(obs, "rollbacks_in_window", 0.into()),
            "window_turns": or(obs, "window_turns", 0.into()),
        }),
        "entropy_alert" => json!({
            "kind": "entropy_alert",
            "turn": t,
            "score": or(obs, "score", 0.into()),
            "threshold": or(obs, "threshold", 0.into()),
        }),
        "agent_process_changed" => json!({
            "kind": "agent_process_changed",
            "turn": t,
            "agent_id": or(obs, "agent_id", "".into()),
            "parent_task_id": or(obs, "parent_task_id", "".into()),
            "role": or(obs, "role", "".into()),
            "isolation": or(obs, "isolation", "".into()),
            "context_inheritance": or(obs, "context_inheritance", "".into()),
            "state": or(obs, "state", "running".into()),
            "permitted_capability_ids": or(obs, "permitted_capability_ids", json!([])),
            "result_termination": if truthy(obs.get("result_termination")) {
                obs["result_termination"].clone()
            } else {
                Value::Null
            },
        }),
        "tool_gated" => json!({
            "kind": "tool_gated",
            "turn": t,
            "call_id": or(obs, "call_id", "".into()),
            "tool": or(obs, "tool", "".into()),
            "reason": text(obs, "reason"),
        }),
        "signal_delivery_disposed" => json!({
            "kind": "signal_delivery_disposed",
            "turn": t,
            "operation_id": or(obs, "operation_id", "".into()),
            "delivery_id": or(obs, "delivery_id", "".into()),
            "attempt": or(obs, "attempt", 0.into()),
            "signal_id": or(obs, "signal_id", "".into()),
            "disposition": or(obs, "disposition", "".into()),
            "queue_depth": or(obs, "queue_depth", 0.into()),
        }),
        "budget_exceeded" => json!({
            "kind": "budget_exceeded",
            "turn": t,
            "budget": or(obs, "budget", "".into()),
            "operation_id": or(obs, "operation_id", "".into()),
            "reservation_id": if truthy(obs.get("reservation_id")) {
                obs["reservation_id"].clone()
            } else {
                Value::Null
            },
        }),
        "budget_usage_reported" => json!({
            "kind": "budget_usage_reported",
            "turn": t,
            "operation_id": or(obs, "operation_id", "".into()),
            "reservation_id": or(obs, "reservation_id", "".into()),
            "tokens": or(obs, "tokens", 0.into()),
            "subagents": or(obs, "subagents", 0.into()),
            "rounds": or(obs, "rounds", 0.into()),
        }),
        "operation_cancelled" => json!({
            "kind": "operation_cancelled",
            "turn": t,
            "operation_id": or(obs, "operation_id", "".into()),
            "reason": or(obs, "reason", "user".into()),
            "pending_call_ids": or(obs, "pending_call_ids", json!([])),
        }),
        "suspended" => json!({
            "kind": "suspended",
            "turn": t,
            "reason": text(obs, "reason"),
            "pending_calls": or(obs, "pending_calls", json!([])),
        }),
        "resumed" => json!({
            "kind": "resumed",
            "turn": t,
            "approved": or(obs, "approved", json!([])),
            "denied": or(obs, "denied", json!([])),
        }),
        "page_in_requested" => return None,
        "page_out_archived" => json!({
            "kind": "page_out",
            "turn": t,
            "action": action(obs),
            "summary": obs.get("summary"),
            "tier_hint": obs.get("tier"),
            "message_count": or(obs, "message_count", 0.into()),
            "archive_ref": obs.get("archive_ref"),
        }),
        "memory_written" => json!({
            "kind": "memory_written",
            "turn": t,
            "record_id": or(obs, "record_id", "".into()),
            "scope": or(obs, "scope", empty_scope()),
            "memory_kind": or(obs, "memory_kind", "".into()),
            "name": or(obs, "name", "".into()),
            "size_bytes": or(obs, "size_bytes", 0.into()),
        }),
        // M2 · the kernel's PersistMemory effect settled as a candidate (the child-request path).
        "memory_candidate_pending" => json!({
            "kind": "memory_candidate_pending",
            "turn": t,
            "candidate_id": match or(obs, "candidate_id", "".into()) {
                Value::String(id) => id,
                other => other.to_string(),
            },
            "source": "child_request",
            "name": or(obs, "name", "".into()),
        }),
        "memory_queried" => json!({
            "kind": "memory_queried",
            "turn": t,
            "scope": or(obs, "scope", empty_scope()),
            "query": or(obs, "query", "".into()),
            "requested_k": or(obs, "requested_k", 0.into()),
            "requires_async_response": or(obs, "requires_async_response", false.into()),
        }),
        "memory_validation_failed" => json!({
            "kind": "memory_validation_failed",
            "turn": t,
            "record_id": or(obs, "record_id", "".into()),
            "error": or(obs, "error", "".into()),
        }),
        "memory_write_failed" => json!({
            "kind": "memory_write_failed",
            "turn": t,
            "record_id": or(obs, "record_id", "".into()),
            "error": or(obs, "error", "".into()),
        }),
        "memory_query_failed" => json!({
            "kind": "memory_query_failed",
            "turn": t,
            "scope": or(obs, "scope", empty_scope()),
            "query": or(obs, "query", "".into()),
            "error": or(obs, "error", "".into()),
        }),
        // Batch metadata for resume recovery; node specs stay in the kernel observation.
        "workflow_batch_spawned" => {
            let nodes = obs
                .get("nodes")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            json!({
                "kind": "workflow_batch_spawned",
                "turn": t,
                "node_count": nodes.len(),
                "node_ids": nodes
                    .iter()
                    .map(|node| or(node, "agent_id", "".into()))
                    .collect::<Vec<_>>(),
            })
        }
        "workflow_completed" => {
            let outcomes = or(obs, "node_outcomes", json!([]));
            json!({
                "kind": "workflow_completed",
                "turn": t,
                "total_nodes": outcomes.as_array().map_or(0, Vec::len),
                "node_outcomes": outcomes,
            })
        }
        _ => json!({
            "kind": "kernel_observation",
            "turn": t,
            "observation_kind": kind,
            "raw": Value::Object(obs.as_object().cloned().unwrap_or_else(Map::new)),
        }),
    };
    Some(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shared cross-SDK fixture; Node is the reference the expectations were generated from.
    const FIXTURE: &str = include_str!(
        "../../../tests/fixtures/sdk-conformance/canonical/kernel-observation-projection.json"
    );

    pub(crate) fn project_fixture_cases(input: &Value) -> Value {
        let mut events = Map::new();
        for case in input["cases"].as_array().unwrap() {
            let options = &case["options"];
            let opts = ProjectionOptions {
                next_archive_start: options["next_archive_start"].as_u64().unwrap_or(0),
                latest_seq: options["latest_seq"].as_u64(),
                preserved_refs: serde_json::from_value(options["preserved_refs"].clone())
                    .unwrap_or_default(),
            };
            let turn = case["turn"].as_u64().unwrap() as u32;
            let event =
                kernel_observation_to_session_event_value(&case["observation"], turn, &opts);
            events.insert(
                case["name"].as_str().unwrap().into(),
                event.unwrap_or(Value::Null),
            );
        }
        Value::Object(events)
    }

    #[test]
    fn projection_matches_the_shared_fixture() {
        let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
        let actual = project_fixture_cases(&fixture["input"]);
        let expected = &fixture["expected"]["canonical"]["events"];
        for (name, want) in expected.as_object().unwrap() {
            assert_eq!(&actual[name], want, "case {name}");
        }
        assert_eq!(
            actual.as_object().unwrap().len(),
            expected.as_object().unwrap().len()
        );
    }

    #[test]
    fn every_projected_fixture_event_decodes_as_a_rust_session_event() {
        let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
        for (name, event) in project_fixture_cases(&fixture["input"])
            .as_object()
            .unwrap()
        {
            if event.is_null() {
                continue;
            }
            serde_json::from_value::<SessionEvent>(event.clone())
                .unwrap_or_else(|error| panic!("case {name}: {error}"));
        }
    }
}
