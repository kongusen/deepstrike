//! P7-S1 / F16: this suite consumes the ENTIRE tests/fixtures/kernel-wire directory via
//! read_dir — a fixture that is neither executed nor charter-exempted below fails the sweep
//! test. The charter lists the only fixtures not executed here, each with the surface that
//! does enforce it. An empty charter is the default; every entry needs a reason.
//!
//! Rust is the typed SDK: where the string bindings assert marker substrings in rejection
//! messages, this sweep asserts the exact [`WireRejectionKind`] — the strongest pin of the four.

use std::collections::BTreeSet;
use std::path::PathBuf;

use deepstrike_core::runtime::kernel::wire::{
    KernelBootstrapLimits, KernelFaultCode, OperationLifecycle, WireEnvelope, WireRejection,
    decode_envelope_json,
};
use deepstrike_sdk::runtime::canonical_kernel::{CanonicalKernel, CanonicalPreparation};
use serde_json::{Value, json};

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tests/fixtures/kernel-wire")
}

fn fixture_names() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(fixture_dir())
        .expect("fixture directory")
        .map(|entry| {
            entry
                .expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.ends_with(".json"))
        .collect();
    names.sort();
    names
}

fn read_fixture(name: &str) -> Value {
    let bytes = std::fs::read_to_string(fixture_dir().join(name)).expect("fixture bytes");
    serde_json::from_str(&bytes).expect("fixture json")
}

fn by_prefix<'a>(names: &'a [String], prefix: &str) -> Vec<&'a str> {
    names
        .iter()
        .map(String::as_str)
        .filter(|name| name.starts_with(prefix))
        .collect()
}

/// Decode one envelope value exactly the way the wire mandates — measure bytes → absolute
/// structural boundary → decode (§7.1) — under the default bootstrap limits every binding
/// constructs with.
fn decode_envelope(value: &Value) -> Result<WireEnvelope, WireRejection> {
    decode_envelope_json(
        &serde_json::to_string(value).expect("envelope serializes"),
        &KernelBootstrapLimits::DEFAULT,
    )
}

/// Envelope-owned facts a business input must never repeat (mirror of core tests.rs).
const BANNED_INPUT_KEYS: [&str; 11] = [
    "operation_id",
    "event_id",
    "now_ms",
    "observed_at_ms",
    "session_id",
    "parent_session_id",
    "agent_id",
    "memory_path",
    "path",
    "file_path",
    "spool_dir",
];

/// Host-owned facts a configuration fixture must never carry (mirror of core config.rs).
const BANNED_CONFIG_KEYS: [&str; 7] = [
    "memory_path",
    "spool_dir",
    "tokenizer",
    "host_effect_retry_attempts",
    "session_id",
    "api_key",
    "endpoint",
];

fn all_keys<'a>(value: &'a Value, out: &mut BTreeSet<&'a str>) {
    match value {
        Value::Array(items) => items.iter().for_each(|item| all_keys(item, out)),
        Value::Object(map) => {
            for (key, child) in map {
                out.insert(key.as_str());
                all_keys(child, out);
            }
        }
        _ => {}
    }
}

fn expected_lifecycle_for(fixture: &Value) -> OperationLifecycle {
    let links = fixture["links"].as_array().expect("links");
    let disposition = &links.last().expect("non-empty links")["step"]["disposition"];
    if disposition["kind"] != "terminal" {
        return OperationLifecycle::Running;
    }
    match disposition["terminal"]["kind"]
        .as_str()
        .expect("terminal kind")
    {
        "cancelled" => OperationLifecycle::Cancelled,
        "failed" => OperationLifecycle::Failed,
        _ => OperationLifecycle::Completed,
    }
}

/// Charter: fixtures NOT executed here, each with the enforcing surface.
/// Families: prefix entries (ending in `_`); individual files: exact names without `.json`.
const CHARTER: [(&str, &str); 7] = [
    (
        "reject_checkpoint_",
        "checkpoint blobs carry the §12 taxonomy, not the envelope decode boundary; enforced by \
         core checkpoint::tests and by the SDK restore rejection coverage",
    ),
    (
        "reject_transaction_",
        "§7.13 faults from a well-formed envelope the transaction refuses (checkpoint_required \
         needs a full journal tail); enforced by core driver §12.3 tests",
    ),
    (
        "golden_checkpoint_",
        "checkpoint candidate/rebase/restore snapshots are produced and pinned by core \
         driver/tests.rs (J1: canonical bytes are core-owned); the SDK restore path is covered by \
         canonical_runner_runtime.rs restore tests",
    ),
    (
        "golden_record_canonical_bytes",
        "canonical byte vectors are core-owned (J1); asserted in core record.rs \
         golden_canonical_bytes_vectors",
    ),
    (
        "golden_record_transition",
        "its step is a synthetic record.rs helper step and a resolve_effect envelope is not \
         drivable standalone (no pending effect exists by design); pinned by core record.rs \
         golden_transition_record. The normalisation/chain-linkage surface is covered by the \
         genesis/chain drives below",
    ),
    (
        "golden_terminal_",
        "bare terminal wire shapes are owned by core terminal.rs; the SDK asserts terminals \
         end-to-end via kernel.terminal() deep-equality in the lifecycle drive below",
    ),
    (
        "golden_config_reject_limits_widen_bootstrap",
        "fixture requires fixture-supplied bootstrap limits; SDK bindings construct with \
         defaults. Enforced by core config.rs with the fixture's limits",
    ),
];

fn charter_covers(name: &str) -> bool {
    let base = name.strip_suffix(".json").unwrap_or(name);
    CHARTER.iter().any(|(key, _)| {
        if key.ends_with('_') {
            name.starts_with(key)
        } else {
            base == *key
        }
    })
}

/// Envelope reject fixtures, minus the two charter families.
fn envelope_rejects<'a>(names: &'a [String]) -> Vec<&'a str> {
    by_prefix(names, "reject_")
        .into_iter()
        .filter(|name| {
            !name.starts_with("reject_checkpoint_") && !name.starts_with("reject_transaction_")
        })
        .collect()
}

/// Config reject fixtures that a default-limits binding can execute.
fn config_rejects_default_limits<'a>(names: &'a [String]) -> Vec<&'a str> {
    by_prefix(names, "golden_config_reject_")
        .into_iter()
        .filter(|name| read_fixture(name).get("bootstrap_limits").is_none())
        .collect()
}

// ---------------------------------------------------------------------------------------------
// the sweep itself: nothing in the directory may go unclassified
// ---------------------------------------------------------------------------------------------

#[test]
fn every_fixture_is_executed_or_charter_exempted() {
    let names = fixture_names();

    let mut executed: BTreeSet<&str> = BTreeSet::new();
    for prefix in [
        "golden_lifecycle_",
        "golden_record_chain",
        "golden_record_genesis",
        "golden_config_resolved",
        "input_",
    ] {
        executed.extend(by_prefix(&names, prefix));
    }
    executed.extend(config_rejects_default_limits(&names));
    executed.extend(envelope_rejects(&names));

    let unclassified: Vec<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|name| !executed.contains(name) && !charter_covers(name))
        .collect();
    assert!(
        unclassified.is_empty(),
        "unclassified fixtures: {unclassified:?}"
    );

    // every charter entry must still match something — a stale charter entry is drift
    let stale: Vec<&str> = CHARTER
        .iter()
        .map(|(key, _)| *key)
        .filter(|key| {
            !names.iter().any(|name| {
                if key.ends_with('_') {
                    name.starts_with(key)
                } else {
                    name.strip_suffix(".json") == Some(key)
                }
            })
        })
        .collect();
    assert!(stale.is_empty(), "stale charter entries: {stale:?}");

    // the charter must stay narrow: bootstrap_limits-carrying config rejects are the only
    // data-driven exemptions allowed beyond the list above
    let default_limits: BTreeSet<&str> =
        config_rejects_default_limits(&names).into_iter().collect();
    for name in by_prefix(&names, "golden_config_reject_") {
        if !default_limits.contains(name) {
            let base = name.strip_suffix(".json").expect("json suffix");
            assert!(
                CHARTER.iter().any(|(key, _)| *key == base),
                "{name}: unchartered bootstrap-limits exemption"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// golden lifecycles: drive every link, pin digests, records, terminal
// ---------------------------------------------------------------------------------------------

#[test]
fn lifecycle_fixtures_drive_to_declared_digests() {
    for name in by_prefix(&fixture_names(), "golden_lifecycle_") {
        let fixture = read_fixture(name);
        let mut kernel = CanonicalKernel::default();
        let links = fixture["links"].as_array().expect("links");

        for (index, link) in links.iter().enumerate() {
            assert!(
                link["envelope"].get("abi_version").is_none(),
                "{name} link {index}: abi_version leaked"
            );
            let envelope = decode_envelope(&link["envelope"]).unwrap_or_else(|rejection| {
                panic!("{name} link {index}: decode rejected: {rejection}")
            });
            let CanonicalPreparation::Prepared(prepared) = kernel.prepare(&envelope) else {
                panic!("{name} link {index}: expected prepared");
            };
            assert_eq!(
                serde_json::to_value(&prepared.planned_step).expect("step json"),
                link["step"],
                "{name} link {index}: planned step",
            );
            // J1 pass-through: the real kernel reproduces the pinned record byte for byte
            assert_eq!(
                std::str::from_utf8(prepared.record.record_bytes().as_slice())
                    .expect("utf8 record"),
                serde_json::to_string(&link["record"]).expect("record json"),
                "{name} link {index}: record bytes",
            );
            let committed = kernel
                .commit(&prepared.token, prepared.record.record_digest())
                .unwrap_or_else(|fault| panic!("{name} link {index}: commit faulted: {fault:?}"));
            assert_eq!(
                committed.step_seq.get(),
                index as u64,
                "{name} link {index}: step_seq"
            );
            assert_eq!(
                committed.record.record_digest().as_str(),
                link["record"]["record_digest"]
                    .as_str()
                    .expect("pinned digest"),
                "{name} link {index}: committed digest",
            );
        }

        assert_eq!(
            kernel.lifecycle(),
            expected_lifecycle_for(&fixture),
            "{name}: lifecycle"
        );

        let last = &links.last().expect("links")["step"]["disposition"];
        if last["kind"] == "terminal" {
            let terminal = kernel
                .terminal()
                .unwrap_or_else(|| panic!("{name}: terminal expected"));
            assert_eq!(
                serde_json::to_value(terminal).expect("terminal json"),
                last["terminal"],
                "{name}: terminal"
            );
        } else {
            assert!(kernel.terminal().is_none(), "{name}: no terminal expected");
        }
    }
}

// ---------------------------------------------------------------------------------------------
// record goldens: normalisation + chain linkage through the real kernel
// ---------------------------------------------------------------------------------------------

/// The fixture records pin synthetic record.rs helper steps, so step_digest/record_digest (and
/// the previous_record_digest chain built on them) are core-unit pins, not kernel-drive pins.
/// What a full-directory drive can and must reproduce: the normalised input (canonical_input +
/// input_digest) and the envelope identity fields; hash-chain linkage is asserted separately
/// against the real digests the kernel produces.
fn assert_record_sans_chain_digests(produced: &Value, pinned: &Value, context: &str) {
    let strip = |record: &Value| -> Value {
        let mut value = record.clone();
        let object = value.as_object_mut().expect("record object");
        object.remove("record_digest");
        object.remove("step_digest");
        object.remove("previous_record_digest");
        value
    };
    assert_eq!(
        strip(produced),
        strip(pinned),
        "{context}: normalised record"
    );
    assert!(
        produced["record_digest"].is_string(),
        "{context}: record_digest present"
    );
    assert!(
        produced["step_digest"].is_string(),
        "{context}: step_digest present"
    );
}

#[test]
fn record_chain_fixtures_pin_normalisation_and_linkage() {
    for name in by_prefix(&fixture_names(), "golden_record_chain") {
        let fixture = read_fixture(name);
        let mut kernel = CanonicalKernel::default();
        let links = fixture["links"].as_array().expect("links");
        assert!(!links.is_empty(), "{name}: empty chain");

        let mut previous_digest = Value::Null;
        for (index, link) in links.iter().enumerate() {
            let context = format!("{name} link {index}");
            let envelope = decode_envelope(&link["envelope"])
                .unwrap_or_else(|rejection| panic!("{context}: decode rejected: {rejection}"));
            let CanonicalPreparation::Prepared(prepared) = kernel.prepare(&envelope) else {
                panic!("{context}: expected prepared");
            };
            let produced: Value = serde_json::from_slice(prepared.record.record_bytes().as_slice())
                .expect("record json");
            assert_record_sans_chain_digests(&produced, &link["record"], &context);
            assert_eq!(
                produced["previous_record_digest"], previous_digest,
                "{context}: chain linkage"
            );
            let committed = kernel
                .commit(&prepared.token, prepared.record.record_digest())
                .unwrap_or_else(|fault| panic!("{context}: commit faulted: {fault:?}"));
            assert_eq!(
                committed.record.record_digest().as_str(),
                produced["record_digest"].as_str().expect("digest"),
                "{context}: committed digest"
            );
            previous_digest = produced["record_digest"].clone();
        }
    }
}

#[test]
fn record_genesis_fixtures_drive_as_fresh_genesis() {
    for name in by_prefix(&fixture_names(), "golden_record_genesis") {
        let fixture = read_fixture(name);
        let mut kernel = CanonicalKernel::default();
        let envelope = decode_envelope(&fixture["envelope"])
            .unwrap_or_else(|rejection| panic!("{name}: decode rejected: {rejection}"));
        let CanonicalPreparation::Prepared(prepared) = kernel.prepare(&envelope) else {
            panic!("{name}: expected prepared");
        };
        let produced: Value =
            serde_json::from_slice(prepared.record.record_bytes().as_slice()).expect("record json");
        assert_record_sans_chain_digests(&produced, &fixture["record"], name);
        assert_eq!(
            produced["previous_record_digest"],
            Value::Null,
            "{name}: genesis has no predecessor"
        );
        let committed = kernel
            .commit(&prepared.token, prepared.record.record_digest())
            .unwrap_or_else(|fault| panic!("{name}: commit faulted: {fault:?}"));
        assert_eq!(
            committed.record.record_digest().as_str(),
            produced["record_digest"].as_str().expect("digest"),
            "{name}: committed digest"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// config goldens
// ---------------------------------------------------------------------------------------------

fn configure_envelope(name: &str, config: &Value) -> Value {
    json!({
        "operation_id": "op-fixture-config",
        "input_id": format!("in-{name}"),
        "observed_at_ms": "1753747200000",
        "input": {"kind": "configure_operation", "config": config},
    })
}

#[test]
fn config_resolved_fixtures_accepted_at_the_wire() {
    // the frozen ResolvedOperationConfig comparison is core-side (config.rs); the SDK asserts
    // the fixture config is accepted by a fresh kernel
    for name in by_prefix(&fixture_names(), "golden_config_resolved") {
        let fixture = read_fixture(name);
        let envelope = decode_envelope(&configure_envelope(name, &fixture["config"]))
            .unwrap_or_else(|rejection| panic!("{name}: decode rejected: {rejection}"));
        let mut kernel = CanonicalKernel::default();
        let CanonicalPreparation::Prepared(_) = kernel.prepare(&envelope) else {
            panic!("{name}: expected prepared");
        };
    }
}

#[test]
fn config_reject_fixtures_fail_with_resolution_stage_fault() {
    for name in by_prefix(&fixture_names(), "golden_config_reject_") {
        let fixture = read_fixture(name);
        if fixture.get("bootstrap_limits").is_some() {
            continue; // charter: fixture-supplied limits, enforced by core config.rs
        }
        // resolution-stage rejections map to fault codes per binding.rs rejection_fault:
        // policy_violation → invalid_config; collection_too_large currently → malformed_envelope
        // (asymmetry under constitution review — both are §7.7 resolution-stage rejections)
        let expected = if fixture["expect"] == "policy_violation" {
            KernelFaultCode::InvalidConfig
        } else {
            KernelFaultCode::MalformedEnvelope
        };
        let envelope = decode_envelope(&configure_envelope(name, &fixture["config"]))
            .unwrap_or_else(|rejection| panic!("{name}: decode rejected: {rejection}"));
        let mut kernel = CanonicalKernel::default();
        let CanonicalPreparation::Rejected(rejected) = kernel.prepare(&envelope) else {
            panic!("{name}: expected rejection");
        };
        assert_eq!(
            rejected.fault.code, expected,
            "{name}: {}",
            rejected.fault.message
        );
    }
}

// ---------------------------------------------------------------------------------------------
// input goldens: wire acceptance + banned-key scan
// ---------------------------------------------------------------------------------------------

#[test]
fn input_fixtures_accepted_at_decode_boundary() {
    for name in by_prefix(&fixture_names(), "input_") {
        let fixture = read_fixture(name);
        decode_envelope(&fixture)
            .unwrap_or_else(|rejection| panic!("{name}: decode-stage rejection: {rejection}"));
    }
}

#[test]
fn no_input_fixture_repeats_envelope_owned_facts() {
    for name in by_prefix(&fixture_names(), "input_") {
        let fixture = read_fixture(name);
        let mut keys = BTreeSet::new();
        all_keys(&fixture["input"], &mut keys);
        for banned in BANNED_INPUT_KEYS {
            assert!(
                !keys.contains(banned),
                "{name}: business input repeats {banned}"
            );
        }
    }
}

#[test]
fn no_config_fixture_carries_host_owned_facts() {
    let names = fixture_names();
    let targets: Vec<&str> = by_prefix(&names, "input_configure_")
        .into_iter()
        .chain(by_prefix(&names, "golden_config_"))
        .collect();
    for name in targets {
        let fixture = read_fixture(name);
        let mut keys = BTreeSet::new();
        all_keys(&fixture, &mut keys);
        for banned in BANNED_CONFIG_KEYS {
            assert!(
                !keys.contains(banned),
                "{name}: config fixture carries {banned}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// rejection fixtures: fail closed with the exact typed kind
// ---------------------------------------------------------------------------------------------

#[test]
fn envelope_reject_fixtures_fail_closed_with_typed_kind() {
    let names = fixture_names();
    for name in envelope_rejects(&names) {
        let fixture = read_fixture(name);
        let Err(rejection) = decode_envelope(&fixture["envelope"]) else {
            panic!("{name}: envelope must not decode");
        };
        // the typed pin: the exact rejection kind, no marker substring
        assert_eq!(
            rejection.kind.as_str(),
            fixture["expect"].as_str().expect("expect kind"),
            "{name}: {rejection}"
        );
    }
}

#[test]
fn envelope_reject_fixtures_cover_required_kinds() {
    let names = fixture_names();
    let kinds: BTreeSet<String> = envelope_rejects(&names)
        .into_iter()
        .map(|name| {
            read_fixture(name)["expect"]
                .as_str()
                .expect("expect")
                .to_owned()
        })
        .collect();
    assert!(
        kinds.contains("unknown_field"),
        "unknown_field coverage missing"
    );
    assert!(
        kinds.contains("unknown_variant"),
        "unknown_variant coverage missing"
    );
}
