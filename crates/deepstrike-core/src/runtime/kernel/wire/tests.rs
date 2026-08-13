//! Canonical Kernel ABI — Phase 1 · Task 3 contract tests.
//!
//! These tests fix the wire rules that every later task builds on:
//! decimal `WireU64`, fixed-point policy ratios, finite observation floats, explicit
//! canonical-bytes projection, strict tagged unions, unknown-field/variant rejection,
//! revision fail-closed, and the five-class input taxonomy with `RootEntry` as the only
//! root entry shape.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::*;

// ---------------------------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------------------------

fn limits() -> KernelBootstrapLimits {
    KernelBootstrapLimits::default()
}

fn decode(json_text: &str) -> Result<WireEnvelope, WireRejection> {
    decode_envelope_json(json_text, &limits())
}

fn decode_kind(json_text: &str) -> WireRejectionKind {
    decode(json_text).expect_err("input must be rejected").kind
}

fn sample_envelope(input: KernelInput) -> WireEnvelope {
    WireEnvelope::new(
        OperationId::new("op-1").unwrap(),
        InputId::new("in-1").unwrap(),
        WireU64::new(1_700_000_000_000),
        input,
    )
}

fn envelope_json(input: Value) -> String {
    serde_json::to_string(&json!({
        "abi_version": KERNEL_ABI_VERSION,
        "operation_id": "op-1",
        "input_id": "in-1",
        "observed_at_ms": "1700000000000",
        "input": input,
    }))
    .unwrap()
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/kernel-wire")
}

fn fixtures_with_prefix(prefix: &str) -> Vec<(String, Value)> {
    let dir = fixture_dir();
    let mut names: Vec<String> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", dir.display()))
        .map(|entry| {
            entry
                .expect("dir entry")
                .file_name()
                .to_string_lossy()
                .to_string()
        })
        .filter(|name| name.ends_with(".json") && name.starts_with(prefix))
        .collect();
    names.sort();
    assert!(
        !names.is_empty(),
        "no {prefix}*.json fixtures in {}",
        dir.display()
    );
    names
        .into_iter()
        .map(|name| {
            let raw = fs::read_to_string(dir.join(&name))
                .unwrap_or_else(|e| panic!("failed to read {name}: {e}"));
            let value: Value =
                serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{name} is not JSON: {e}"));
            (name, value)
        })
        .collect()
}

/// Every key that appears anywhere in `value`, recursively.
fn all_keys(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                out.insert(key.clone());
                all_keys(child, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| all_keys(item, out)),
        _ => {}
    }
}

// ---------------------------------------------------------------------------------------------
// revision
// ---------------------------------------------------------------------------------------------

#[test]
fn kernel_abi_revision_is_three_and_checkpoint_revision_is_two() {
    assert_eq!(KERNEL_ABI_VERSION, 3);
    assert_eq!(KERNEL_CHECKPOINT_VERSION, 2);
}

#[test]
fn wrong_or_missing_revision_is_rejected_before_decoding_the_body() {
    // a v2 payload: old field name `version`, old event shape — the revision probe fires first,
    // so the host sees a revision fault instead of a deserialization error about the body.
    let v2 = r#"{"version":2,"operation_id":"op-1","event_id":"e-1","observed_at_ms":7,
                 "event":{"kind":"start_run","task":{"goal":"g","criteria":[]}}}"#;
    assert_eq!(decode_kind(v2), WireRejectionKind::VersionMismatch);

    let missing = r#"{"operation_id":"op-1","input_id":"in-1","observed_at_ms":"1","input":{"kind":"force_compact"}}"#;
    assert_eq!(decode_kind(missing), WireRejectionKind::VersionMismatch);

    let future = r#"{"abi_version":4,"operation_id":"op-1","input_id":"in-1","observed_at_ms":"1",
                     "input":{"kind":"host_control","command":{"kind":"force_compact"}}}"#;
    assert_eq!(decode_kind(future), WireRejectionKind::VersionMismatch);
}

#[test]
fn revision_is_fail_closed_even_without_the_probe() {
    // The typed envelope itself only accepts revision 3, so no decode path can bypass §16.2.
    let raw = json!({
        "abi_version": 2,
        "operation_id": "op-1",
        "input_id": "in-1",
        "observed_at_ms": "1",
        "input": { "kind": "host_control", "command": { "kind": "force_compact" } },
    });
    assert!(serde_json::from_value::<WireEnvelope>(raw).is_err());
}

// ---------------------------------------------------------------------------------------------
// scalars (§7.1.1)
// ---------------------------------------------------------------------------------------------

#[test]
fn wire_u64_travels_as_a_decimal_string_across_the_full_range() {
    for value in [
        0u64,
        1,
        (1u64 << 53) - 1, // last JS-safe integer
        1u64 << 53,       // first value a JS number cannot represent exactly
        u64::MAX,
    ] {
        let wire = WireU64::new(value);
        let text = serde_json::to_string(&wire).unwrap();
        assert_eq!(text, format!("\"{value}\""));
        let back: WireU64 = serde_json::from_str(&text).unwrap();
        assert_eq!(back.get(), value);
    }
    assert!(WireU64::new((1u64 << 53) - 1).is_js_safe());
    assert!(!WireU64::new(1u64 << 53).is_js_safe());
}

#[test]
fn wire_u64_rejects_json_numbers_and_non_canonical_decimals() {
    for raw in [
        "42",                       // JSON number
        "42.0",                     // float
        "\"\"",                     // empty
        "\"007\"",                  // leading zeros
        "\"+7\"",                   // sign
        "\" 7\"",                   // whitespace
        "\"7 \"",                   //
        "\"-7\"",                   // negative
        "\"0x10\"",                 // radix prefix
        "\"18446744073709551616\"", // u64::MAX + 1
        "null",
        "true",
    ] {
        let err = serde_json::from_str::<WireU64>(raw)
            .expect_err(&format!("{raw} must not decode as WireU64"));
        assert!(
            err.to_string().contains(SCALAR_ERROR_MARKER),
            "{raw}: unexpected error {err}"
        );
    }
}

#[test]
fn wire_u64_stays_a_decimal_string_inside_a_tagged_union() {
    // internally tagged enums re-deserialize from buffered content; the scalar rule must survive it
    let body = envelope_json(json!({
        "kind": "host_control",
        "command": { "kind": "update_deadline", "deadline_ms": 1700000000000u64 },
    }));
    assert_eq!(decode_kind(&body), WireRejectionKind::InvalidScalar);

    let ok = envelope_json(json!({
        "kind": "host_control",
        "command": { "kind": "update_deadline", "deadline_ms": "1700000000000" },
    }));
    decode(&ok).expect("decimal-string deadline decodes");
}

#[test]
fn policy_ratios_are_fixed_point_parts_per_million_not_floats() {
    let quarter = Ppm::new(250_000).unwrap();
    assert_eq!(serde_json::to_string(&quarter).unwrap(), "250000");
    assert_eq!(
        serde_json::from_str::<Ppm>("250000").unwrap().get(),
        250_000
    );
    assert_eq!(Ppm::ONE.get(), 1_000_000);

    for raw in ["0.25", "-1", "\"250000\"", "1000001", "null"] {
        assert!(
            serde_json::from_str::<Ppm>(raw).is_err(),
            "{raw} must not decode as Ppm"
        );
    }
    assert!(Ppm::new(1_000_001).is_err());
}

#[test]
fn observation_floats_must_be_finite() {
    let score = FiniteF64::new(0.5).unwrap();
    assert_eq!(serde_json::to_string(&score).unwrap(), "0.5");
    assert_eq!(serde_json::from_str::<FiniteF64>("0.5").unwrap().get(), 0.5);
    assert_eq!(serde_json::from_str::<FiniteF64>("2").unwrap().get(), 2.0);

    assert!(FiniteF64::new(f64::NAN).is_err());
    assert!(FiniteF64::new(f64::INFINITY).is_err());
    assert!(FiniteF64::new(f64::NEG_INFINITY).is_err());
    // 1e400 has no finite f64 representation — it must not sneak in as `inf`.
    assert!(serde_json::from_str::<FiniteF64>("1e400").is_err());
}

#[test]
fn canonical_bytes_project_as_explicit_base64_in_json() {
    for bytes in [
        vec![],
        vec![0x00],
        vec![0x00, 0xff],
        vec![b'a', b'b', b'c'],
        (0u8..=255).collect::<Vec<u8>>(),
    ] {
        let canonical = CanonicalBytes::new(bytes.clone());
        let text = serde_json::to_string(&canonical).unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["encoding"], json!("base64"));
        assert!(value["data"].is_string());
        let back: CanonicalBytes = serde_json::from_str(&text).unwrap();
        assert_eq!(back.as_slice(), bytes.as_slice());
    }

    // implicit forms are rejected: a bare string, a byte array, an unknown encoding tag
    assert!(serde_json::from_str::<CanonicalBytes>("\"YWJj\"").is_err());
    assert!(serde_json::from_str::<CanonicalBytes>("[97,98,99]").is_err());
    assert!(
        serde_json::from_str::<CanonicalBytes>(r#"{"encoding":"hex","data":"616263"}"#).is_err()
    );
    assert!(
        serde_json::from_str::<CanonicalBytes>(r#"{"encoding":"base64","data":"YWJj","extra":1}"#)
            .is_err()
    );
    // non-canonical base64 (missing padding / illegal alphabet)
    assert!(
        serde_json::from_str::<CanonicalBytes>(r#"{"encoding":"base64","data":"YWJ"}"#).is_err()
    );
    assert!(
        serde_json::from_str::<CanonicalBytes>(r#"{"encoding":"base64","data":"YW J="}"#).is_err()
    );
}

#[test]
fn identities_are_branded_non_empty_strings() {
    assert_eq!(
        serde_json::to_string(&OperationId::new("op-1").unwrap()).unwrap(),
        "\"op-1\""
    );
    assert!(OperationId::new("").is_err());
    assert!(InputId::new("").is_err());
    assert!(TaskId::new("a\u{0007}b").is_err());
    assert!(EffectId::new("x".repeat(MAX_ID_BYTES + 1)).is_err());
    assert!(serde_json::from_str::<OperationId>("\"\"").is_err());
    assert!(serde_json::from_str::<OperationId>("7").is_err());
}

#[test]
fn empty_identities_are_rejected_at_the_envelope_boundary() {
    let raw = json!({
        "abi_version": KERNEL_ABI_VERSION,
        "operation_id": "",
        "input_id": "in-1",
        "observed_at_ms": "1",
        "input": { "kind": "host_control", "command": { "kind": "force_compact" } },
    });
    assert_eq!(
        decode_kind(&serde_json::to_string(&raw).unwrap()),
        WireRejectionKind::InvalidScalar
    );
}

// ---------------------------------------------------------------------------------------------
// absolute boundaries (§7.1 / §7.3)
// ---------------------------------------------------------------------------------------------

#[test]
fn absolute_byte_boundary_runs_before_json_parsing() {
    let tight = KernelBootstrapLimits {
        absolute_max_input_bytes: 32,
        ..KernelBootstrapLimits::default()
    };
    // deliberately not valid JSON: the byte bound must fire first
    let oversized = "{".repeat(64);
    let rejection = decode_envelope_json(&oversized, &tight).expect_err("byte bound fires");
    assert_eq!(rejection.kind, WireRejectionKind::InputTooLarge);
}

#[test]
fn absolute_depth_boundary_runs_before_json_parsing() {
    let shallow = KernelBootstrapLimits {
        absolute_max_json_depth: 4,
        ..KernelBootstrapLimits::default()
    };
    let deep = format!("{}1{}", "[".repeat(16), "]".repeat(16));
    assert_eq!(
        decode_envelope_json(&deep, &shallow)
            .expect_err("depth bound fires")
            .kind,
        WireRejectionKind::DepthExceeded
    );
}

#[test]
fn bounded_json_accepts_tool_schema_depth_within_the_absolute_envelope_limit() {
    let mut schema = json!({ "type": "string" });
    for _ in 0..24 {
        schema = json!({
            "type": "object",
            "properties": { "nested": schema },
        });
    }

    BoundedJson::new(schema).expect("a finite nested tool schema remains a legal wire scalar");
}

#[test]
fn absolute_collection_boundary_runs_before_json_parsing() {
    let narrow = KernelBootstrapLimits {
        absolute_max_collection_entries: 8,
        ..KernelBootstrapLimits::default()
    };
    let wide = format!("[{}]", vec!["1"; 32].join(","));
    assert_eq!(
        decode_envelope_json(&wide, &narrow)
            .expect_err("collection bound fires")
            .kind,
        WireRejectionKind::CollectionTooLarge
    );
}

// ---------------------------------------------------------------------------------------------
// taxonomy (§7.2)
// ---------------------------------------------------------------------------------------------

#[test]
fn the_taxonomy_has_exactly_five_classes_each_with_its_own_validation_path() {
    let classes = [
        KernelInput::ConfigureOperation(ConfigureOperation {
            config: OperationConfig::default(),
        }),
        KernelInput::StartOperation(StartOperation {
            entry: RootEntry::Workflow(RootWorkflowEntry {
                spec: WorkflowSpec::default(),
            }),
            initial_context: InitialContext::default(),
        }),
        KernelInput::ResolveEffect(ResolveEffect {
            effect_id: EffectId::new("op-1:step:1:effect:0").unwrap(),
            outcome: EffectOutcome::Succeeded(EffectSucceeded {
                result: EffectSuccess::Approval(ApprovalSuccess::default()),
            }),
        }),
        KernelInput::DeliverExternalEvent(DeliverExternalEvent {
            event: ExternalEvent::DeliverSignal(DeliverSignal {
                delivery_id: DeliveryId::new("d-1").unwrap(),
                attempt: 1,
                signal: LogicalSignal::new(SignalId::new("s-1").unwrap()),
            }),
        }),
        KernelInput::HostControl(HostControl {
            command: HostCommand::ForceCompact(ForceCompactCommand {}),
        }),
    ];

    let authorities: BTreeSet<InputAuthority> =
        classes.iter().map(KernelInput::authority).collect();
    assert_eq!(
        authorities.len(),
        5,
        "each input class must enter its own validation path"
    );

    let tags: BTreeSet<String> = classes
        .iter()
        .map(|input| {
            serde_json::to_value(input).unwrap()["kind"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(
        tags,
        BTreeSet::from([
            "configure_operation".to_string(),
            "start_operation".to_string(),
            "resolve_effect".to_string(),
            "deliver_external_event".to_string(),
            "host_control".to_string(),
        ])
    );

    // No class is admissible after a terminal (§6.1.9) and every class declares a closed set.
    for input in &classes {
        let admissible = input.admissible_lifecycles();
        assert!(!admissible.is_empty());
        assert!(
            admissible.iter().all(|state| !state.is_terminal()),
            "no state-changing input is admissible after terminal"
        );
    }
    assert_eq!(
        classes[0].admissible_lifecycles(),
        &[OperationLifecycle::Created]
    );
    assert_eq!(
        classes[1].admissible_lifecycles(),
        &[OperationLifecycle::Configured]
    );
}

#[test]
fn unknown_input_variant_is_rejected() {
    let body = envelope_json(json!({ "kind": "resume" }));
    assert_eq!(decode_kind(&body), WireRejectionKind::UnknownVariant);

    // the deleted P1-as-wire-input escape hatch is not a sixth class
    let syscall = envelope_json(json!({
        "kind": "syscall_request",
        "request": { "kind": "submit_workflow", "spec": { "name": "w", "nodes": [] } },
    }));
    assert_eq!(decode_kind(&syscall), WireRejectionKind::UnknownVariant);
}

#[test]
fn unknown_fields_are_rejected_at_every_nesting_level() {
    // envelope level
    let mut envelope: Value = serde_json::from_str(&envelope_json(json!({
        "kind": "host_control",
        "command": { "kind": "force_compact" },
    })))
    .unwrap();
    envelope["session_id"] = json!("sess-1");
    assert_eq!(
        decode_kind(&serde_json::to_string(&envelope).unwrap()),
        WireRejectionKind::UnknownField
    );

    // input-variant level
    let variant = envelope_json(json!({
        "kind": "host_control",
        "command": { "kind": "force_compact" },
        "operation_id": "op-1",
    }));
    assert_eq!(decode_kind(&variant), WireRejectionKind::UnknownField);

    // nested payload level (host command)
    let nested = envelope_json(json!({
        "kind": "host_control",
        "command": { "kind": "cancel", "reason": "user", "pending_call_ids": [], "operation_id": "op-1" },
    }));
    assert_eq!(decode_kind(&nested), WireRejectionKind::UnknownField);

    // deeply nested payload level (root entry → agent spec)
    let deep = envelope_json(json!({
        "kind": "start_operation",
        "entry": {
            "kind": "agent",
            "task": { "goal": "g" },
            "run_spec": { "goal": "g", "session_id": "sess-1" },
        },
        "initial_context": {},
    }));
    assert_eq!(decode_kind(&deep), WireRejectionKind::UnknownField);
}

// ---------------------------------------------------------------------------------------------
// root entry (§7.4)
// ---------------------------------------------------------------------------------------------

#[test]
fn root_entry_is_the_only_root_start_shape() {
    let agent = envelope_json(json!({
        "kind": "start_operation",
        "entry": { "kind": "agent", "task": { "goal": "write the brief" } },
        "initial_context": {},
    }));
    let decoded = decode(&agent).expect("agent root decodes");
    match decoded.input {
        KernelInput::StartOperation(start) => {
            assert_eq!(start.entry.root_kind(), RootKind::Agent);
        }
        other => panic!("unexpected input: {other:?}"),
    }

    let workflow = envelope_json(json!({
        "kind": "start_operation",
        "entry": { "kind": "workflow", "spec": { "name": "pipeline", "nodes": [] } },
        "initial_context": {},
    }));
    let decoded = decode(&workflow).expect("workflow root decodes");
    match decoded.input {
        KernelInput::StartOperation(start) => {
            assert_eq!(start.entry.root_kind(), RootKind::Workflow);
        }
        other => panic!("unexpected input: {other:?}"),
    }

    // the deleted alternative root shapes are not reachable
    for tag in ["run", "resume", "load_workflow", "spawn_sub_agent"] {
        let body = envelope_json(json!({
            "kind": "start_operation",
            "entry": { "kind": tag },
            "initial_context": {},
        }));
        assert_eq!(
            decode_kind(&body),
            WireRejectionKind::UnknownVariant,
            "{tag} must not be a root entry"
        );
    }
}

#[test]
fn initial_context_lives_only_on_start_operation() {
    for entry in [
        json!({ "kind": "agent", "task": { "goal": "g" }, "initial_context": {} }),
        json!({ "kind": "workflow", "spec": { "name": "w", "nodes": [] }, "initial_context": {} }),
    ] {
        let body = envelope_json(json!({
            "kind": "start_operation",
            "entry": entry,
            "initial_context": {},
        }));
        assert_eq!(
            decode_kind(&body),
            WireRejectionKind::UnknownField,
            "initial_context must not be duplicated inside a root entry variant"
        );
    }

    // …and it is required on StartOperation itself
    let missing = envelope_json(json!({
        "kind": "start_operation",
        "entry": { "kind": "agent", "task": { "goal": "g" } },
    }));
    assert_eq!(decode_kind(&missing), WireRejectionKind::MissingField);
}

#[test]
fn logical_agent_spec_carries_no_host_session_identity() {
    let spec = LogicalAgentSpec::new("write the brief");
    let value = serde_json::to_value(&spec).unwrap();
    let mut keys = BTreeSet::new();
    all_keys(&value, &mut keys);
    for banned in [
        "session_id",
        "parent_session_id",
        "agent_id",
        "identity",
        "memory_path",
        "path",
    ] {
        assert!(
            !keys.contains(banned),
            "LogicalAgentSpec leaks host identity field {banned}"
        );
    }

    for banned in [
        json!({ "goal": "g", "identity": { "agent_id": "a", "session_id": "s" } }),
        json!({ "goal": "g", "parent_session_id": "s" }),
    ] {
        assert!(
            serde_json::from_value::<LogicalAgentSpec>(banned).is_err(),
            "host session identity must not decode into LogicalAgentSpec"
        );
    }
}

#[test]
fn execution_focus_projects_its_root_kind_and_round_trips() {
    let agent_turn = ExecutionFocus::agent_turn(TaskId::new("task-root").unwrap());
    assert_eq!(agent_turn.root_kind_hint(), RootKind::Agent);

    let controller = ExecutionFocus::workflow_controller(
        WorkflowId::new("wf-1").unwrap(),
        Some(TaskId::new("task-root").unwrap()),
    );
    assert_eq!(controller.root_kind_hint(), RootKind::Workflow);
    assert!(controller.is_nested_in_agent());
    assert!(
        !ExecutionFocus::workflow_controller(WorkflowId::new("wf-1").unwrap(), None)
            .is_nested_in_agent()
    );

    for focus in [agent_turn, controller] {
        let text = serde_json::to_string(&focus).unwrap();
        assert_eq!(
            serde_json::from_str::<ExecutionFocus>(&text).unwrap(),
            focus
        );
    }
    // there is no host-selected focus, and no undeclared field can ride along with one
    assert!(serde_json::from_str::<ExecutionFocus>(r#"{"kind":"host_selected"}"#).is_err());
    assert!(
        serde_json::from_str::<ExecutionFocus>(
            r#"{"kind":"agent_turn","task_id":"t-1","session_id":"s-1"}"#
        )
        .is_err()
    );
}

// ---------------------------------------------------------------------------------------------
// host command / syscall / external event
// ---------------------------------------------------------------------------------------------

#[test]
fn seed_knowledge_is_a_host_command_and_page_in_stays_a_syscall() {
    // DEC-9: the two page-in senses must never share a name.
    let seed = envelope_json(json!({
        "kind": "host_control",
        "command": {
            "kind": "seed_knowledge",
            "entries": [{ "content": "style guide", "key": "style" }],
        },
    }));
    decode(&seed).expect("seed_knowledge is a host command");

    let host_page_in = envelope_json(json!({
        "kind": "host_control",
        "command": { "kind": "page_in", "entries": [] },
    }));
    assert_eq!(
        decode_kind(&host_page_in),
        WireRejectionKind::UnknownVariant
    );

    let syscall: SyscallRequest = serde_json::from_value(json!({
        "kind": "page_in",
        "handle_id": "h-1",
    }))
    .expect("PageIn is a P1 syscall over a handle");
    assert!(matches!(syscall, SyscallRequest::PageIn(_)));
    assert!(
        serde_json::from_value::<SyscallRequest>(json!({
            "kind": "seed_knowledge",
            "entries": [],
        }))
        .is_err()
    );
}

#[test]
fn child_completion_carries_the_attempt_and_its_parent_requests() {
    let body = envelope_json(json!({
        "kind": "deliver_external_event",
        "event": {
            "kind": "child_completed",
            "task_id": "task-7",
            "attempt_id": "task-7:attempt:1",
            "result": { "status": "completed", "output": "done" },
            "parent_requests": [
                { "kind": "activate_skill", "name": "research" },
                { "kind": "update_task", "update": { "progress": "half" } }
            ],
        },
    }));
    let decoded = decode(&body).expect("child completion decodes");
    match decoded.input {
        KernelInput::DeliverExternalEvent(DeliverExternalEvent {
            event: ExternalEvent::ChildCompleted(child),
        }) => {
            assert_eq!(child.attempt_id.as_str(), "task-7:attempt:1");
            assert_eq!(child.parent_requests.len(), 2);
        }
        other => panic!("unexpected input: {other:?}"),
    }
}

#[test]
fn signal_target_is_the_operation_or_one_logical_task() {
    for target in [
        json!({ "kind": "operation" }),
        json!({ "kind": "task", "task_id": "task-3" }),
    ] {
        serde_json::from_value::<SignalTarget>(target).expect("closed signal target");
    }
    assert!(
        serde_json::from_value::<SignalTarget>(json!({ "kind": "session", "session_id": "s" }))
            .is_err()
    );
    // a host session id cannot ride along inside a legal target either
    assert!(
        serde_json::from_value::<SignalTarget>(
            json!({ "kind": "task", "task_id": "task-3", "session_id": "s" })
        )
        .is_err()
    );
}

#[test]
fn payload_less_variants_stay_strict() {
    // an empty payload struct is not an excuse to accept extra fields
    let body = envelope_json(json!({
        "kind": "host_control",
        "command": { "kind": "force_compact", "operation_id": "op-1" },
    }));
    assert_eq!(decode_kind(&body), WireRejectionKind::UnknownField);
    assert!(
        serde_json::from_value::<SignalTarget>(json!({ "kind": "operation", "task_id": "t-1" }))
            .is_err()
    );
}

#[test]
fn policy_patches_require_a_revision_and_a_closed_patch_union() {
    let ok = envelope_json(json!({
        "kind": "host_control",
        "command": {
            "kind": "apply_policy_patch",
            "expected_revision": "4",
            "patch": { "kind": "replace_signal_policy", "policy": { "queue_max": 16 } },
        },
    }));
    decode(&ok).expect("policy patch decodes");

    let no_revision = envelope_json(json!({
        "kind": "host_control",
        "command": {
            "kind": "apply_policy_patch",
            "patch": { "kind": "replace_signal_policy", "policy": { "queue_max": 16 } },
        },
    }));
    assert_eq!(decode_kind(&no_revision), WireRejectionKind::MissingField);

    let open_patch = envelope_json(json!({
        "kind": "host_control",
        "command": {
            "kind": "apply_policy_patch",
            "expected_revision": "4",
            "patch": { "kind": "replace_context_policy", "policy": {} },
        },
    }));
    assert_eq!(decode_kind(&open_patch), WireRejectionKind::UnknownVariant);

    // §16.1: the signal policy no longer carries its own version constant
    assert!(
        serde_json::from_value::<SignalPolicy>(json!({ "queue_max": 16, "version": 1 })).is_err()
    );
}

// ---------------------------------------------------------------------------------------------
// no duplicated operation / clock / session / path facts (§7.1, §7.5)
// ---------------------------------------------------------------------------------------------

#[test]
fn business_inputs_never_repeat_operation_clock_session_or_path_facts() {
    const BANNED: [&str; 11] = [
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

    for (name, fixture) in fixtures_with_prefix("input_") {
        let mut keys = BTreeSet::new();
        all_keys(&fixture["input"], &mut keys);
        for banned in BANNED {
            assert!(
                !keys.contains(banned),
                "{name}: business input repeats the envelope-owned fact {banned:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------
// golden fixtures
// ---------------------------------------------------------------------------------------------

#[test]
fn canonical_goldens_round_trip_through_the_typed_envelope() {
    let fixtures = fixtures_with_prefix("input_");
    assert!(
        fixtures.len() >= 5,
        "expected at least one golden per input class, got {}",
        fixtures.len()
    );

    let mut covered: BTreeSet<String> = BTreeSet::new();
    for (name, fixture) in fixtures {
        let text = serde_json::to_string(&fixture).unwrap();
        let envelope = decode(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
        let reencoded = serde_json::to_value(&envelope).unwrap();
        assert_eq!(
            reencoded, fixture,
            "{name}: canonical round-trip changed the document"
        );

        // re-decoding our own output is stable
        let again = decode(&serde_json::to_string(&reencoded).unwrap())
            .unwrap_or_else(|e| panic!("{name} (second pass): {e}"));
        assert_eq!(serde_json::to_value(&again).unwrap(), fixture);

        covered.insert(fixture["input"]["kind"].as_str().unwrap().to_string());
    }

    assert_eq!(
        covered,
        BTreeSet::from([
            "configure_operation".to_string(),
            "start_operation".to_string(),
            "resolve_effect".to_string(),
            "deliver_external_event".to_string(),
            "host_control".to_string(),
        ]),
        "every input class needs at least one canonical golden"
    );
}

#[test]
fn rejection_fixtures_fail_closed_with_the_declared_kind() {
    // Two families are deliberately excluded, and for the same reason: they are not envelopes and
    // never cross this decode boundary.
    //
    // * `reject_checkpoint_*` are checkpoint blobs, carrying the §12 taxonomy
    //   (`checkpoint_incompatible` / `checkpoint_corrupted`); exercised by
    //   `checkpoint::tests::checkpoint_rejection_fixtures_fail_closed_with_the_declared_kind`.
    // * `reject_transaction_*` are §7.13 *faults* from a well-formed envelope the transaction
    //   refused — `checkpoint_required` above all, which is a decision about the journal's tail and
    //   not about the bytes; exercised by the §12.3 tests in `driver`.
    let fixtures: Vec<_> = fixtures_with_prefix("reject_")
        .into_iter()
        .filter(|(name, _)| {
            !name.starts_with("reject_checkpoint_") && !name.starts_with("reject_transaction_")
        })
        .collect();
    assert!(fixtures.len() >= 8, "too few rejection fixtures");

    let mut kinds: BTreeSet<String> = BTreeSet::new();
    for (name, fixture) in fixtures {
        let expected = fixture["expect"]
            .as_str()
            .unwrap_or_else(|| panic!("{name}: missing `expect`"));
        let envelope_text = serde_json::to_string(&fixture["envelope"]).unwrap();
        let rejection = decode(&envelope_text)
            .map(|ok| panic!("{name}: expected rejection, decoded {ok:?}"))
            .unwrap_err();
        assert_eq!(
            rejection.kind.as_str(),
            expected,
            "{name}: wrong rejection kind ({})",
            rejection.message
        );
        kinds.insert(expected.to_string());
    }

    for required in ["unknown_field", "unknown_variant", "version_mismatch"] {
        assert!(
            kinds.contains(required),
            "rejection fixtures must cover {required}"
        );
    }
}

#[test]
fn every_input_class_round_trips_from_rust_values() {
    let inputs = [
        KernelInput::ConfigureOperation(ConfigureOperation {
            config: OperationConfig {
                // Task 5: the knowledge budget lives in `context_policy`, not at the config root
                context_policy: Some(ContextPolicy {
                    knowledge_budget_ppm: Some(Ppm::new(250_000).unwrap()),
                    ..ContextPolicy::default()
                }),
                kernel_limits: Some(KernelLimits {
                    max_input_bytes: Some(4096),
                    max_json_depth: Some(16),
                    max_collection_entries: Some(256),
                    collection_limits: None,
                }),
                host_effect_support: HostEffectSupport::new([EffectKindTag::CallProvider]),
                ..OperationConfig::default()
            },
        }),
        KernelInput::StartOperation(StartOperation {
            entry: RootEntry::Agent(RootAgentEntry {
                task: LogicalTask::new("write the brief"),
                run_spec: Some(LogicalAgentSpec::new("write the brief")),
            }),
            initial_context: InitialContext::default(),
        }),
        KernelInput::ResolveEffect(ResolveEffect {
            effect_id: EffectId::new("op-1:step:1:effect:0").unwrap(),
            outcome: EffectOutcome::Failed(EffectFailed {
                failure: HostEffectFailure {
                    kind: HostEffectFailureKind::ProtocolError,
                    message: "unsupported effect".to_string(),
                    retryable: None,
                },
            }),
        }),
        KernelInput::DeliverExternalEvent(DeliverExternalEvent {
            event: ExternalEvent::ChildCompleted(ChildCompleted {
                task_id: TaskId::new("task-7").unwrap(),
                attempt_id: AttemptId::new("task-7:attempt:1").unwrap(),
                result: ChildResult::default(),
                parent_requests: vec![SyscallRequest::ActivateSkill(ActivateSkillRequest {
                    name: "research".to_string(),
                    lease_turns: Some(3),
                })],
            }),
        }),
        KernelInput::HostControl(HostControl {
            command: HostCommand::Cancel(CancelCommand {
                reason: CancellationReason::User,
                pending_call_ids: vec![CallId::new("call-1").unwrap()],
            }),
        }),
    ];

    for input in inputs {
        let envelope = sample_envelope(input);
        let text = serde_json::to_string(&envelope).unwrap();
        let back = decode(&text).unwrap_or_else(|e| panic!("{text}: {e}"));
        assert_eq!(back, envelope);
    }
}
