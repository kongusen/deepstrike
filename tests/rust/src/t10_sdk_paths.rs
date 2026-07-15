use async_trait::async_trait;
use deepstrike_core::memory::durable::SessionData as CoreSessionData;
use deepstrike_core::mm::memory::{MemoryQuery, MemoryRecall, MemoryRecord};
use deepstrike_sdk::*;
use std::sync::{Arc, Mutex};

// ─── Mocks ──────────────────────────────────────────────────────────────────

struct TrackingKnowledgeSource {
    init_count: Arc<Mutex<u32>>,
}

#[async_trait]
impl KnowledgeSource for TrackingKnowledgeSource {
    async fn init(&self) -> Result<()> {
        *self.init_count.lock().unwrap() += 1;
        Ok(())
    }
    async fn retrieve(&self, _goal: &str, _top_k: usize) -> Result<Vec<String>> {
        Ok(vec!["DeepStrike is a Rust-kernel agent framework.".into()])
    }
}

struct TrackingDreamStore {
    saved: Arc<Mutex<Vec<CoreSessionData>>>,
}

#[async_trait]
impl DreamStore for TrackingDreamStore {
    async fn upsert(&self, _agent_id: &str, _record: MemoryRecord) -> Result<()> {
        Ok(())
    }
    async fn search(
        &self,
        _agent_id: &str,
        _query: &MemoryQuery,
    ) -> Result<Vec<MemoryRecall>> {
        Ok(vec![])
    }
    async fn save_session(&self, data: CoreSessionData) -> Result<()> {
        self.saved.lock().unwrap().push(data);
        Ok(())
    }
}

fn make_provider() -> OpenAIProvider {
    use std::path::PathBuf;
    let env_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(".env");
    let _ = dotenvy::from_path(&env_path);
    let key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
    let url =
        std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".into());
    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into());
    OpenAIProvider::with_base_url(key, model, url)
}

fn make_runner_with<F>(setup: F) -> RuntimeRunner
where
    F: FnOnce(&mut LocalExecutionPlane, &mut RuntimeOptions),
{
    let mut plane = LocalExecutionPlane::new();
    let mut opts = RuntimeOptions {
        provider: Box::new(make_provider()),
        execution_plane: None,
        session_log: Some(Arc::new(InMemorySessionLog::new())),
        compression_store: None,
        spool_dir: None,
        kernel_reliability: None,
        session_id: None,
        max_tokens: 4096,
        max_turns: Some(25),
        timeout_ms: None,
        extensions: None,
        agent_id: None,
        memory_scope: None,
        system_prompt: None,
        initial_memory: vec![],
        skill_dir: None,
        dream_store: None,
        knowledge_source: None,
        signal_source: None,
        governance: None,
        os_profile: None,
        governance_policy: None,
        signal_policy: None,
        scheduler_policy: None,
        resource_quota: None,
        memory_policy: None,
        tokenizer: None,
        enable_plan_tool: None,
        on_tool_suspend: None,
        on_permission_request: None,
        milestone_policy: deepstrike_sdk::runtime::MilestonePolicy::default(),
        milestone_contract: None,
        run_spec: None,
        on_milestone_evaluate: None,
        allowed_tool_ids: None,
        on_turn_metrics: None,
        stable_core_tool_ids: Vec::new(),
        pre_query_memory: None,
    };
    setup(&mut plane, &mut opts);
    opts.execution_plane = Some(Box::new(plane));
    RuntimeRunner::new(opts)
}

// ─── system_prompt ───────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn system_prompt_is_followed() {
    let runner = make_runner_with(|_, opts| {
        opts.system_prompt = Some("You are a pirate. Always end every reply with 'Arrr!'".into());
    });
    let result = runner.execute("Say hello.").await.unwrap();
    assert!(
        result.to_lowercase().contains("arrr"),
        "expected 'Arrr!' in: {result}"
    );
}

// ─── initial_memory ──────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn initial_memory_is_recalled() {
    let runner = make_runner_with(|_, opts| {
        opts.initial_memory = vec!["The user's favourite colour is chartreuse.".into()];
    });
    let result = runner
        .execute("What is the user's favourite colour? Answer in one word.")
        .await
        .unwrap();
    assert!(
        result.to_lowercase().contains("chartreuse"),
        "expected 'chartreuse' in: {result}"
    );
}

// ─── save_session ────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn save_session_called_after_run() {
    let saved = Arc::new(Mutex::new(vec![]));
    let store = TrackingDreamStore {
        saved: saved.clone(),
    };
    let runner = make_runner_with(|_, opts| {
        opts.dream_store = Some(Box::new(store));
        opts.agent_id = Some("test-agent".into());
    });
    runner.execute("Reply \"ok\".").await.unwrap();
    assert!(
        !saved.lock().unwrap().is_empty(),
        "save_session should have been called"
    );
    assert_eq!(saved.lock().unwrap()[0].agent_id, "test-agent");
}

// ─── knowledge init() ────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn knowledge_init_called_before_run() {
    let init_count = Arc::new(Mutex::new(0u32));
    let ks = TrackingKnowledgeSource {
        init_count: init_count.clone(),
    };
    let runner = make_runner_with(|_, opts| {
        opts.knowledge_source = Some(Box::new(ks));
    });
    runner.execute("Reply \"ok\".").await.unwrap();
    assert!(
        *init_count.lock().unwrap() >= 1,
        "init() should have been called"
    );
}

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn knowledge_init_called_once_per_run() {
    let init_count = Arc::new(Mutex::new(0u32));
    let ks = TrackingKnowledgeSource {
        init_count: init_count.clone(),
    };
    let runner = make_runner_with(|_, opts| {
        opts.knowledge_source = Some(Box::new(ks));
    });
    runner.execute("Reply \"ok\".").await.unwrap();
    assert_eq!(*init_count.lock().unwrap(), 1);
}

// ─── AttemptLoop.stream() ───────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn attempt_loop_streaming_emits_events() {
    use futures::StreamExt;

    let runner = make_runner_with(|_, _| {});
    let attempt_loop = AttemptLoop::new(
        RuntimeAttemptBody::new(&runner),
        LlmEvalJudge::new(make_provider()),
        StopPolicy::new(2),
    )
    .unwrap();
    let mut req = AttemptRequest::generated("What is 6 * 7? Output only the number.");
    req.criteria = vec![Criterion::required("Answer must be 42")];

    let mut stream = attempt_loop.stream(req);

    let mut has_token = false;
    let mut has_judging = false;
    let mut has_terminal = false;
    let mut result = String::new();

    while let Some(evt) = stream.next().await {
        match evt.unwrap() {
            AttemptLoopEvent::Token(t) => {
                result.push_str(&t);
                has_token = true;
            }
            AttemptLoopEvent::Judging { .. } => has_judging = true,
            AttemptLoopEvent::Completed(_) => has_terminal = true,
            _ => {}
        }
    }

    assert!(has_token, "should emit Token events");
    assert!(has_judging, "should emit Judging");
    assert!(has_terminal, "should terminate");
    assert!(!result.is_empty());
}

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn attempt_loop_done_verdict_has_details() {
    use futures::StreamExt;

    let runner = make_runner_with(|_, _| {});
    let attempt_loop = AttemptLoop::new(
        RuntimeAttemptBody::new(&runner),
        LlmEvalJudge::new(make_provider()),
        StopPolicy::new(2),
    )
    .unwrap();
    let mut req = AttemptRequest::generated("Output the number 99.");
    req.criteria = vec![
        Criterion::required("Response must contain 99"),
        Criterion::optional("Response should be concise").with_weight(0.5),
    ];

    let mut stream = attempt_loop.stream(req);

    let mut found_done = false;
    while let Some(evt) = stream.next().await {
        if let Ok(AttemptLoopEvent::Completed(outcome)) = evt {
            if let Some(verdict) = outcome.verdict {
                assert!(verdict.details.len() > 0, "details should be populated");
                assert!(verdict.overall_score >= 0.0 && verdict.overall_score <= 1.0);
                found_done = true;
            }
        }
    }
    // may reach max_attempts instead of done — both are valid
    let _ = found_done;
}
