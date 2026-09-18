#![cfg(test)]
//! S5 · D-A mixed-batch multi-step sequence fixtures (0.2.64 execution evidence plane).
//!
//! A new fixture *kind*: a whole multi-step sequence instead of one envelope. Each fixture
//! under `tests/fixtures/mixed-batch/` pins a real provider batch shape and the publication
//! contract the kernel must honour for it, in the vocabulary of the mixed-batch constitution
//! (docs/architecture/runtime-causality.md, M1–M5):
//!
//! * `published_kinds` — the step's exact ordered publication (M1: effects XOR terminal is
//!   type-level, so the list alone pins it);
//! * `withheld_kinds` — kinds the step dispatched but must NOT co-publish (M3: a tool batch
//!   never rides the same step as a syscall effect — a two-effect step would brick a host
//!   that consumes one effect per step, and the pending copy would collide with the
//!   re-derived batch under §15.3's one-pending-per-kind rule);
//! * `rederived` — kinds published now that an earlier step withheld (M3/M4: the kind slot
//!   is free again after the syscall effect settles, so the resume rebuilds the batch from
//!   history; re-derive determinism is what makes J4/C3 legal).
//!
//! The two founding scenarios:
//!
//! 1. `syscall-host-tool-mixed-batch.json` — the 0.2.62 incident scene: one provider turn
//!    mixes a syscall (memory query) with a host tool call. The kernel publishes only the
//!    syscall effect; the execute_tools batch is withheld and re-derived on resume.
//! 2. `syscall-only-control-plane.json` — §5k: a pure control-plane batch (skill +
//!    update_plan) publishes no effect of its own, so the kernel itself continues the turn
//!    with another provider call rather than leaving the operation with nothing
//!    outstanding.
//!
//! Runner shape (locked decision Q2): this module extends the tests/rust driver
//! infrastructure — the replay assertions live here because core owns M1–M5. The node
//! projection layer consumes the *same* fixtures for the first-effect assertion
//! (node/tests/mixed-batch-projection.test.ts). No new conformance runner.
//!
//! The replay harness below is the host's durable path through the public wire API —
//! transaction prepare → journal append → commit → driver fold — the same shape as the
//! core driver's own test Runtime, built here from exported types only.
//!
//! The fixtures pin literal envelopes verbatim, including the effect ids a resolve
//! references: minting is deterministic (operation id + step seq + index), so a pinned id
//! that no longer names the pending effect fails the replay loudly. The emitter at the
//! bottom regenerates the fixtures from the typed builders when the wire format evolves;
//! run it with `DS_EMIT_MIXED_BATCH=1 cargo test -p deepstrike-tests t17` and diff.

use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use deepstrike_core::context::manager::MEMORY_TOOL_NAME;
use deepstrike_core::runtime::kernel::wire::config::{
    ConfigDefaults, ExecutionPolicy, HostEffectSupport, MemoryPolicy, OperationConfig,
    ResourceQuota, SkillMetadata as WireSkill,
};
use deepstrike_core::runtime::kernel::wire::driver::{
    CanonicalOperationDriver, PlannedStep, SYSCALL_TOOL_NAMES,
};
use deepstrike_core::runtime::kernel::wire::effect::{
    EffectKindTag, EffectOutcome, EffectSucceeded, EffectSuccess, InlineToolResult,
    MemoryAccessBinding, MemoryCapabilities, MemoryQueriedSuccess, MemoryRecall, MemoryRecordRef,
    ProviderCompleted, ProviderMessage, ProviderOutcome, ProviderSuccess, ToolCall as WireToolCall,
    ToolResult as WireToolResult, ToolResultDisposition,
    ToolResultPayload as WireToolResultPayload, ToolSchema as WireToolSchema, ToolsSuccess,
};
use deepstrike_core::runtime::kernel::wire::envelope::{
    ConfigureOperation, KernelInput, ResolveEffect, StartOperation, WireEnvelope,
};
use deepstrike_core::runtime::kernel::wire::fault::PrepareToken;
use deepstrike_core::runtime::kernel::wire::record::KernelRecord;
use deepstrike_core::runtime::kernel::wire::root::{
    InitialContext, LogicalAgentSpec, LogicalTask, MessageRole, RootAgentEntry, RootEntry,
};
use deepstrike_core::runtime::kernel::wire::scalar::{
    BoundedJson, CallId, EffectId, InputId, MemoryBindingId, OperationId, WireU64,
};
use deepstrike_core::runtime::kernel::wire::syscall::MemoryKind as SyscallMemoryKind;
use deepstrike_core::runtime::kernel::wire::transaction::{
    CommittedTransition, InMemoryRecordIndex, KernelTransaction,
};

// ---------------------------------------------------------------------------------------------
// harness: the host's durable path (prepare → append → commit → fold), public API only
// ---------------------------------------------------------------------------------------------

struct Runtime {
    tx: KernelTransaction<PlannedStep, InMemoryRecordIndex>,
    driver: CanonicalOperationDriver,
    journal: Vec<KernelRecord>,
}

impl Runtime {
    fn new() -> Self {
        Self {
            tx: KernelTransaction::new(ConfigDefaults::default(), InMemoryRecordIndex::new()),
            driver: CanonicalOperationDriver::new(),
            journal: Vec::new(),
        }
    }

    fn submit(&mut self, envelope: &WireEnvelope) -> CommittedTransition<PlannedStep> {
        let preparation = {
            let Self { tx, driver, .. } = self;
            tx.prepare(envelope, |context| driver.plan(context))
        };
        let token: PrepareToken = preparation
            .token()
            .unwrap_or_else(|| panic!("expected a prepared step, got {:?}", preparation.fault()))
            .clone();
        let head = preparation.record().unwrap().record_digest().clone();
        let committed = self.tx.commit(&token, &head).expect("commit must succeed");
        self.journal.push(committed.record.clone());
        self.driver
            .note_committed(committed.step_seq)
            .expect("the driver folds the step it planned");
        committed
    }
}

fn published_kinds(committed: &CommittedTransition<PlannedStep>) -> Vec<String> {
    committed
        .published_effects()
        .iter()
        .map(|effect| effect.tag().as_str().to_string())
        .collect()
}

// ---------------------------------------------------------------------------------------------
// scenario builders — the emitter's typed source; the fixtures pin their JSON output
// ---------------------------------------------------------------------------------------------

fn envelope(operation: &str, id: &str, observed_at_ms: u64, input: KernelInput) -> WireEnvelope {
    WireEnvelope::new(
        OperationId::new(operation).unwrap(),
        InputId::new(id).unwrap(),
        WireU64::new(observed_at_ms),
        input,
    )
}

/// The full syscall-reachable configuration: meta-tool catalog plus one host tool, memory
/// bound read+write, one declared skill, and host support for every effect the scenarios
/// publish.
fn syscall_config(operation: &str) -> WireEnvelope {
    let tool_catalog = SYSCALL_TOOL_NAMES
        .iter()
        .chain(std::iter::once(&"search"))
        .map(|name| WireToolSchema {
            name: (*name).to_string(),
            description: String::new(),
            parameters: Default::default(),
        })
        .collect();
    let config = OperationConfig {
        execution_policy: Some(ExecutionPolicy {
            max_turns: Some(12),
            ..ExecutionPolicy::default()
        }),
        host_effect_support: HostEffectSupport::new([
            EffectKindTag::CallProvider,
            EffectKindTag::ExecuteTools,
            EffectKindTag::LoadPayload,
            EffectKindTag::SpawnTasks,
            EffectKindTag::PreemptTasks,
            EffectKindTag::PersistMemory,
            EffectKindTag::QueryMemory,
        ]),
        tool_catalog,
        skill_catalog: vec![WireSkill {
            name: "debug".to_string(),
            description: "debug helper".to_string(),
            when_to_use: None,
            allowed_tools: Vec::new(),
            capability_grants: Vec::new(),
            effort: None,
            estimated_tokens: None,
        }],
        memory_access: Some(MemoryAccessBinding {
            binding_id: MemoryBindingId::new("mem-binding-1").unwrap(),
            capabilities: MemoryCapabilities {
                read: true,
                write: true,
            },
        }),
        memory_policy: Some(MemoryPolicy {
            retrieval_top_k: Some(4),
            ..MemoryPolicy::default()
        }),
        resource_quota: Some(ResourceQuota {
            max_workflow_nodes: Some(3),
            ..ResourceQuota::default()
        }),
        ..OperationConfig::default()
    };
    envelope(
        operation,
        "in-configure",
        1_700_000_000_000,
        KernelInput::ConfigureOperation(ConfigureOperation { config }),
    )
}

fn agent_start(operation: &str) -> WireEnvelope {
    let goal = "write the research brief";
    envelope(
        operation,
        "in-start",
        1_700_000_001_000,
        KernelInput::StartOperation(StartOperation {
            entry: RootEntry::Agent(RootAgentEntry {
                task: LogicalTask::new(goal),
                run_spec: Some(LogicalAgentSpec {
                    // The complete configured surface, so the tests never lean on a
                    // permissive missing-baseline fallback.
                    exposure_baseline: Some(
                        SYSCALL_TOOL_NAMES
                            .iter()
                            .chain(std::iter::once(&"search"))
                            .map(|name| (*name).to_string())
                            .collect(),
                    ),
                    ..LogicalAgentSpec::new(goal)
                }),
            }),
            initial_context: InitialContext::default(),
        }),
    )
}

fn tool_call(call_id: &str, name: &str, arguments: Value) -> WireToolCall {
    WireToolCall {
        call_id: CallId::new(call_id).unwrap(),
        name: name.to_string(),
        arguments: BoundedJson::new(arguments).unwrap(),
    }
}

fn provider_result(
    operation: &str,
    id: &str,
    observed_at_ms: u64,
    effect: &EffectId,
    calls: Vec<WireToolCall>,
) -> WireEnvelope {
    resolved(
        operation,
        id,
        observed_at_ms,
        effect,
        EffectSuccess::Provider(ProviderSuccess {
            outcome: ProviderOutcome::Completed(ProviderCompleted {
                message: ProviderMessage {
                    role: MessageRole::Assistant,
                    content: String::new(),
                    tool_calls: calls,
                    tool_call_id: None,
                    tokens: None,
                },
                observed_input_tokens: None,
                observed_output_tokens: None,
                stop_reason: None,
            }),
        }),
    )
}

fn resolved(
    operation: &str,
    id: &str,
    observed_at_ms: u64,
    effect: &EffectId,
    result: EffectSuccess,
) -> WireEnvelope {
    envelope(
        operation,
        id,
        observed_at_ms,
        KernelInput::ResolveEffect(ResolveEffect {
            effect_id: effect.clone(),
            outcome: EffectOutcome::Succeeded(EffectSucceeded { result }),
        }),
    )
}

fn memory_queried(recall_content: &str) -> EffectSuccess {
    EffectSuccess::MemoryQueried(MemoryQueriedSuccess {
        recalls: vec![MemoryRecall {
            record_ref: MemoryRecordRef::new("rec-1").unwrap(),
            name: "brief-style".to_string(),
            kind: SyscallMemoryKind::Project,
            content: recall_content.to_string(),
            score: None,
        }],
    })
}

fn tools_succeeded(call_id: &str, output: &str) -> EffectSuccess {
    EffectSuccess::Tools(ToolsSuccess {
        results: vec![WireToolResultPayload::Inline(InlineToolResult {
            call_id: CallId::new(call_id).unwrap(),
            result: WireToolResult {
                output: output.into(),
                durable_content: None,
                is_error: false,
                disposition: ToolResultDisposition::Recoverable,
                tokens: None,
            },
        })],
    })
}

// ---------------------------------------------------------------------------------------------
// fixture replay — the generic driver over the pinned literal envelopes
// ---------------------------------------------------------------------------------------------

fn fixture_dir() -> PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(manifest_dir).join("../fixtures/mixed-batch")
}

fn load_fixture(file_name: &str) -> Value {
    let path = fixture_dir().join(file_name);
    let raw = fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {}", path.display(), e));
    serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("fixture {} is not valid JSON: {}", file_name, e))
}

fn expect_string_array(value: &Value, path: &str) -> Vec<String> {
    value
        .as_array()
        .unwrap_or_else(|| panic!("{path} must be an array"))
        .iter()
        .map(|item| {
            let kind = item
                .as_str()
                .unwrap_or_else(|| panic!("{path} entries must be strings"));
            // The assertion vocabulary is exactly the EffectKindTag spelling — schema.json's
            // enums exist to pin that, and this is where the pin is enforced.
            serde_json::from_value::<EffectKindTag>(Value::String(kind.to_string()))
                .unwrap_or_else(|_| panic!("{path}: {kind:?} is not an effect kind tag"));
            kind.to_string()
        })
        .collect()
}

/// The schema.json gate, hand-rolled: the suite carries no `jsonschema` dependency (t16
/// precedent), so this asserts exactly the shape schema.json declares — closed key sets and
/// required fields — and nothing more.
fn validate_fixture_shape(file_name: &str, fixture: &Value) {
    let object = fixture
        .as_object()
        .unwrap_or_else(|| panic!("{file_name}: fixture must be an object"));
    let expected: BTreeSet<&str> = ["id", "kind", "title", "semantics", "provenance", "steps"]
        .into_iter()
        .collect();
    let actual: BTreeSet<&str> = object.keys().map(String::as_str).collect();
    assert_eq!(
        actual, expected,
        "{file_name}: top-level keys must be exactly {expected:?}"
    );
    for key in ["id", "kind", "title", "semantics"] {
        assert!(
            fixture[key].as_str().is_some_and(|s| !s.is_empty()),
            "{file_name}: {key} must be a non-empty string"
        );
    }
    let sources = &fixture["provenance"]["sources"];
    assert!(
        sources.as_array().is_some_and(|s| !s.is_empty()),
        "{file_name}: provenance.sources must cite at least one anchor"
    );
    for (index, step) in fixture["steps"].as_array().unwrap().iter().enumerate() {
        let path = format!("{file_name} step {index}");
        let keys: BTreeSet<&str> = step
            .as_object()
            .unwrap_or_else(|| panic!("{path}: step must be an object"))
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            ["input", "expect"].into_iter().collect(),
            "{path}: step keys must be exactly input + expect"
        );
        assert!(
            step["input"].is_object(),
            "{path}: input must be an envelope"
        );
        let expect_keys: BTreeSet<&str> = step["expect"]
            .as_object()
            .unwrap_or_else(|| panic!("{path}: expect must be an object"))
            .keys()
            .map(String::as_str)
            .collect();
        assert!(
            expect_keys.contains("published_kinds")
                && expect_keys.is_subset(
                    &["published_kinds", "withheld_kinds", "rederived"]
                        .into_iter()
                        .collect()
                ),
            "{path}: expect requires published_kinds and admits only withheld_kinds/rederived"
        );
    }
}

/// Replay one sequence fixture through the durable path, asserting the publication contract
/// of every step. Returns nothing; the assertions are the verdict.
fn replay_fixture(file_name: &str) {
    let fixture = load_fixture(file_name);
    validate_fixture_shape(file_name, &fixture);
    assert_eq!(
        fixture["kind"].as_str(),
        Some("mixed_batch_sequence"),
        "{file_name}: kind must be mixed_batch_sequence"
    );
    let fixture_id = fixture["id"].as_str().expect("fixture id");
    assert!(
        !fixture_id.is_empty() && file_name.starts_with(fixture_id),
        "{file_name}: id must be a non-empty prefix of the file name"
    );
    let steps = fixture["steps"]
        .as_array()
        .unwrap_or_else(|| panic!("{file_name}: steps must be an array"));
    assert!(!steps.is_empty(), "{file_name}: empty sequence");

    let mut runtime = Runtime::new();
    // The M3 ledger: kinds a step withheld. A later `rederived` claim must name a kind this
    // set already holds — republication without a prior withholding is not re-derivation.
    let mut withheld_so_far: Vec<String> = Vec::new();

    for (index, step) in steps.iter().enumerate() {
        let path = format!("{file_name} step {index}");
        let envelope: WireEnvelope = serde_json::from_value(step["input"].clone())
            .unwrap_or_else(|e| panic!("{path}: input is not a wire envelope: {e}"));
        let committed = runtime.submit(&envelope);
        let published = published_kinds(&committed);

        let expect = &step["expect"];
        let expected = expect_string_array(
            &expect["published_kinds"],
            &format!("{path} expect.published_kinds"),
        );
        assert_eq!(
            published, expected,
            "{path}: publication must match the pinned order"
        );

        if let Some(withheld) = expect.get("withheld_kinds") {
            for kind in expect_string_array(withheld, &format!("{path} expect.withheld_kinds")) {
                assert!(
                    !published.contains(&kind),
                    "{path}: {kind} must stay withheld (M3 forbids co-publication)"
                );
                if !withheld_so_far.contains(&kind) {
                    withheld_so_far.push(kind);
                }
            }
        }
        if let Some(rederived) = expect.get("rederived") {
            for kind in expect_string_array(rederived, &format!("{path} expect.rederived")) {
                assert!(
                    published.contains(&kind),
                    "{path}: {kind} must be published by the resume that re-derives it"
                );
                assert!(
                    withheld_so_far.contains(&kind),
                    "{path}: {kind} is claimed re-derived but no earlier step withheld it"
                );
            }
        }
    }
}

#[test]
fn syscall_host_tool_mixed_batch_fixture_replays() {
    replay_fixture("syscall-host-tool-mixed-batch.json");
}

#[test]
fn syscall_only_control_plane_fixture_replays() {
    replay_fixture("syscall-only-control-plane.json");
}

// ---------------------------------------------------------------------------------------------
// emitter — regenerate the pinned fixtures from the typed builders when the wire evolves
// ---------------------------------------------------------------------------------------------

/// Run one scenario live, recording the exact envelopes submitted. Resolve envelopes are
/// built against the effect ids the live run actually mints; the fixture then pins them.
fn record_scenario(
    operation: &str,
    script: &mut dyn FnMut(&mut Runtime, &str) -> Vec<(WireEnvelope, Value)>,
) -> Vec<Value> {
    let mut runtime = Runtime::new();
    script(&mut runtime, operation)
        .into_iter()
        .map(|(envelope, expect)| {
            json!({
                "input": serde_json::to_value(&envelope).expect("envelope JSON"),
                "expect": expect,
            })
        })
        .collect()
}

/// The 0.2.62 incident scene: a syscall + host-tool mixed batch.
fn mixed_batch_scenario() -> Vec<Value> {
    record_scenario("op-mixed-batch-1", &mut |runtime, operation| {
        let mut steps = Vec::new();

        let configure = syscall_config(operation);
        runtime.submit(&configure);
        steps.push((configure, json!({ "published_kinds": [] })));

        let start = agent_start(operation);
        let started = runtime.submit(&start);
        let provider_effect = started.published_effects()[0].effect_id.clone();
        steps.push((start, json!({ "published_kinds": ["call_provider"] })));

        // The model's turn mixes a syscall (memory query) with a host tool call.
        let mixed = provider_result(
            operation,
            "in-mixed",
            1_700_000_002_000,
            &provider_effect,
            vec![
                tool_call("call-1", MEMORY_TOOL_NAME, json!({"query": "past briefs"})),
                tool_call("call-2", "search", json!({"q": "kpi benchmarks"})),
            ],
        );
        let mixed_committed = runtime.submit(&mixed);
        let query_effect = mixed_committed.published_effects()[0].effect_id.clone();
        steps.push((
            mixed,
            json!({
                "published_kinds": ["query_memory"],
                "withheld_kinds": ["execute_tools"],
            }),
        ));

        // Settling the syscall effect frees the kind slot; the resume re-derives the batch.
        let settle_query = resolved(
            operation,
            "in-recalls",
            1_700_000_003_000,
            &query_effect,
            memory_queried("prefers numbered sections"),
        );
        let settle_committed = runtime.submit(&settle_query);
        let tools_effect = settle_committed.published_effects()[0].effect_id.clone();
        steps.push((
            settle_query,
            json!({
                "published_kinds": ["execute_tools"],
                "rederived": ["execute_tools"],
            }),
        ));

        // Settling the batch resumes the loop: results plus recalls are in, so the kernel
        // calls the provider again.
        let settle_tools = resolved(
            operation,
            "in-tools",
            1_700_000_004_000,
            &tools_effect,
            tools_succeeded("call-2", "kpi: 12%"),
        );
        runtime.submit(&settle_tools);
        steps.push((
            settle_tools,
            json!({ "published_kinds": ["call_provider"] }),
        ));

        steps
    })
}

/// §5k: a pure control-plane batch (skill activation + plan update) publishes no effect of
/// its own, so the kernel continues the turn itself.
fn control_plane_scenario() -> Vec<Value> {
    record_scenario("op-control-plane-1", &mut |runtime, operation| {
        let mut steps = Vec::new();

        let configure = syscall_config(operation);
        runtime.submit(&configure);
        steps.push((configure, json!({ "published_kinds": [] })));

        let start = agent_start(operation);
        let started = runtime.submit(&start);
        let provider_effect = started.published_effects()[0].effect_id.clone();
        steps.push((start, json!({ "published_kinds": ["call_provider"] })));

        let control = provider_result(
            operation,
            "in-control",
            1_700_000_002_000,
            &provider_effect,
            vec![
                tool_call("call-1", "skill", json!({"name": "debug"})),
                tool_call(
                    "call-2",
                    "update_plan",
                    json!({"progress": "sources listed"}),
                ),
            ],
        );
        runtime.submit(&control);
        steps.push((control, json!({ "published_kinds": ["call_provider"] })));

        steps
    })
}

fn emit_fixtures() {
    let dir = fixture_dir();
    let fixtures = [
        (
            "syscall-host-tool-mixed-batch.json",
            json!({
                "id": "syscall-host-tool-mixed-batch",
                "kind": "mixed_batch_sequence",
                "title": "0.2.62 incident scene: a syscall + host-tool mixed batch",
                "semantics": "One provider turn mixes a memory query (syscall) with a host \
                     tool call. M3: the kernel publishes only the syscall effect; the \
                     execute_tools batch is withheld from the step, then re-derived by the \
                     resume once the syscall effect settles and the kind slot is free. \
                     Settling the batch continues the loop with a fresh provider call.",
                "provenance": {
                    "sources": [
                        "docs/architecture/runtime-causality.md M3/M4",
                        "crates/deepstrike-core/src/runtime/kernel/wire/driver/provider.rs plan_provider_completed",
                        "crates/deepstrike-core/src/runtime/kernel/wire/driver/tests.rs a_mixed_syscall_and_host_tool_batch_resolves_without_re_emitting_the_tool_batch"
                    ]
                },
                "steps": mixed_batch_scenario(),
            }),
        ),
        (
            "syscall-only-control-plane.json",
            json!({
                "id": "syscall-only-control-plane",
                "kind": "mixed_batch_sequence",
                "title": "§5k: a pure control-plane batch",
                "semantics": "A provider turn of syscalls alone (skill activation + plan \
                     update) publishes no effect of its own. Without an answer the operation \
                     would stall, so the kernel continues the turn itself: the step's only \
                     publication is the next provider call.",
                "provenance": {
                    "sources": [
                        "docs/architecture/runtime-causality.md M1",
                        "crates/deepstrike-core/src/runtime/kernel/wire/driver/provider.rs plan_provider_completed (§5k note)",
                        "crates/deepstrike-core/src/runtime/kernel/wire/driver/tests.rs a_syscall_only_turn_continues_with_another_provider_call"
                    ]
                },
                "steps": control_plane_scenario(),
            }),
        ),
    ];
    for (file_name, fixture) in fixtures {
        let path = dir.join(file_name);
        let mut text = serde_json::to_string_pretty(&fixture).expect("fixture JSON");
        text.push('\n');
        fs::write(&path, text)
            .unwrap_or_else(|e| panic!("failed to write {}: {}", path.display(), e));
        eprintln!("emitted {}", path.display());
    }
}

#[test]
fn maybe_emit_fixtures() {
    if std::env::var("DS_EMIT_MIXED_BATCH").is_ok() {
        emit_fixtures();
    }
}
