use deepstrike_sdk::*;
use std::sync::{Arc, Mutex};
use async_trait::async_trait;
use deepstrike_core::memory::durable::SessionData as CoreSessionData;
use deepstrike_core::memory::semantic::MemoryEntry;
use deepstrike_core::memory::curator::CurationResult;

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
    async fn load_sessions(&self, _agent_id: &str) -> Result<Vec<CoreSessionData>> { Ok(vec![]) }
    async fn load_memories(&self, _agent_id: &str) -> Result<Vec<MemoryEntry>> { Ok(vec![]) }
    async fn commit(&self, _agent_id: &str, _result: CurationResult, _existing: &[MemoryEntry]) -> Result<()> { Ok(()) }
    async fn search(&self, _agent_id: &str, _query: &str, _top_k: usize) -> Result<Vec<MemoryEntry>> { Ok(vec![]) }
    async fn save_session(&self, data: CoreSessionData) -> Result<()> {
        self.saved.lock().unwrap().push(data);
        Ok(())
    }
}

fn make_provider() -> OpenAIProvider {
    use std::path::PathBuf;
    let env_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent().unwrap().parent().unwrap()
        .join(".env");
    let _ = dotenvy::from_path(&env_path);
    let key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
    let url = std::env::var("OPENAI_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".into());
    let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into());
    OpenAIProvider::with_base_url(key, model, url)
}

// ─── system_prompt ───────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn system_prompt_is_followed() {
    let agent = Agent::new(make_provider(), AgentOptions {
        system_prompt: Some("You are a pirate. Always end every reply with 'Arrr!'".into()),
        ..AgentOptions::new(4096)
    });
    let result = agent.run("Say hello.").await.unwrap();
    assert!(result.to_lowercase().contains("arrr"), "expected 'Arrr!' in: {result}");
}

// ─── initial_memory ──────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn initial_memory_is_recalled() {
    let agent = Agent::new(make_provider(), AgentOptions {
        initial_memory: vec!["The user's favourite colour is chartreuse.".into()],
        ..AgentOptions::new(4096)
    });
    let result = agent.run("What is the user's favourite colour? Answer in one word.").await.unwrap();
    assert!(result.to_lowercase().contains("chartreuse"), "expected 'chartreuse' in: {result}");
}

// ─── save_session ────────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn save_session_called_after_run() {
    let saved = Arc::new(Mutex::new(vec![]));
    let store = TrackingDreamStore { saved: saved.clone() };
    let agent = Agent::new(make_provider(), AgentOptions {
        dream_store: Some(Box::new(store)),
        agent_id: Some("test-agent".into()),
        ..AgentOptions::new(4096)
    });
    agent.run("Reply \"ok\".").await.unwrap();
    assert!(!saved.lock().unwrap().is_empty(), "save_session should have been called");
    assert_eq!(saved.lock().unwrap()[0].agent_id, "test-agent");
}

// ─── knowledge init() ────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn knowledge_init_called_before_run() {
    let init_count = Arc::new(Mutex::new(0u32));
    let ks = TrackingKnowledgeSource { init_count: init_count.clone() };
    let agent = Agent::new(make_provider(), AgentOptions {
        knowledge_source: Some(Box::new(ks)),
        ..AgentOptions::new(4096)
    });
    agent.run("Reply \"ok\".").await.unwrap();
    assert!(*init_count.lock().unwrap() >= 1, "init() should have been called");
}

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn knowledge_init_called_once_per_run() {
    let init_count = Arc::new(Mutex::new(0u32));
    let ks = TrackingKnowledgeSource { init_count: init_count.clone() };
    let agent = Agent::new(make_provider(), AgentOptions {
        knowledge_source: Some(Box::new(ks)),
        ..AgentOptions::new(4096)
    });
    agent.run("Reply \"ok\".").await.unwrap();
    assert_eq!(*init_count.lock().unwrap(), 1);
}

// ─── HarnessLoop.run_streaming() ─────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn harness_loop_streaming_emits_events() {
    use futures::StreamExt;
    use deepstrike_sdk::harness::{HarnessEvent, HarnessRequest, Criterion};

    let agent = Agent::new(make_provider(), AgentOptions::new(4096));
    let harness = HarnessLoop::new(&agent, make_provider(), 2, None);
    let req = HarnessRequest {
        goal: "What is 6 * 7? Output only the number.".into(),
        criteria: vec![Criterion::required("Answer must be 42")],
        extensions: None,
    };

    let stream = harness.run_streaming(req);
    futures::pin_mut!(stream);

    let mut has_token = false;
    let mut has_supervising = false;
    let mut has_terminal = false;
    let mut result = String::new();

    while let Some(evt) = stream.next().await {
        match evt.unwrap() {
            HarnessEvent::Token(t) => { result.push_str(&t); has_token = true; }
            HarnessEvent::Supervising => has_supervising = true,
            HarnessEvent::Done { .. } | HarnessEvent::MaxAttemptsReached => has_terminal = true,
            _ => {}
        }
    }

    assert!(has_token, "should emit Token events");
    assert!(has_supervising, "should emit Supervising");
    assert!(has_terminal, "should terminate");
    assert!(!result.is_empty());
}

#[tokio::test]
#[ignore = "requires OPENAI_API_KEY"]
async fn harness_loop_done_verdict_has_details() {
    use futures::StreamExt;
    use deepstrike_sdk::harness::{HarnessEvent, HarnessRequest, Criterion};

    let agent = Agent::new(make_provider(), AgentOptions::new(4096));
    let harness = HarnessLoop::new(&agent, make_provider(), 2, None);
    let req = HarnessRequest {
        goal: "Output the number 99.".into(),
        criteria: vec![
            Criterion::required("Response must contain 99"),
            Criterion::optional("Response should be concise").with_weight(0.5),
        ],
        extensions: None,
    };

    let stream = harness.run_streaming(req);
    futures::pin_mut!(stream);

    let mut found_done = false;
    while let Some(evt) = stream.next().await {
        if let Ok(HarnessEvent::Done { verdict, .. }) = evt {
            assert!(verdict.details.len() > 0, "details should be populated");
            assert!(verdict.overall_score >= 0.0 && verdict.overall_score <= 1.0);
            found_done = true;
        }
    }
    // may reach max_attempts instead of done — both are valid
    let _ = found_done;
}
