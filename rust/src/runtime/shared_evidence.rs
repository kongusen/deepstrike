//! Storage-neutral shared evidence projection.
//!
//! This read model composes session-log observations, kernel-journal bytes, knowledge and memory
//! references without writing to any source. The wire-value entry point is also used by the
//! conformance adapter so event bytes remain the exact canonical session-event bytes supplied by a
//! host; typed callers can obtain those values through `SessionEvent::from_wire_value` first.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use super::kernel_journal::KernelJournal;
use super::session_log::SessionLog;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct EvidenceScope {
    pub operation_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceCursor {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_log_seq: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_step_seq: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_ordinal: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceDiagnostic {
    pub code: String,
    #[serde(skip_serializing)]
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvidenceLink {
    pub relation: String,
    pub domain: String,
    pub ref_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvidenceRetention {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_ttl_days: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prunable: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvidenceRef {
    pub ref_id: String,
    pub domain: String,
    pub authority: String,
    pub availability: String,
    #[serde(skip_serializing)]
    pub scope: EvidenceScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settlement: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_refs: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub links: Option<Vec<EvidenceLink>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention: Option<EvidenceRetention>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SharedEvidenceSnapshot {
    pub operation_id: String,
    pub scope: EvidenceScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub as_of: Option<EvidenceCursor>,
    pub completeness: String,
    pub diagnostics: Vec<EvidenceDiagnostic>,
    pub refs: Vec<EvidenceRef>,
    pub verification_journal: Vec<Vec<u8>>,
    pub verification_session_logs: Vec<Vec<Vec<u8>>>,
    pub verification_checkpoints: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, Default)]
struct ObservedRefs {
    values: BTreeMap<String, EvidenceRef>,
}

impl ObservedRefs {
    fn key(domain: &str, ref_id: &str) -> String {
        format!("{domain}\0{ref_id}")
    }

    fn put(&mut self, mut incoming: EvidenceRef) {
        let key = Self::key(&incoming.domain, &incoming.ref_id);
        if let Some(existing) = self.values.get_mut(&key) {
            if incoming.content_digest.is_some() {
                existing.content_digest = incoming.content_digest.take();
            }
            if incoming.payload_ref.is_some() {
                existing.payload_ref = incoming.payload_ref.take();
            }
            if incoming.size_bytes.is_some() {
                existing.size_bytes = incoming.size_bytes.take();
            }
            if incoming.version.is_some() {
                existing.version = incoming.version.take();
            }
            if incoming.provenance.is_some() {
                existing.provenance = incoming.provenance.take();
            }
            if incoming.settlement.is_some() {
                existing.settlement = incoming.settlement.take();
            }
            if incoming.evidence_refs.is_some() {
                existing.evidence_refs = incoming.evidence_refs.take();
            }
            if incoming.links.is_some() {
                existing.links = incoming.links.take();
            }
            if incoming.retention.is_some() {
                existing.retention = incoming.retention.take();
            }
        } else {
            self.values.insert(key, incoming);
        }
    }

    fn has(&self, domain: &str, ref_id: &str) -> bool {
        self.values.contains_key(&Self::key(domain, ref_id))
    }
}

fn ref_for(
    scope: &EvidenceScope,
    ref_id: &str,
    domain: &str,
    authority: &str,
    availability: &str,
) -> EvidenceRef {
    EvidenceRef {
        ref_id: ref_id.into(),
        domain: domain.into(),
        authority: authority.into(),
        availability: availability.into(),
        scope: scope.clone(),
        content_digest: None,
        payload_ref: None,
        size_bytes: None,
        version: None,
        provenance: None,
        settlement: None,
        evidence_refs: None,
        links: None,
        retention: None,
    }
}

fn diagnostic(
    diagnostics: &mut Vec<EvidenceDiagnostic>,
    code: &str,
    message: impl Into<String>,
    domain: Option<&str>,
    ref_id: Option<&str>,
) {
    diagnostics.push(EvidenceDiagnostic {
        code: code.into(),
        message: message.into(),
        domain: domain.map(str::to_owned),
        ref_id: ref_id.map(str::to_owned),
    });
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

/// Canonical event JSON: UTF-16 key order, compact encoding, null object members omitted, array nulls kept.
pub fn canonical_event_json(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(value) => value.to_string(),
        Value::Number(number) => {
            if let Some(float) = number.as_f64() {
                if float.is_finite()
                    && float.fract() == 0.0
                    && float.abs() <= 9_007_199_254_740_991.0
                {
                    return (float as i64).to_string();
                }
            }
            number.to_string()
        }
        Value::String(value) => serde_json::to_string(value).expect("string serializes"),
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_event_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Object(values) => {
            let mut keys: Vec<&String> = values.keys().collect();
            keys.sort_by_key(|key| key.encode_utf16().collect::<Vec<_>>());
            let fields = keys
                .into_iter()
                .filter_map(|key| {
                    let value = values.get(key)?;
                    if value.is_null() {
                        return None;
                    }
                    Some(format!(
                        "{}:{}",
                        serde_json::to_string(key).ok()?,
                        canonical_event_json(value)
                    ))
                })
                .collect::<Vec<_>>();
            format!("{{{}}}", fields.join(","))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnowledgeIdentity {
    pub ref_id: String,
    pub key: String,
    pub origin: Option<String>,
    pub links: Option<Vec<EvidenceLink>>,
}

pub fn knowledge_identity(entry_id: &str, content_digest: &str) -> KnowledgeIdentity {
    let mut key = entry_id.to_string();
    let prefix = "knowledge:";
    let suffix = format!(":{content_digest}");
    if entry_id.starts_with(prefix) && entry_id.ends_with(&suffix) {
        let body = &entry_id[prefix.len()..entry_id.len() - suffix.len()];
        if let Some((ordinal, rest)) = body.split_once(':') {
            if !ordinal.is_empty()
                && ordinal.chars().all(|c| c.is_ascii_digit())
                && !rest.is_empty()
            {
                key = rest.into();
            }
        }
    }
    let ref_id = format!("{key}@{content_digest}");
    let (origin, target) = key.split_once(':').map_or((None, ""), |(prefix, target)| {
        (
            match prefix {
                "memory" => Some("memory".into()),
                "skill" => Some("skill".into()),
                _ => None,
            },
            target,
        )
    });
    let links = if origin.as_deref() == Some("memory") && !target.is_empty() {
        Some(vec![EvidenceLink {
            relation: "recalled_from".into(),
            domain: "memory_record".into(),
            ref_id: target.into(),
        }])
    } else {
        None
    };
    KnowledgeIdentity {
        ref_id,
        key,
        origin,
        links,
    }
}

fn memory_in_scope(record: &Value, scope: &EvidenceScope) -> bool {
    let memory = record.get("scope").unwrap_or(record);
    (scope.tenant_id.is_none()
        || memory.get("tenant_id").and_then(Value::as_str) == scope.tenant_id.as_deref())
        && (scope.namespace.is_none()
            || memory.get("namespace").and_then(Value::as_str) == scope.namespace.as_deref())
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn observe_event(
    seq: u64,
    event: &Value,
    scope: &EvidenceScope,
    refs: &mut ObservedRefs,
    knowledge: &mut BTreeMap<String, (String, String)>,
    diagnostics: &mut Vec<EvidenceDiagnostic>,
) {
    let bytes = canonical_event_json(event).into_bytes();
    let payload_ref = format!("sessionlog:{seq}");
    let mut session = ref_for(
        scope,
        &seq.to_string(),
        "session_event",
        "host_observation",
        "resident",
    );
    session.content_digest = Some(digest(&bytes));
    session.size_bytes = Some(bytes.len() as u64);
    session.payload_ref = Some(payload_ref.clone());
    refs.put(session);
    let kind = event
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match kind {
        "provider_attempt" => {
            if let (Some(effect), Some(attempt)) = (
                string_field(event, "effect_id"),
                event.get("attempt_seq").and_then(Value::as_u64),
            ) {
                let mut reference = ref_for(
                    scope,
                    &format!("{effect}#{attempt}"),
                    "provider_attempt",
                    "host_observation",
                    "resident",
                );
                reference.payload_ref = Some(payload_ref);
                refs.put(reference);
            }
        }
        "prompt_measured" => {
            let ref_id = string_field(event, "effect_id").unwrap_or_else(|| {
                format!(
                    "turn:{}",
                    event.get("turn").and_then(Value::as_u64).unwrap_or(0)
                )
            });
            let mut reference = ref_for(
                scope,
                &ref_id,
                "prompt_measurement",
                "host_observation",
                "resident",
            );
            reference.payload_ref = Some(payload_ref);
            reference.provenance = event
                .get("measurement")
                .and_then(|m| m.get("source"))
                .and_then(|s| s.get("kind"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            refs.put(reference);
        }
        "llm_completed" => {
            if let Some(wire) = event.get("wire_evidence") {
                let ref_id = string_field(event, "invocation_id")
                    .or_else(|| string_field(event, "effect_id"))
                    .unwrap_or_else(|| {
                        format!(
                            "turn:{}",
                            event.get("turn").and_then(Value::as_u64).unwrap_or(0)
                        )
                    });
                let mut reference = ref_for(
                    scope,
                    &ref_id,
                    "wire_replay",
                    "host_observation",
                    "resident",
                );
                reference.payload_ref = Some(payload_ref);
                reference.version = string_field(wire, "response_id");
                refs.put(reference);
            }
        }
        "context_prepared" => {
            if let Some(effect_id) = string_field(event, "effect_id") {
                let mut reference = ref_for(
                    scope,
                    &effect_id,
                    "context_binding",
                    "derived_projection",
                    "resident",
                );
                reference.payload_ref = Some(payload_ref.clone());
                reference.content_digest = event
                    .get("preparation")
                    .and_then(|p| p.get("binding"))
                    .and_then(|b| b.get("digest"))
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                refs.put(reference);
            }
            if let Some(entries) = event
                .get("preparation")
                .and_then(|p| p.get("state"))
                .and_then(|s| s.get("knowledge"))
                .and_then(Value::as_array)
            {
                for entry in entries {
                    let (Some(entry_id), Some(content_digest)) = (
                        string_field(entry, "entry_id"),
                        string_field(entry, "content_digest"),
                    ) else {
                        continue;
                    };
                    let identity = knowledge_identity(&entry_id, &content_digest);
                    knowledge.insert(identity.ref_id.clone(), (entry_id, content_digest.clone()));
                    let mut reference = ref_for(
                        scope,
                        &identity.ref_id,
                        "knowledge_entry",
                        "derived_projection",
                        "resident",
                    );
                    reference.content_digest = Some(content_digest);
                    reference.payload_ref = Some(payload_ref.clone());
                    reference.provenance = identity.origin;
                    reference.links = identity.links;
                    refs.put(reference);
                }
            }
        }
        "memory_written" => {
            let record_id = string_field(event, "record_id").unwrap_or_default();
            if !memory_in_scope(event, scope) {
                diagnostic(
                    diagnostics,
                    "memory_scope_mismatch",
                    "memory event belongs to another tenant or namespace and was excluded",
                    Some("memory_record"),
                    Some(&record_id),
                );
                return;
            }
            let mut reference = ref_for(
                scope,
                &record_id,
                "memory_record",
                "host_observation",
                "external",
            );
            reference.payload_ref = Some(payload_ref);
            reference.size_bytes = event.get("size_bytes").and_then(Value::as_u64);
            reference.settlement = Some("accepted".into());
            refs.put(reference);
        }
        "memory_retrieval_result" => {
            if let Some(hits) = event.get("hits").and_then(Value::as_array) {
                for hit in hits {
                    let Some(record) = hit.get("record") else {
                        continue;
                    };
                    let record_id = string_field(record, "record_id").unwrap_or_default();
                    if !memory_in_scope(record, scope) {
                        diagnostic(
                            diagnostics,
                            "memory_scope_mismatch",
                            "memory event belongs to another tenant or namespace and was excluded",
                            Some("memory_record"),
                            Some(&record_id),
                        );
                        continue;
                    }
                    let mut reference = ref_for(
                        scope,
                        &record_id,
                        "memory_record",
                        "host_observation",
                        "external",
                    );
                    reference.payload_ref = Some(payload_ref.clone());
                    reference.provenance = record.get("provenance").map(|p| {
                        format!(
                            "{}/{}",
                            string_field(p, "author").unwrap_or_default(),
                            string_field(p, "trust").unwrap_or_default()
                        )
                    });
                    reference.settlement = Some("accepted".into());
                    if let Some(provenance) = record.get("provenance") {
                        if let Some(values) =
                            provenance.get("evidence_refs").and_then(Value::as_array)
                        {
                            if !values.is_empty() {
                                reference.evidence_refs = Some(
                                    values
                                        .iter()
                                        .filter_map(Value::as_str)
                                        .map(str::to_owned)
                                        .collect(),
                                );
                            }
                        }
                    }
                    reference.retention = Some(EvidenceRetention {
                        pinned: record.get("pinned").and_then(Value::as_bool),
                        observed_ttl_days: record.get("ttl_days").and_then(Value::as_u64),
                        prunable: None,
                    });
                    refs.put(reference);
                }
            }
        }
        "memory_candidate_pending" | "memory_candidate_rejected" => {
            let Some(candidate_id) = string_field(event, "candidate_id") else {
                return;
            };
            let mut reference = ref_for(
                scope,
                &candidate_id,
                "memory_candidate",
                "host_observation",
                "external",
            );
            reference.payload_ref = Some(payload_ref);
            reference.provenance = string_field(event, "source");
            reference.settlement = Some(
                if kind.ends_with("pending") {
                    "pending"
                } else {
                    "rejected"
                }
                .into(),
            );
            reference.version = string_field(event, "receipt_id");
            refs.put(reference);
        }
        _ => {}
    }
}

fn refine_candidates(
    sidecar: Option<&Value>,
    scope: &EvidenceScope,
    refs: &mut ObservedRefs,
    diagnostics: &mut Vec<EvidenceDiagnostic>,
) {
    let Some(receipts) = sidecar
        .and_then(|v| v.get("memory_candidate_receipts"))
        .and_then(Value::as_object)
    else {
        return;
    };
    let Some(agent_id) = scope.agent_id.as_deref() else {
        diagnostic(
            diagnostics,
            "memory_candidate_agent_unscoped",
            "candidate receipts are keyed by agentId, which the evidence scope does not carry",
            Some("memory_candidate"),
            None,
        );
        return;
    };
    let candidates: Vec<(String, EvidenceRef)> = refs
        .values
        .values()
        .filter(|r| r.domain == "memory_candidate")
        .map(|r| (r.ref_id.clone(), r.clone()))
        .collect();
    for (candidate_id, candidate) in candidates {
        let Some(receipt) = receipts.get(&candidate_id) else {
            continue;
        };
        let _ = agent_id;
        let receipt_candidate = string_field(receipt, "candidate_id");
        let status = string_field(receipt, "status");
        let receipt_id = string_field(receipt, "receipt_id");
        let conflict = if receipt_candidate.as_deref() != Some(candidate_id.as_str()) {
            Some(format!(
                "sink receipt names candidate {}",
                receipt_candidate.unwrap_or_default()
            ))
        } else if candidate.settlement.as_deref() == Some("rejected")
            && status.as_deref() != Some("rejected")
        {
            Some(format!(
                "sink receipt ({}) disagrees with the logged candidate (rejected)",
                status.clone().unwrap_or_default()
            ))
        } else if status == candidate.settlement
            && candidate.version.is_some()
            && receipt_id != candidate.version
        {
            Some(format!(
                "sink receipt {} renames the logged {} answer {}",
                receipt_id.clone().unwrap_or_default(),
                status.clone().unwrap_or_default(),
                candidate.version.clone().unwrap_or_default()
            ))
        } else {
            None
        };
        if let Some(message) = conflict {
            diagnostic(
                diagnostics,
                "memory_candidate_receipt_conflict",
                message,
                Some("memory_candidate"),
                Some(&candidate_id),
            );
            continue;
        }
        let mut fields = candidate;
        fields.settlement = status;
        fields.version = receipt_id;
        if fields.settlement.as_deref() == Some("accepted") {
            fields.content_digest = string_field(receipt, "digest");
            fields.payload_ref = string_field(receipt, "record_ref");
            if let Some(record_ref) = fields.payload_ref.clone() {
                if refs.has("memory_record", &record_ref) {
                    fields.links = Some(vec![EvidenceLink {
                        relation: "settled_as".into(),
                        domain: "memory_record".into(),
                        ref_id: record_ref,
                    }]);
                }
            }
        }
        refs.put(fields);
    }
}

/// Read the existing Rust storage capabilities into the shared evidence read model.
///
/// Session events are serialized through their registered wire representation, while journal bytes
/// remain opaque and are copied verbatim. This function only invokes read methods; it never appends
/// to the session log or journal. `sidecar` is an optional read-only JSON projection used for the
/// host-owned provenance and candidate-receipt lookups.
pub async fn read_session_evidence(
    operation_id: &str,
    session_id: &str,
    scope: &Value,
    session_log: &dyn SessionLog,
    kernel_journal: Option<&dyn KernelJournal>,
    sidecar: Option<&Value>,
) -> Result<SharedEvidenceSnapshot, std::io::Error> {
    let entries = session_log.read(session_id, 0, None).await?;
    let events = entries
        .into_iter()
        .map(|entry| {
            serde_json::to_value(entry.event)
                .map(|event| (entry.seq, event))
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let journal = match kernel_journal {
        Some(journal) => {
            let entries = journal
                .read_from(operation_id, 0)
                .await
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            let checkpoint = journal
                .latest_checkpoint(operation_id)
                .await
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            Some(json!({
                "entries": entries.iter().map(|entry| json!({
                    "step_seq": entry.step_seq,
                    "record_digest": entry.record_digest,
                    "record_bytes": entry.record_bytes,
                })).collect::<Vec<_>>(),
                "checkpoint": checkpoint.map(|checkpoint| json!({
                    "checkpoint_id": checkpoint.checkpoint_id,
                    "ordinal": checkpoint.ordinal,
                    "through_step_seq": checkpoint.through_step_seq,
                    "state_digest": checkpoint.state_digest,
                    "checkpoint_bytes": checkpoint.checkpoint_bytes,
                })),
            }))
        }
        None => None,
    };
    Ok(read_shared_evidence_values(
        operation_id,
        session_id,
        scope,
        &events,
        journal.as_ref(),
        sidecar,
    ))
}

pub fn read_shared_evidence_values(
    operation_id: &str,
    session_id: &str,
    scope: &Value,
    events: &[(u64, Value)],
    journal: Option<&Value>,
    sidecar: Option<&Value>,
) -> SharedEvidenceSnapshot {
    let evidence_scope = EvidenceScope {
        operation_id: operation_id.into(),
        session_id: Some(session_id.into()),
        agent_id: string_field(scope, "agent_id"),
        tenant_id: string_field(scope, "tenant_id"),
        namespace: string_field(scope, "namespace"),
    };
    let mut refs = ObservedRefs::default();
    let mut diagnostics = Vec::new();
    let mut knowledge = BTreeMap::new();
    let mut session_logs = Vec::new();
    for (seq, event) in events {
        let bytes = canonical_event_json(event).into_bytes();
        session_logs.push(bytes);
        observe_event(
            *seq,
            event,
            &evidence_scope,
            &mut refs,
            &mut knowledge,
            &mut diagnostics,
        );
    }
    let mut verification_journal = Vec::new();
    let mut verification_checkpoints = Vec::new();
    let mut journal_step_seq = None;
    let mut checkpoint_ordinal = None;
    if let Some(journal) = journal {
        if let Some(entries) = journal.get("entries").and_then(Value::as_array) {
            for entry in entries {
                if let Some(step) = entry.get("step_seq").and_then(Value::as_u64) {
                    journal_step_seq = Some(step);
                    let mut reference = ref_for(
                        &evidence_scope,
                        &step.to_string(),
                        "journal_record",
                        "kernel_fact",
                        "resident",
                    );
                    reference.content_digest = string_field(entry, "record_digest");
                    if let Some(bytes) = entry.get("record_bytes").and_then(Value::as_array) {
                        let raw = bytes
                            .iter()
                            .filter_map(Value::as_u64)
                            .map(|v| v as u8)
                            .collect::<Vec<_>>();
                        reference.size_bytes = Some(raw.len() as u64);
                        verification_journal.push(raw);
                    }
                    refs.put(reference);
                }
            }
        }
        if let Some(checkpoint) = journal.get("checkpoint") {
            checkpoint_ordinal = checkpoint.get("ordinal").and_then(Value::as_u64);
            if let Some(bytes) = checkpoint.get("checkpoint_bytes").and_then(Value::as_array) {
                let raw = bytes
                    .iter()
                    .filter_map(Value::as_u64)
                    .map(|v| v as u8)
                    .collect::<Vec<_>>();
                verification_checkpoints.push(raw.clone());
                let mut reference = ref_for(
                    &evidence_scope,
                    checkpoint
                        .get("checkpoint_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    "checkpoint",
                    "kernel_fact",
                    "resident",
                );
                reference.content_digest = string_field(checkpoint, "state_digest");
                reference.size_bytes = Some(raw.len() as u64);
                reference.version = checkpoint_ordinal.map(|v| v.to_string());
                refs.put(reference);
            }
        }
        let first = journal
            .get("entries")
            .and_then(Value::as_array)
            .and_then(|e| e.first())
            .and_then(|v| v.get("step_seq"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let covered = journal
            .get("checkpoint")
            .and_then(|c| c.get("through_step_seq"))
            .and_then(Value::as_u64);
        if first > 0 && covered.map_or(true, |value| value < first - 1) {
            diagnostic(
                &mut diagnostics,
                "journal_prefix_pruned",
                format!(
                    "journal records before step {first} are pruned and no checkpoint covers them"
                ),
                Some("journal_record"),
                None,
            );
        }
    } else {
        diagnostic(
            &mut diagnostics,
            "journal_not_provided",
            "no KernelJournal was supplied",
            None,
            None,
        );
    }
    if let Some(provenance) = sidecar
        .and_then(|v| v.get("knowledge_provenance"))
        .and_then(Value::as_object)
    {
        for (ref_id, (entry_id, content_digest)) in &knowledge {
            if let Some(value) = provenance.get(entry_id) {
                if let Some(reference) = refs
                    .values
                    .get_mut(&ObservedRefs::key("knowledge_entry", ref_id))
                {
                    reference.provenance = value.as_str().map(str::to_owned);
                }
            } else {
                let _ = content_digest;
            }
        }
    }
    refine_candidates(sidecar, &evidence_scope, &mut refs, &mut diagnostics);
    let mut ref_values = refs.values.into_values().collect::<Vec<_>>();
    ref_values.sort_by(|left, right| {
        format!("{}\0{}", left.domain, left.ref_id)
            .cmp(&format!("{}\0{}", right.domain, right.ref_id))
    });
    let mut completeness = if ref_values.is_empty() && verification_journal.is_empty() {
        "unavailable"
    } else {
        "complete"
    }
    .to_string();
    let present: BTreeSet<String> = ref_values
        .iter()
        .map(|r| ObservedRefs::key(&r.domain, &r.ref_id))
        .collect();
    for reference in &ref_values {
        if let Some(links) = &reference.links {
            for link in links {
                if !present.contains(&ObservedRefs::key(&link.domain, &link.ref_id)) {
                    completeness = "partial".into();
                    diagnostic(
                        &mut diagnostics,
                        "evidence_link_unresolved",
                        format!(
                            "{} target {}/{} is not in the snapshot",
                            link.relation, link.domain, link.ref_id
                        ),
                        Some(&reference.domain),
                        Some(&reference.ref_id),
                    );
                }
            }
        }
        if reference.domain == "knowledge_entry" && reference.provenance.is_none() {
            completeness = "partial".into();
            diagnostic(
                &mut diagnostics,
                "knowledge_provenance_missing",
                "knowledge reference has no provenance from its sidecar",
                Some("knowledge_entry"),
                Some(&reference.ref_id),
            );
        }
        if reference.availability == "missing" {
            completeness = "partial".into();
        }
    }
    for item in &diagnostics {
        if item.code != "evidence_empty" {
            completeness = "partial".into();
        }
    }
    SharedEvidenceSnapshot {
        operation_id: operation_id.into(),
        scope: evidence_scope,
        as_of: Some(EvidenceCursor {
            session_log_seq: events.last().map(|(seq, _)| *seq as i64),
            journal_step_seq,
            checkpoint_ordinal,
        }),
        completeness,
        diagnostics,
        refs: ref_values,
        verification_journal,
        verification_session_logs: if session_logs.is_empty() {
            Vec::new()
        } else {
            vec![session_logs]
        },
        verification_checkpoints,
    }
}

pub fn project_shared_evidence(snapshot: &SharedEvidenceSnapshot) -> Value {
    let defined = |mut object: Map<String, Value>| {
        object.retain(|_, value| !value.is_null());
        Value::Object(object)
    };
    let as_of = snapshot
        .as_of
        .as_ref()
        .map(|cursor| {
            defined(Map::from_iter([
                (
                    "session_log_seq".into(),
                    cursor.session_log_seq.map_or(Value::Null, |v| json!(v)),
                ),
                (
                    "journal_step_seq".into(),
                    cursor.journal_step_seq.map_or(Value::Null, |v| json!(v)),
                ),
                (
                    "checkpoint_ordinal".into(),
                    cursor.checkpoint_ordinal.map_or(Value::Null, |v| json!(v)),
                ),
            ]))
        })
        .unwrap_or_else(|| json!({}));
    json!({ "as_of": as_of, "completeness": snapshot.completeness, "diagnostics": snapshot.diagnostics.iter().map(|d| { let mut o = Map::new(); o.insert("code".into(), json!(d.code)); if let Some(domain) = &d.domain { o.insert("domain".into(), json!(domain)); } if let Some(ref_id) = &d.ref_id { o.insert("ref_id".into(), json!(ref_id)); } Value::Object(o) }).collect::<Vec<_>>(), "refs": snapshot.refs.iter().map(|r| { let mut o = Map::new(); o.insert("domain".into(), json!(r.domain)); o.insert("ref_id".into(), json!(r.ref_id)); o.insert("authority".into(), json!(r.authority)); o.insert("availability".into(), json!(r.availability)); if let Some(v)=&r.content_digest {o.insert("content_digest".into(),json!(v));} if let Some(v)=&r.payload_ref {o.insert("payload_ref".into(),json!(v));} if let Some(v)=r.size_bytes {o.insert("size_bytes".into(),json!(v));} if let Some(v)=&r.version {o.insert("version".into(),json!(v));} if let Some(v)=&r.provenance {o.insert("provenance".into(),json!(v));} if let Some(v)=&r.settlement {o.insert("settlement".into(),json!(v));} if let Some(v)=&r.evidence_refs {o.insert("evidence_refs".into(),json!(v));} if let Some(v)=&r.links {o.insert("links".into(),json!(v.iter().map(|l|json!({"relation":l.relation,"domain":l.domain,"ref_id":l.ref_id})).collect::<Vec<_>>()));} if let Some(v)=&r.retention { let mut retention = Map::new(); if let Some(value) = v.pinned { retention.insert("pinned".into(), json!(value)); } if let Some(value) = v.observed_ttl_days { retention.insert("observed_ttl_days".into(), json!(value)); } if let Some(value) = v.prunable { retention.insert("prunable".into(), json!(value)); } o.insert("retention".into(), Value::Object(retention)); } defined(o) }).collect::<Vec<_>>(), "verification": { "journal": snapshot.verification_journal.len(), "session_logs": snapshot.verification_session_logs.iter().map(Vec::len).collect::<Vec<_>>(), "checkpoints": snapshot.verification_checkpoints.len() } })
}
