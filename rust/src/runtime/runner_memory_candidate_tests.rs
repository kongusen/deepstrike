//! M1/M2 · the memory runtime policy and the candidate sink, through the runner's real write
//! paths: session extraction, semantic archive, and the PersistMemory effect answer.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use deepstrike_core::context::renderer::InternalRenderedContext;
use deepstrike_core::mm::memory::{
    MemoryAuthor, MemoryKind, MemoryProvenance, MemoryQuery, MemoryRecall, MemoryRecord,
    MemoryScope, MemoryTrustLevel,
};
use deepstrike_core::runtime::kernel::KernelObservation;
use deepstrike_core::runtime::session::SessionEvent;
use deepstrike_core::types::message::{CoreMessage, ToolSchema};
use futures::StreamExt;

use super::{MilestonePolicy, RuntimeOptions, RuntimeRunner};
use crate::memory::{
    InMemoryMemoryCandidateSink, MemoryCandidateSink, MemoryCandidateSource, MemoryRetrievalPolicy,
    MemoryRuntimePolicy, MemoryStore, MemoryWritePolicy,
};
use crate::providers::{LLMProvider, StreamEvent};
use crate::runtime::session_log::{InMemorySessionLog, SessionLog};

const AGENT: &str = "agent-candidate";

/// Answers every call with the same text and counts the calls. The text is a valid extraction
/// answer, so the main turn, the extraction pass and the archive summary all accept it.
struct CountingProvider {
    calls: Arc<AtomicUsize>,
    /// The text of every turn of every context the provider was shown, call by call.
    seen_turns: Arc<std::sync::Mutex<Vec<Vec<(String, String)>>>>,
}

const EXTRACTED: &str = r#"{"memories":[{"name":"prefers-focused-tests","kind":"feedback","content":"User prefers focused tests.","description":"stated preference","confidence":1}]}"#;

#[async_trait::async_trait]
impl LLMProvider for CountingProvider {
    async fn complete(
        &self,
        _context: &InternalRenderedContext,
        _tools: &[ToolSchema],
        _extensions: Option<&serde_json::Value>,
    ) -> crate::Result<CoreMessage> {
        unreachable!("the runner streams")
    }

    async fn stream(
        &self,
        context: &InternalRenderedContext,
        _tools: &[ToolSchema],
        _extensions: Option<&serde_json::Value>,
        _state: Option<&crate::providers::ProviderRunState>,
    ) -> crate::Result<Box<dyn futures::Stream<Item = crate::Result<StreamEvent>> + Send + Unpin>>
    {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.seen_turns.lock().unwrap().push(
            context
                .turns
                .iter()
                .map(|turn| {
                    let role = serde_json::json!(turn.role)
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    let content = serde_json::json!(turn.content)
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    (role, content)
                })
                .collect(),
        );
        Ok(Box::new(futures::stream::iter(vec![
            Ok(StreamEvent::TextDelta {
                delta: EXTRACTED.into(),
            }),
            Ok(StreamEvent::Done),
        ])))
    }
}

/// Counts writes and searches; holds nothing but the hits every search answers with.
#[derive(Default)]
struct CountingStore {
    puts: Arc<AtomicUsize>,
    searches: Arc<AtomicUsize>,
    hits: Vec<MemoryRecall>,
}

#[async_trait::async_trait]
impl MemoryStore for CountingStore {
    async fn put(&self, _agent_id: &str, _record: MemoryRecord) -> crate::Result<()> {
        self.puts.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    async fn get(&self, _agent_id: &str, _record_id: &str) -> crate::Result<Option<MemoryRecord>> {
        Ok(None)
    }
    async fn delete(&self, _agent_id: &str, _record_id: &str) -> crate::Result<()> {
        Ok(())
    }
    async fn search(
        &self,
        _agent_id: &str,
        _query: &MemoryQuery,
    ) -> crate::Result<Vec<MemoryRecall>> {
        self.searches.fetch_add(1, Ordering::SeqCst);
        Ok(self.hits.clone())
    }
    async fn save_session(
        &self,
        _data: deepstrike_core::memory::durable::SessionData,
    ) -> crate::Result<()> {
        Ok(())
    }
}

struct Harness {
    runner: RuntimeRunner,
    log: Arc<InMemorySessionLog>,
    sink: Arc<InMemoryMemoryCandidateSink>,
    model_calls: Arc<AtomicUsize>,
    puts: Arc<AtomicUsize>,
    searches: Arc<AtomicUsize>,
    seen_turns: Arc<std::sync::Mutex<Vec<Vec<(String, String)>>>>,
}

fn policy(write: MemoryWritePolicy) -> MemoryRuntimePolicy {
    MemoryRuntimePolicy {
        retrieval: MemoryRetrievalPolicy::Host,
        session_extraction: write,
        model_writes: write,
        semantic_archive: write,
    }
}

fn harness(
    memory_runtime_policy: Option<MemoryRuntimePolicy>,
    with_sink: bool,
    pre_query_calls: Option<Arc<AtomicUsize>>,
) -> Harness {
    harness_with_hits(
        memory_runtime_policy,
        with_sink,
        pre_query_calls,
        Vec::new(),
    )
}

fn harness_with_hits(
    memory_runtime_policy: Option<MemoryRuntimePolicy>,
    with_sink: bool,
    pre_query_calls: Option<Arc<AtomicUsize>>,
    hits: Vec<MemoryRecall>,
) -> Harness {
    let log = Arc::new(InMemorySessionLog::new());
    let sink = Arc::new(InMemoryMemoryCandidateSink::new());
    let model_calls = Arc::new(AtomicUsize::new(0));
    let seen_turns = Arc::new(std::sync::Mutex::new(Vec::new()));
    let store = CountingStore {
        hits,
        ..CountingStore::default()
    };
    let (puts, searches) = (store.puts.clone(), store.searches.clone());
    let pre_query_memory = pre_query_calls.map(|calls| {
        Arc::new(move |goal: &str| {
            calls.fetch_add(1, Ordering::SeqCst);
            vec![MemoryQuery {
                scope: MemoryScope::new("tenant", "candidates"),
                query: goal.to_string(),
                top_k: 3,
                kinds: Vec::new(),
                min_score: None,
            }]
        }) as Arc<dyn Fn(&str) -> Vec<MemoryQuery> + Send + Sync>
    });
    let runner = RuntimeRunner::new(RuntimeOptions {
        artifact_set_digest: None,
        provider: Box::new(CountingProvider {
            calls: model_calls.clone(),
            seen_turns: seen_turns.clone(),
        }),
        execution_plane: None,
        session_log: Some(log.clone()),
        compression_store: None,
        payload_store: None,
        kernel_reliability: None,
        session_id: None,
        max_tokens: 8000,
        max_turns: Some(3),
        timeout_ms: None,
        extensions: None,
        agent_id: Some(AGENT.into()),
        memory_scope: Some(MemoryScope::new("tenant", "candidates")),
        pre_query_memory,
        system_prompt: None,
        initial_memory: vec![],
        skill_dir: None,
        memory_store: Some(Box::new(store)),
        knowledge_source: None,
        signal_source: None,
        governance: None,
        os_profile: None,
        governance_policy: None,
        signal_policy: None,
        scheduler_policy: None,
        resource_quota: None,
        memory_policy: None,
        memory_runtime_policy,
        memory_candidate_sink: with_sink.then(|| sink.clone() as Arc<dyn MemoryCandidateSink>),
        tokenizer: None,
        enable_plan_tool: None,
        on_tool_suspend: None,
        on_permission_request: None,
        milestone_policy: MilestonePolicy::AutoPass,
        milestone_contract: None,
        run_spec: None,
        allowed_tool_ids: None,
        baseline_tool_ids: None,
        on_turn_metrics: None,
        stable_core_tool_ids: vec![],
        on_milestone_evaluate: None,
    });
    Harness {
        runner,
        log,
        sink,
        model_calls,
        puts,
        searches,
        seen_turns,
    }
}

async fn run_to_end(runner: &RuntimeRunner, session_id: &str) {
    let mut stream = runner
        .run_streaming("remember how I like tests", &[], None, Some(session_id))
        .await
        .expect("run starts");
    while let Some(event) = stream.next().await {
        event.expect("run event");
    }
}

async fn events(log: &InMemorySessionLog, session_id: &str) -> Vec<SessionEvent> {
    log.read(session_id, 0, None)
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.event)
        .collect()
}

fn proposal(content: &str) -> MemoryRecord {
    MemoryRecord {
        record_id: "memory:proposal".into(),
        scope: MemoryScope::new("tenant", "candidates"),
        name: "child-proposal".into(),
        kind: MemoryKind::Reference,
        content: content.into(),
        description: "proposed by a child agent".into(),
        provenance: MemoryProvenance {
            session_id: Some("session".into()),
            author: MemoryAuthor::Model,
            trust: MemoryTrustLevel::Untrusted,
            evidence_refs: Vec::new(),
        },
        created_at: 1,
        updated_at: 1,
        last_recalled_at: None,
        recall_count: 0,
        confidence: 1.0,
        links: Vec::new(),
        pinned: false,
        ttl_days: None,
    }
}

#[tokio::test]
async fn a_candidate_sink_path_without_a_sink_refuses_to_run() {
    let h = harness(Some(policy(MemoryWritePolicy::CandidateSink)), false, None);
    let Err(error) = h
        .runner
        .run_streaming("go", &[], None, Some("no-sink"))
        .await
    else {
        panic!("a candidate_sink policy without a sink must not start");
    };
    let message = error.to_string();
    assert!(message.contains("memory_candidate_sink"), "{message}");
    assert!(message.contains("session_extraction, model_writes, semantic_archive"));
    assert_eq!(h.model_calls.load(Ordering::SeqCst), 0);
}

/// S03 · an extracted memory becomes a pending candidate: the store holds nothing, the session
/// records `memory_candidate_pending`, and nothing is logged as written.
#[tokio::test]
async fn session_extraction_to_the_candidate_sink_writes_no_record() {
    let h = harness(Some(policy(MemoryWritePolicy::CandidateSink)), true, None);
    run_to_end(&h.runner, "extract-candidate").await;

    assert_eq!(
        h.puts.load(Ordering::SeqCst),
        0,
        "the store is never written"
    );
    let held = h.sink.candidates(Some(AGENT));
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].source, MemoryCandidateSource::SessionExtraction);
    assert_eq!(held[0].record.name, "prefers-focused-tests");
    assert!(held[0].candidate_id.starts_with("candidate:"));

    let log = events(&h.log, "extract-candidate").await;
    assert!(
        !log.iter()
            .any(|event| matches!(event, SessionEvent::MemoryWritten { .. })),
        "a pending candidate is not a written record"
    );
    let pending = log
        .iter()
        .find_map(|event| match event {
            SessionEvent::MemoryCandidatePending {
                candidate_id,
                source,
                receipt_id,
                name,
                ..
            } => Some((candidate_id, source, receipt_id, name)),
            _ => None,
        })
        .expect("a memory_candidate_pending audit");
    assert_eq!(pending.0, &held[0].candidate_id);
    assert_eq!(pending.1, "session_extraction");
    assert!(
        pending.2.is_some(),
        "the host path records the sink's receipt"
    );
    assert_eq!(pending.3, "prefers-focused-tests");
}

/// Without a policy the extraction still writes the store, as before M1.
#[tokio::test]
async fn no_policy_keeps_extraction_writing_the_store() {
    let h = harness(None, false, None);
    run_to_end(&h.runner, "extract-store").await;
    assert_eq!(h.puts.load(Ordering::SeqCst), 1);
    assert!(h.sink.candidates(None).is_empty());
    assert!(
        events(&h.log, "extract-store")
            .await
            .iter()
            .any(|event| matches!(event, SessionEvent::MemoryWritten { .. }))
    );
}

/// M1 · disabled paths are checked before the auxiliary model: the run makes its one turn and
/// no extraction call; disabled retrieval skips the prefetch hook and host queries.
#[tokio::test]
async fn disabled_paths_cost_no_model_call_and_no_store_access() {
    let pre_query_calls = Arc::new(AtomicUsize::new(0));
    let h = harness(
        Some(MemoryRuntimePolicy {
            retrieval: MemoryRetrievalPolicy::Disabled,
            ..policy(MemoryWritePolicy::Disabled)
        }),
        false,
        Some(pre_query_calls.clone()),
    );
    run_to_end(&h.runner, "all-disabled").await;

    assert_eq!(
        h.model_calls.load(Ordering::SeqCst),
        1,
        "only the main turn"
    );
    assert_eq!(pre_query_calls.load(Ordering::SeqCst), 0);
    assert_eq!(h.puts.load(Ordering::SeqCst), 0);

    let hits = h
        .runner
        .query_memory(
            MemoryQuery {
                scope: MemoryScope::new("tenant", "candidates"),
                query: "tests".into(),
                top_k: 3,
                kinds: Vec::new(),
                min_score: None,
            },
            Some("all-disabled"),
            None,
        )
        .await
        .unwrap();
    assert!(hits.is_empty());
    assert_eq!(h.searches.load(Ordering::SeqCst), 0);
}

/// S04 · PersistMemory answered by the sink. A redelivery of the same effect after a lost
/// acknowledgement asks for the same candidate and gets the original receipt; the sink holds
/// one candidate. A different proposal under the same effect is a conflict.
#[tokio::test]
async fn a_redelivered_persist_memory_effect_gets_the_original_receipt() {
    let h = harness(Some(policy(MemoryWritePolicy::CandidateSink)), true, None);

    let first = h
        .runner
        .memory_candidate_effect_result("op-1", "effect-7", proposal("fact"))
        .await;
    assert_eq!(first["kind"], "memory_candidate_result");
    assert_eq!(first["effect_id"], "effect-7");
    assert_eq!(first["status"], "pending");
    assert!(first.get("error").is_none());

    let again = h
        .runner
        .memory_candidate_effect_result("op-1", "effect-7", proposal("fact"))
        .await;
    assert_eq!(again, first, "the same answer, not a new candidate");
    assert_eq!(h.sink.candidates(Some(AGENT)).len(), 1);
    assert_eq!(
        h.sink.candidates(Some(AGENT))[0].source,
        MemoryCandidateSource::ChildRequest
    );

    let conflict = h
        .runner
        .memory_candidate_effect_result("op-1", "effect-7", proposal("another fact"))
        .await;
    assert_eq!(conflict["status"], "rejected");
    assert_eq!(conflict["reason_code"], "candidate_conflict");
    assert_ne!(conflict["receipt_id"], first["receipt_id"]);
    assert_eq!(h.sink.candidates(Some(AGENT))[0].record.content, "fact");

    let other_effect = h
        .runner
        .memory_candidate_effect_result("op-1", "effect-8", proposal("fact"))
        .await;
    assert_eq!(other_effect["status"], "pending");
    assert_ne!(other_effect["candidate_id"], first["candidate_id"]);
    assert_eq!(h.puts.load(Ordering::SeqCst), 0);
}

/// A sink that gives no answer leaves the effect retryable: the bridge sees `error`.
#[tokio::test]
async fn a_sink_error_is_a_retryable_candidate_result() {
    struct Unreachable;
    #[async_trait::async_trait]
    impl MemoryCandidateSink for Unreachable {
        async fn submit(
            &self,
            _agent_id: &str,
            _candidate: crate::memory::MemoryCandidate,
        ) -> crate::Result<crate::memory::MemoryCandidateReceipt> {
            Err(crate::Error::Other("sink unreachable".into()))
        }
    }
    let mut h = harness(Some(policy(MemoryWritePolicy::CandidateSink)), true, None);
    h.runner.opts.memory_candidate_sink = Some(Arc::new(Unreachable));
    let result = h
        .runner
        .memory_candidate_effect_result("op-1", "effect-1", proposal("fact"))
        .await;
    assert_eq!(result["kind"], "memory_candidate_result");
    assert!(
        result["error"]
            .as_str()
            .is_some_and(|error| error.contains("sink unreachable"))
    );
    assert!(result.get("status").is_none());
}

/// The semantic archive proposes one candidate per page-out effect, named by the effect, so a
/// second archive of the same effect is the same candidate rather than a new one.
#[tokio::test]
async fn the_semantic_archive_hands_one_candidate_per_page_out_effect() {
    let h = harness(Some(policy(MemoryWritePolicy::CandidateSink)), true, None);
    let archived = vec![CoreMessage::user("we decided to keep tests focused")];
    for _ in 0..2 {
        h.runner
            .archive_semantic_page_out(
                archived.clone(),
                Some("auto_compact".into()),
                "archive-session",
                "op-archive",
                "effect:page-out/1",
            )
            .await;
    }
    let held = h.sink.candidates(Some(AGENT));
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].source, MemoryCandidateSource::SemanticArchive);
    assert_eq!(held[0].record.name, "page-out-effect:page-out_1");
    assert_eq!(
        held[0].record.provenance.session_id.as_deref(),
        Some("archive-session")
    );
    assert_eq!(h.puts.load(Ordering::SeqCst), 0);
    let log = events(&h.log, "archive-session").await;
    let pending = log
        .iter()
        .filter(|event| matches!(event, SessionEvent::MemoryCandidatePending { .. }))
        .count();
    assert_eq!(
        pending, 2,
        "each delivery is audited with the sink's answer"
    );
    assert!(
        !log.iter()
            .any(|event| matches!(event, SessionEvent::MemoryWritten { .. }))
    );
}

#[tokio::test]
async fn a_disabled_semantic_archive_makes_no_summarizer_call() {
    let h = harness(Some(policy(MemoryWritePolicy::Disabled)), false, None);
    h.runner
        .archive_semantic_page_out(
            vec![CoreMessage::user("anything")],
            None,
            "archive-off",
            "op",
            "effect-1",
        )
        .await;
    assert_eq!(h.model_calls.load(Ordering::SeqCst), 0);
    assert_eq!(h.puts.load(Ordering::SeqCst), 0);
}

/// The kernel's pending-candidate fact is projected into the session log with its source.
#[tokio::test]
async fn the_kernel_candidate_observation_is_projected_into_the_session_log() {
    let h = harness(Some(policy(MemoryWritePolicy::CandidateSink)), true, None);
    let kernel = Arc::new(tokio::sync::Mutex::new(
        crate::runtime::canonical_runner_runtime::CanonicalRunnerRuntime::new(
            crate::runtime::canonical_kernel::CanonicalKernel::default(),
            Arc::new(crate::runtime::InMemoryKernelJournal::new()),
            "projection",
            crate::runtime::canonical_runner_runtime::CanonicalRunnerOptions {
                max_context_tokens: 1_000,
                max_turns: Some(3),
                max_total_tokens: None,
                max_wall_ms: None,
                artifact_set_digest: None,
                memory_binding_id: "memory".into(),
                persist_payload: None,
            },
        )
        .unwrap(),
    ));
    let mut observations = vec![KernelObservation::MemoryCandidatePending {
        turn: 2,
        candidate_id: "candidate:abc".into(),
        name: "child-proposal".into(),
    }];
    h.runner
        .append_observations(
            "projection",
            &kernel,
            &mut observations,
            &mut std::collections::VecDeque::new(),
            0,
        )
        .await;
    let log = events(&h.log, "projection").await;
    let projected = serde_json::to_value(
        log.iter()
            .find(|event| event.kind_str() == "memory_candidate_pending")
            .expect("projected"),
    )
    .unwrap();
    assert_eq!(
        projected,
        serde_json::json!({
            "kind": "memory_candidate_pending",
            "turn": 2,
            "candidate_id": "candidate:abc",
            "source": "child_request",
            "name": "child-proposal",
        })
    );
}

/// A rejected candidate is audited as `memory_candidate_rejected`, and the funnel reports it
/// as not written.
#[tokio::test]
async fn a_rejected_candidate_is_audited_and_reported_unwritten() {
    let h = harness(Some(policy(MemoryWritePolicy::CandidateSink)), true, None);
    let source = MemoryCandidateSource::SessionExtraction;
    let first = h
        .runner
        .write_memory_routed(
            proposal("fact"),
            Some("reject"),
            None,
            Some((source, "candidate:same".into())),
        )
        .await
        .unwrap();
    assert!(first, "a pending candidate counts as handed over");
    let second = h
        .runner
        .write_memory_routed(
            proposal("different fact"),
            Some("reject"),
            None,
            Some((source, "candidate:same".into())),
        )
        .await
        .unwrap();
    assert!(!second);
    let rejected = events(&h.log, "reject")
        .await
        .into_iter()
        .find_map(|event| match event {
            SessionEvent::MemoryCandidateRejected {
                reason_code,
                source,
                ..
            } => Some((reason_code, source)),
            _ => None,
        })
        .expect("a memory_candidate_rejected audit");
    assert_eq!(rejected.0, "candidate_conflict");
    assert_eq!(rejected.1, "session_extraction");
}

fn recalled(record_id: &str, content: &str) -> MemoryRecall {
    MemoryRecall {
        record: MemoryRecord {
            record_id: record_id.into(),
            kind: MemoryKind::Reference,
            provenance: MemoryProvenance {
                session_id: None,
                author: MemoryAuthor::Host,
                trust: MemoryTrustLevel::HostVerified,
                evidence_refs: Vec::new(),
            },
            ..proposal(content)
        },
        score: 0.9,
        why: "fixture".into(),
    }
}

/// M5 · the vector every SDK's recall-projection test pins.
#[test]
fn the_recall_turn_matches_the_other_sdks() {
    let record = MemoryRecord {
        record_id: "record-v".into(),
        scope: MemoryScope::new("t", "n"),
        kind: MemoryKind::Project,
        provenance: MemoryProvenance {
            session_id: None,
            author: MemoryAuthor::Extraction,
            trust: MemoryTrustLevel::Untrusted,
            evidence_refs: Vec::new(),
        },
        ..proposal("prefers numbered sections")
    };
    assert_eq!(
        crate::memory::memory_recall_turn(&record),
        "[MEMORY record_id=record-v kind=project trust=untrusted] prefers numbered sections"
    );
}

/// M5 · the turn-1 prefetch puts each hit in history as its own canonical recall turn, once per
/// record however many queries hit it, and audits the prefetch.
#[tokio::test]
async fn the_initial_prefetch_lands_each_hit_as_a_canonical_history_turn() {
    let h = harness_with_hits(
        None,
        false,
        Some(Arc::new(AtomicUsize::new(0))),
        vec![
            recalled("record-a", "FACT_A"),
            recalled("record-a", "FACT_A"),
        ],
    );
    run_to_end(&h.runner, "prefetch").await;

    let first = h.seen_turns.lock().unwrap()[0].clone();
    let recall_turns: Vec<_> = first
        .into_iter()
        .filter(|(_, content)| content.starts_with("[MEMORY "))
        .collect();
    assert_eq!(
        recall_turns,
        vec![(
            "user".to_string(),
            "[MEMORY record_id=record-a kind=reference trust=host_verified] FACT_A".to_string()
        )]
    );
    let audited: Vec<_> = events(&h.log, "prefetch")
        .await
        .into_iter()
        .filter_map(|event| match event {
            SessionEvent::MemoryRetrievalResult { hits } => Some(hits),
            _ => None,
        })
        .collect();
    assert_eq!(audited.len(), 1);
    assert_eq!(audited[0].len(), 1, "the duplicate hit is one recall");
}

/// K4 · M5: a sprint renewal re-runs the prefetch for the live goal. The hit is seeded as keyed
/// knowledge, not a history message (a running operation refuses host history), and the
/// re-fetch is audited.
#[tokio::test]
async fn a_renewal_re_runs_the_prefetch_into_keyed_knowledge() {
    let h = harness_with_hits(
        None,
        false,
        Some(Arc::new(AtomicUsize::new(0))),
        vec![recalled("record-a", "FACT_A")],
    );
    *h.runner.current_goal.lock().unwrap() = Some("keep going".into());
    let kernel = Arc::new(tokio::sync::Mutex::new(
        crate::runtime::canonical_runner_runtime::CanonicalRunnerRuntime::new(
            crate::runtime::canonical_kernel::CanonicalKernel::default(),
            Arc::new(crate::runtime::InMemoryKernelJournal::new()),
            "renewal",
            crate::runtime::canonical_runner_runtime::CanonicalRunnerOptions {
                max_context_tokens: 1_000,
                max_turns: Some(3),
                max_total_tokens: None,
                max_wall_ms: None,
                artifact_set_digest: None,
                memory_binding_id: "memory".into(),
                persist_payload: None,
            },
        )
        .unwrap(),
    ));
    // A running operation: the state a live renewal observation arrives in.
    kernel
        .lock()
        .await
        .start_agent_value(serde_json::json!({ "goal": "keep going" }), None)
        .await
        .unwrap();
    let mut observations = vec![KernelObservation::Renewed { sprint: 1 }];
    h.runner
        .append_observations(
            "renewal",
            &kernel,
            &mut observations,
            &mut std::collections::VecDeque::new(),
            0,
        )
        .await;

    assert_eq!(h.searches.load(Ordering::SeqCst), 1);
    // The audit is written only after the kernel accepted every seed command.
    let audited = events(&h.log, "renewal")
        .await
        .into_iter()
        .filter(|event| matches!(event, SessionEvent::MemoryRetrievalResult { .. }))
        .count();
    assert_eq!(audited, 1);
}

/// M4 · `AgentSession::remember` reports a write the kernel refused instead of returning `Ok`
/// for a record that never reached the store.
#[tokio::test]
async fn a_refused_remember_is_an_error_and_reaches_no_store() {
    let h = harness(None, false, None);
    let puts = h.puts.clone();
    let agent = crate::agent::Agent::with_runner(h.runner);
    let refused = MemoryRecord {
        name: "n".repeat(10_000),
        ..proposal("fact")
    };
    let error = agent
        .session("remember")
        .remember(refused)
        .await
        .expect_err("the kernel refuses an oversized name");
    assert!(error.to_string().contains("memory write denied"));
    assert_eq!(puts.load(Ordering::SeqCst), 0);
    agent
        .session("remember")
        .remember(proposal("fact"))
        .await
        .expect("an admissible write is stored");
    assert_eq!(puts.load(Ordering::SeqCst), 1);
}
