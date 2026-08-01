#![cfg(test)]
//! Canonical Kernel ABI — version-neutral semantic fixtures (Phase 0 · Task 2).
//!
//! These fixtures freeze *semantics*, not wire bytes. Canonical wire goldens live under
//! `tests/fixtures/kernel-wire`; each fixture here states one scenario in
//! version-neutral vocabulary; every historical anchor (v1/v2 wire name, `file:line`, test name)
//! is confined to `evidence.sources`.
//!
//! This module is the gate for that contract:
//!   1. every fixture parses and validates against `schema.json`;
//!   2. narrative fields carry no version-specific vocabulary (denylist lint);
//!   3. ids are unique and agree with their filenames;
//!   4. each of the seven positive domains has at least three fixtures;
//!   5. every fixture cites at least one real anchor.

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

const DOMAINS: [&str; 8] = [
    "agent", "workflow", "signal", "budget", "cancel", "fault", "resume", "removed",
];

const POSITIVE_DOMAINS: [&str; 7] = [
    "agent", "workflow", "signal", "budget", "cancel", "fault", "resume",
];

const MIN_POSITIVE_PER_DOMAIN: usize = 3;

/// The frozen Phase 0 · Task 2 inventory: (domain, positive fixture count).
/// Changing the fixture set is a deliberate act — update this table with it.
const FROZEN_POSITIVE_INVENTORY: [(&str, usize); 7] = [
    ("agent", 5),
    ("workflow", 5),
    ("signal", 4),
    ("budget", 4),
    ("cancel", 4),
    ("fault", 7),
    ("resume", 5),
];

/// Fields whose prose must stay version-neutral. `evidence` is deliberately excluded: it is the
/// only place historical wire names, file:line anchors and test names may appear.
const NARRATIVE_FIELDS: [&str; 5] = ["title", "semantics", "given", "when", "then"];

/// Version-specific vocabulary that must not leak into the narrative fields.
///
/// Sourced from the wire-tag / symbol columns of
/// `.local-docs/.local_spc/canonical-kernel-abi-capability-inventory.md` (§1 input events,
/// §2 effects, §3 observations, §5 struct fields, §6 version constants) plus the durable
/// transition entry points. Matched case-insensitively as substrings, so every entry is an
/// identifier-shaped token (snake_case wire tag, CamelCase type, or host symbol) that cannot
/// collide with ordinary prose.
const VOCABULARY_DENYLIST: [&str; 99] = [
    // --- top-level wire tags (input events)
    "start_run",
    "complete_run",
    "configure_run",
    "load_workflow",
    "submit_workflow",
    "submit_workflow_nodes",
    "spawn_sub_agent",
    "sub_agent_completed",
    "deliver_signal",
    "cancel_operation",
    "provider_result",
    "provider_error",
    "tool_results",
    "approval_result",
    "workflow_spawn_result",
    "preempt_result",
    "memory_persist_result",
    "memory_query_result",
    "milestone_result",
    "large_result_spool_result",
    "page_out_archive_result",
    "write_memory",
    "query_memory",
    "update_task",
    "force_compact",
    "page_in",
    "skill_activated",
    "skill_deactivated",
    "mount_capability",
    "unmount_capability",
    "capability_command",
    "add_system_message",
    "add_knowledge_message",
    "add_history_message",
    "preload_history",
    "remove_knowledge",
    "load_governance_policy",
    "load_milestone_contract",
    "set_tokenizer",
    "set_tools",
    "set_available_skills",
    "set_stable_core_tools",
    "set_memory_enabled",
    "set_knowledge_enabled",
    "set_plan_tool_enabled",
    "set_memory_policy",
    "set_signal_policy",
    "set_resource_quota",
    "set_scheduler_budget",
    "set_criteria_gate",
    "set_repeat_fuse",
    "set_entropy_watch",
    "set_knowledge_budget",
    // --- effect / observation tags
    "call_provider",
    "execute_tool",
    "request_approval",
    "spawn_workflow",
    "preempt_sub_agents",
    "persist_memory",
    "spool_large_result",
    "archive_page_out",
    "evaluate_milestone",
    "large_result_spooled",
    "signals_pending",
    "read_result",
    // --- struct fields / host-leaked identity
    "operation_id",
    "event_id",
    "input_id",
    "step_seq",
    "effect_id",
    "call_id",
    "now_ms",
    "observed_at_ms",
    "actor_id",
    "agent_id",
    "session_id",
    "parent_session_id",
    "submitter_agent_id",
    "pending_call_ids",
    "resumed_",
    "spool",
    "snapshot_version",
    "abi_version",
    "last_step",
    "accepted_inputs",
    "memory_path",
    // --- core types & durable entry points
    "kernelinput",
    "kernelstep",
    "kernelaction",
    "kernelobservation",
    "kerneleffect",
    "kernelsnapshot",
    "kernelfault",
    "loopstatemachine",
    "prepare_step",
    "commit_prepared",
    "abort_prepared",
    "restore_snapshot",
    "step_json",
];

fn fixture_dir() -> PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(manifest_dir).join("../fixtures/kernel-contract")
}

/// (file name, parsed fixture), sorted by file name. `schema.json` is excluded.
fn load_fixtures() -> Vec<(String, Value)> {
    let dir = fixture_dir();
    let mut entries: Vec<String> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", dir.display(), e))
        .map(|e| {
            e.expect("dir entry")
                .file_name()
                .to_string_lossy()
                .to_string()
        })
        .filter(|name| name.ends_with(".json") && name != "schema.json")
        .collect();
    entries.sort();

    assert!(
        !entries.is_empty(),
        "no fixtures found in {}",
        dir.display()
    );

    entries
        .into_iter()
        .map(|name| {
            let raw = fs::read_to_string(dir.join(&name))
                .unwrap_or_else(|e| panic!("failed to read fixture {}: {}", name, e));
            let value: Value = serde_json::from_str(&raw)
                .unwrap_or_else(|e| panic!("fixture {} is not valid JSON: {}", name, e));
            (name, value)
        })
        .collect()
}

fn load_schema() -> Value {
    let path = fixture_dir().join("schema.json");
    let raw = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {}", path.display(), e));
    serde_json::from_str(&raw).expect("schema.json is valid JSON")
}

// --------------------------------------------------------------------------------------------
// Minimal JSON Schema (draft 2020-12) validator.
//
// The suite has no `jsonschema` dependency (see tests/rust/Cargo.toml) and this task must not add
// one, so this covers exactly the keyword subset used by `schema.json`. Any keyword outside that
// subset panics loudly rather than being silently ignored, so a future schema change cannot
// quietly weaken the gate.
// --------------------------------------------------------------------------------------------

const SUPPORTED_KEYWORDS: [&str; 15] = [
    "$schema",
    "$id",
    "title",
    "description",
    "type",
    "enum",
    "required",
    "properties",
    "additionalProperties",
    "items",
    "minItems",
    "minLength",
    "pattern",
    "maxItems",
    "maxLength",
];

fn matches_pattern(pattern: &str, value: &str) -> bool {
    match pattern {
        // kebab-case identifier
        "^[a-z0-9]+(-[a-z0-9]+)*$" => {
            !value.is_empty()
                && !value.starts_with('-')
                && !value.ends_with('-')
                && !value.contains("--")
                && value
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        }
        // spec section reference, e.g. §6.1
        r"^§[0-9]+(\.[0-9]+)*$" => {
            let Some(rest) = value.strip_prefix('§') else {
                return false;
            };
            !rest.is_empty()
                && rest
                    .split('.')
                    .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
        }
        other => panic!(
            "schema.json uses pattern {:?} which this validator does not implement; \
             extend matches_pattern() before landing the schema change",
            other
        ),
    }
}

fn json_type_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) => {
            if n.is_i64() || n.is_u64() {
                "integer"
            } else {
                "number"
            }
        }
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn validate(value: &Value, schema: &Value, path: &str, errors: &mut Vec<String>) {
    let obj = schema.as_object().expect("schema node is an object");

    for key in obj.keys() {
        assert!(
            SUPPORTED_KEYWORDS.contains(&key.as_str()),
            "schema.json uses unsupported keyword {:?} at {}; extend the validator first",
            key,
            path
        );
    }

    if let Some(expected) = obj.get("type").and_then(Value::as_str) {
        let actual = json_type_of(value);
        let ok = expected == actual || (expected == "number" && actual == "integer");
        if !ok {
            errors.push(format!(
                "{}: expected type {}, got {}",
                path, expected, actual
            ));
            return;
        }
    }

    if let Some(allowed) = obj.get("enum").and_then(Value::as_array) {
        if !allowed.contains(value) {
            errors.push(format!("{}: {} is not one of {:?}", path, value, allowed));
        }
    }

    if let Some(text) = value.as_str() {
        if let Some(min) = obj.get("minLength").and_then(Value::as_u64) {
            if (text.chars().count() as u64) < min {
                errors.push(format!("{}: shorter than minLength {}", path, min));
            }
        }
        if let Some(max) = obj.get("maxLength").and_then(Value::as_u64) {
            if (text.chars().count() as u64) > max {
                errors.push(format!("{}: longer than maxLength {}", path, max));
            }
        }
        if let Some(pattern) = obj.get("pattern").and_then(Value::as_str) {
            if !matches_pattern(pattern, text) {
                errors.push(format!("{}: {:?} does not match {}", path, text, pattern));
            }
        }
    }

    if let Some(items) = value.as_array() {
        if let Some(min) = obj.get("minItems").and_then(Value::as_u64) {
            if (items.len() as u64) < min {
                errors.push(format!(
                    "{}: has {} items, minItems is {}",
                    path,
                    items.len(),
                    min
                ));
            }
        }
        if let Some(max) = obj.get("maxItems").and_then(Value::as_u64) {
            if (items.len() as u64) > max {
                errors.push(format!(
                    "{}: has {} items, maxItems is {}",
                    path,
                    items.len(),
                    max
                ));
            }
        }
        if let Some(item_schema) = obj.get("items") {
            for (index, item) in items.iter().enumerate() {
                validate(item, item_schema, &format!("{}[{}]", path, index), errors);
            }
        }
    }

    if let Some(map) = value.as_object() {
        if let Some(required) = obj.get("required").and_then(Value::as_array) {
            for field in required {
                let name = field.as_str().expect("required entry is a string");
                if !map.contains_key(name) {
                    errors.push(format!("{}: missing required field {:?}", path, name));
                }
            }
        }
        let properties = obj.get("properties").and_then(Value::as_object);
        if obj.get("additionalProperties") == Some(&Value::Bool(false)) {
            for key in map.keys() {
                let known = properties.map(|p| p.contains_key(key)).unwrap_or(false);
                if !known {
                    errors.push(format!("{}: unexpected field {:?}", path, key));
                }
            }
        }
        if let Some(properties) = properties {
            for (key, sub_schema) in properties {
                if let Some(child) = map.get(key) {
                    validate(child, sub_schema, &format!("{}.{}", path, key), errors);
                }
            }
        }
    }
}

fn narrative_strings(fixture: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for field in NARRATIVE_FIELDS {
        match &fixture[field] {
            Value::String(s) => out.push((field.to_string(), s.clone())),
            Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    if let Some(s) = item.as_str() {
                        out.push((format!("{}[{}]", field, index), s.to_string()));
                    }
                }
            }
            _ => {}
        }
    }
    out
}

// --------------------------------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------------------------------

#[test]
fn every_fixture_validates_against_the_schema() {
    let schema = load_schema();
    let mut errors = Vec::new();

    for (name, fixture) in load_fixtures() {
        validate(&fixture, &schema, &name, &mut errors);
    }

    assert!(
        errors.is_empty(),
        "schema validation failed:\n{}",
        errors.join("\n")
    );
}

#[test]
fn fixture_ids_are_unique_and_match_their_filenames() {
    let mut seen: BTreeMap<String, String> = BTreeMap::new();

    for (name, fixture) in load_fixtures() {
        let id = fixture["id"].as_str().expect("id is a string").to_string();
        let domain = fixture["domain"].as_str().expect("domain is a string");
        let kind = fixture["kind"].as_str().expect("kind is a string");

        if let Some(previous) = seen.insert(id.clone(), name.clone()) {
            panic!("duplicate fixture id {:?} in {} and {}", id, previous, name);
        }

        assert_eq!(
            name,
            format!("{}.json", id.replace('-', "_")),
            "file name must be the underscore form of the id"
        );
        assert!(
            name.starts_with(&format!("{}_", domain)),
            "{} must be named <domain>_<slug>.json for domain {:?}",
            name,
            domain
        );
        // negative fixtures live in the `removed` domain and therefore in removed_*.json
        assert_eq!(
            kind == "negative",
            domain == "removed",
            "{}: kind {:?} and domain {:?} disagree",
            name,
            kind,
            domain
        );
    }
}

#[test]
fn narrative_fields_use_version_neutral_vocabulary() {
    let mut violations = Vec::new();

    for (name, fixture) in load_fixtures() {
        for (field, text) in narrative_strings(&fixture) {
            let haystack = text.to_lowercase();
            for banned in VOCABULARY_DENYLIST {
                if haystack.contains(banned) {
                    violations.push(format!(
                        "{} · {}: contains version-specific token {:?} \
                         (historical anchors belong in evidence.sources)",
                        name, field, banned
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "version-specific vocabulary leaked into fixture narrative:\n{}",
        violations.join("\n")
    );
}

#[test]
fn every_positive_domain_has_at_least_three_fixtures() {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();

    for (name, fixture) in load_fixtures() {
        let domain = fixture["domain"].as_str().expect("domain is a string");
        assert!(
            DOMAINS.contains(&domain),
            "{}: unknown domain {:?}",
            name,
            domain
        );
        if fixture["kind"] == Value::String("positive".into()) {
            *counts.entry(domain.to_string()).or_default() += 1;
        }
    }

    for domain in POSITIVE_DOMAINS {
        let count = counts.get(domain).copied().unwrap_or(0);
        assert!(
            count >= MIN_POSITIVE_PER_DOMAIN,
            "domain {:?} has {} positive fixtures, expected at least {}",
            domain,
            count,
            MIN_POSITIVE_PER_DOMAIN
        );
    }

    // …and the set itself is frozen, so an accidental deletion cannot hide behind the minimum.
    for (domain, expected) in FROZEN_POSITIVE_INVENTORY {
        let count = counts.get(domain).copied().unwrap_or(0);
        assert_eq!(
            count, expected,
            "domain {:?} has {} positive fixtures, frozen inventory says {}; \
             update FROZEN_POSITIVE_INVENTORY together with the fixture set",
            domain, count, expected
        );
    }
}

#[test]
fn removed_behaviours_are_covered_by_negative_fixtures() {
    let negatives: Vec<String> = load_fixtures()
        .into_iter()
        .filter(|(_, fixture)| fixture["kind"] == Value::String("negative".into()))
        .map(|(name, _)| name)
        .collect();

    assert!(
        negatives.len() >= 10,
        "expected at least 10 negative fixtures for the deleted behaviours, got {}: {:?}",
        negatives.len(),
        negatives
    );
}

#[test]
fn every_fixture_cites_at_least_one_anchor() {
    let mut errors = Vec::new();

    for (name, fixture) in load_fixtures() {
        let sources = fixture["evidence"]["sources"]
            .as_array()
            .expect("evidence.sources is an array");
        assert!(!sources.is_empty(), "{}: evidence.sources is empty", name);

        // an anchor is a spec section, a Rust/TS/Python symbol path, or a file:line reference
        let anchored = sources.iter().any(|source| {
            let text = source.as_str().unwrap_or_default();
            text.contains('§')
                || text.contains("::")
                || text.contains(".rs:")
                || text.contains(".ts:")
                || text.contains(".py:")
        });
        if !anchored {
            errors.push(format!(
                "{}: no real anchor in evidence.sources ({:?})",
                name, sources
            ));
        }
    }

    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

#[test]
fn fixture_targets_point_at_known_spec_sections() {
    // Sanity net: the target must be a section of the Canonical Kernel ABI spec, i.e. §1..§25.
    let mut sections: BTreeSet<String> = BTreeSet::new();

    for (name, fixture) in load_fixtures() {
        let target = fixture["target"].as_str().expect("target is a string");
        let top = target
            .trim_start_matches('§')
            .split('.')
            .next()
            .unwrap_or_default()
            .parse::<u32>()
            .unwrap_or_else(|_| panic!("{}: malformed target {:?}", name, target));
        assert!(
            (1..=25).contains(&top),
            "{}: target {:?} is outside the spec's §1..§25",
            name,
            target
        );
        sections.insert(target.to_string());
    }

    assert!(
        sections.len() >= 15,
        "fixtures only reference {} distinct spec sections; coverage looks too narrow",
        sections.len()
    );
}
