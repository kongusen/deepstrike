//! M1/M2 · the memory runtime policy and the candidate sink, through the runner's real write
//! paths and the PersistMemory effect answer.

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

/// Answers every call with the same ordinary text and counts the calls.
struct CountingProvider {
    calls: Arc<AtomicUsize>,
    /// The text of every turn of every context the provider was shown, call by call.
    seen_turns: Arc<std::sync::Mutex<Vec<Vec<(String, String)>>>>,
    /// The rendered knowledge of every context the provider was shown, call by call.
    seen_knowledge: Arc<std::sync::Mutex<Vec<String>>>,
}

const MODEL_OUTPUT: &str = "done";

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
        self.seen_knowledge
            .lock()
            .unwrap()
            .push(context.system_knowledge.clone());
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
                delta: MODEL_OUTPUT.into(),
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
    seen_knowledge: Arc<std::sync::Mutex<Vec<String>>>,
}

fn policy(write: MemoryWritePolicy) -> MemoryRuntimePolicy {
    MemoryRuntimePolicy {
        retrieval: MemoryRetrievalPolicy::Host,
        model_writes: write,
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
    let seen_knowledge = Arc::new(std::sync::Mutex::new(Vec::new()));
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
        provider: Box::new(CountingProvider {
            calls: model_calls.clone(),
            seen_turns: seen_turns.clone(),
            seen_knowledge: seen_knowledge.clone(),
        }),
        execution_plane: None,
        session_log: Some(log.clone()),
        compression_store: None,
        payload_store: None,
        kernel_reliability: None,
        notes_budget_tokens: None,
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
        dependency_outputs: Default::default(),
        inherited_knowledge: Vec::new(),
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
        seen_knowledge,
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
    assert!(message.contains("model_writes"));
    assert_eq!(h.model_calls.load(Ordering::SeqCst), 0);
}

/// Ordinary model output does not trigger an automatic extraction or archive write.
#[tokio::test]
async fn plain_model_output_does_not_create_memory_candidate() {
    let h = harness(Some(policy(MemoryWritePolicy::CandidateSink)), true, None);
    run_to_end(&h.runner, "plain-model-output").await;

    assert_eq!(h.puts.load(Ordering::SeqCst), 0, "the store is never written");
    assert!(
        h.sink.candidates(Some(AGENT)).is_empty(),
        "memory candidates require an explicit model proposal"
    );

    let log = events(&h.log, "plain-model-output").await;
    assert!(!log.iter().any(|event| matches!(
        event,
        SessionEvent::MemoryWritten { .. } | SessionEvent::MemoryCandidatePending { .. }
    )));
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
        .memory_candidate_effect_result("op-1", "effect-7", MemoryCandidateSource::ChildRequest, proposal("fact"))
        .await;
    assert_eq!(first["kind"], "memory_candidate_result");
    assert_eq!(first["effect_id"], "effect-7");
    assert_eq!(first["status"], "pending");
    assert!(first.get("error").is_none());

    let again = h
        .runner
        .memory_candidate_effect_result("op-1", "effect-7", MemoryCandidateSource::ChildRequest, proposal("fact"))
        .await;
    assert_eq!(again, first, "the same answer, not a new candidate");
    assert_eq!(h.sink.candidates(Some(AGENT)).len(), 1);
    assert_eq!(
        h.sink.candidates(Some(AGENT))[0].source,
        MemoryCandidateSource::ChildRequest
    );

    let conflict = h
        .runner
        .memory_candidate_effect_result("op-1", "effect-7", MemoryCandidateSource::ChildRequest, proposal("another fact"))
        .await;
    assert_eq!(conflict["status"], "rejected");
    assert_eq!(conflict["reason_code"], "candidate_conflict");
    assert_ne!(conflict["receipt_id"], first["receipt_id"]);
    assert_eq!(h.sink.candidates(Some(AGENT))[0].record.content, "fact");

    let other_effect = h
        .runner
        .memory_candidate_effect_result("op-1", "effect-8", MemoryCandidateSource::ChildRequest, proposal("fact"))
        .await;
    // Another effect is another candidate, but the same fact is not reviewed twice.
    assert_ne!(other_effect["candidate_id"], first["candidate_id"]);
    assert_eq!(other_effect["status"], "rejected");
    assert_eq!(other_effect["reason_code"], "already_pending_review");
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
        .memory_candidate_effect_result("op-1", "effect-1", MemoryCandidateSource::ChildRequest, proposal("fact"))
        .await;
    assert_eq!(result["kind"], "memory_candidate_result");
    assert!(
        result["error"]
            .as_str()
            .is_some_and(|error| error.contains("sink unreachable"))
    );
    assert!(result.get("status").is_none());
}

/// Model proposals use a stable candidate identity for redelivery.
#[tokio::test]
async fn model_proposals_use_one_candidate_per_effect() {
    let h = harness(Some(policy(MemoryWritePolicy::CandidateSink)), true, None);
    let first = h
        .runner
        .memory_candidate_effect_result("op-1", "effect-1", MemoryCandidateSource::ModelProposal, proposal("fact"))
        .await;
    assert_eq!(first["status"], "pending");
    let again = h
        .runner
        .memory_candidate_effect_result("op-1", "effect-1", MemoryCandidateSource::ModelProposal, proposal("fact"))
        .await;
    assert_eq!(again, first);
    let conflict = h
        .runner
        .memory_candidate_effect_result("op-1", "effect-1", MemoryCandidateSource::ModelProposal, proposal("different fact"))
        .await;
    assert_eq!(conflict["status"], "rejected");
    assert_eq!(conflict["reason_code"], "candidate_conflict");
    let other = h
        .runner
        .memory_candidate_effect_result("op-1", "effect-2", MemoryCandidateSource::ModelProposal, proposal("fact"))
        .await;
    // Another effect is another candidate, but the same fact is not reviewed twice.
    assert_ne!(other["candidate_id"], first["candidate_id"]);
    assert_eq!(other["reason_code"], "already_pending_review");
    let distinct = h
        .runner
        .memory_candidate_effect_result("op-1", "effect-3", MemoryCandidateSource::ModelProposal, proposal("another fact"))
        .await;
    assert_eq!(distinct["status"], "pending");
    assert_eq!(h.sink.candidates(Some(AGENT)).len(), 2);
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
    let source = MemoryCandidateSource::ModelProposal;
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
    assert_eq!(rejected.1, "model_proposal");
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

/// M5 · the turn-1 prefetch puts each hit in keyed knowledge as its canonical recall line, once
/// per record however many queries hit it, never in history, and audits the prefetch.
#[tokio::test]
async fn the_initial_prefetch_lands_each_hit_as_canonical_keyed_knowledge() {
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

    let knowledge = h.seen_knowledge.lock().unwrap()[0].clone();
    let recall_lines: Vec<_> = knowledge
        .lines()
        .filter(|line| line.starts_with("[MEMORY "))
        .collect();
    assert_eq!(
        recall_lines,
        vec!["[MEMORY record_id=record-a kind=reference trust=host_verified] proposed by a child agent"]
    );
    let first = h.seen_turns.lock().unwrap()[0].clone();
    assert!(first
        .iter()
        .all(|(_, content)| !content.starts_with("[MEMORY ")));
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
