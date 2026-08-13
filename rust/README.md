<p align="center">
  <a href="https://github.com/kongusen/deepstrike">
    <img src="https://raw.githubusercontent.com/kongusen/deepstrike/main/docs/public/banner.png" alt="DeepStrike" width="420" />
  </a>
</p>

# DeepStrike Rust SDK

Build Rust Agents with providers, typed tools, durable sessions, Memory, Skills, Knowledge, governance, and bounded evaluation loops.

Use `RuntimeRunner` when an Agent needs streaming events, durable session recovery, tool control, or host-provided integrations.

## Add to your project

```toml
[dependencies]
deepstrike-sdk = "0.2"
tokio = { version = "1", features = ["full"] }
futures = "0.3"
serde_json = "1"
```

---

## Quick start

```rust
use std::sync::Arc;
use deepstrike_sdk::{
    InMemorySessionLog, LocalExecutionPlane, MilestonePolicy,
    OpenAIProvider, RegisteredTool, ResourceQuota, RuntimeOptions, RuntimeRunner,
};

#[tokio::main]
async fn main() {
    let provider = OpenAIProvider::with_base_url("sk-...", "gpt-5-mini", "https://api.openai.com/v1");
    let mut plane = LocalExecutionPlane::new();
    plane.register(RegisteredTool::text(
        "add", "Add two numbers.",
        serde_json::json!({"type":"object","properties":{"x":{"type":"integer"},"y":{"type":"integer"}},"required":["x","y"]}),
        |args| Box::pin(async move {
            Ok(format!("{}", args["x"].as_i64().unwrap() + args["y"].as_i64().unwrap()))
        }),
    ));

    let runner = RuntimeRunner::new(RuntimeOptions {
        provider: Box::new(provider),
        execution_plane: Some(Box::new(plane)),
        session_log: Some(Arc::new(InMemorySessionLog::new())),
        compression_store: None,
        payload_store: None,
        kernel_reliability: None,
        session_id: None,
        max_tokens: 32_000,
        max_turns: Some(10),
        timeout_ms: None,
        extensions: None,
        agent_id: None,
        memory_scope: None,
        pre_query_memory: None,
        system_prompt: None,
        initial_memory: vec![],
        skill_dir: None,
        memory_store: None,
        knowledge_source: None,
        signal_source: None,
        governance: None,
        os_profile: None,
        governance_policy: None,
        signal_policy: None,
        scheduler_policy: None,
        resource_quota: Some(ResourceQuota {
            max_concurrent_subagents: Some(4),
            max_total_subagents: None,
            max_spawn_depth: Some(2),
            memory_writes_per_window: Some((20, 60_000)),
            max_workflow_nodes: None,
        }),
        memory_policy: None,
        tokenizer: None,
        enable_plan_tool: None,
        on_tool_suspend: None,
        on_permission_request: None,
        milestone_policy: MilestonePolicy::RequireVerifier,
        milestone_contract: None,
        run_spec: None,
        allowed_tool_ids: None,
        baseline_tool_ids: None,
        tool_dispatch_gate: None,
        on_turn_metrics: None,
        stable_core_tool_ids: vec![],
        on_milestone_evaluate: None,
    });

    // Same session_id → prior turns are replayed from SessionLog
    // runner.wake("session-1").await?;  // resume mid-run after crash

    let text = runner.execute("What is 17 + 28?").await.unwrap();
    println!("{text}");
}
```

Streaming via `RuntimeRunner::run_streaming`:

```rust
use deepstrike_sdk::{RunEvent, RuntimeRunner};
use futures::StreamExt;

let mut stream = runner.run_streaming("Summarize README.md", &[], None, None).await?;
while let Some(evt) = stream.next().await {
    match evt? {
        RunEvent::TextDelta(d) => print!("{d}"),
        RunEvent::ToolCall { name, .. } => println!("\n[→ {name}]"),
        RunEvent::ToolResult { content, .. } => println!("  = {content}"),
        RunEvent::Done { iterations, status, .. } => println!("\ndone in {iterations} turns ({status})"),
        _ => {}
    }
}
```

---

## Providers

| Constructor | Backend |
|-------------|---------|
| `OpenAIProvider::new(api_key)` | OpenAI API |
| `OpenAIProvider::with_base_url(key, model, url)` | Any OpenAI-compatible endpoint |
| `AnthropicProvider::new(api_key)` | Anthropic API |
| `qwen(api_key)` | DashScope (通义千问) |
| `deepseek(api_key)` | DeepSeek API |
| `minimax(api_key)` | MiniMax API |
| `ollama(model)` | Local Ollama |
| `kimi(api_key)` | Moonshot Kimi |

Custom providers: implement the `LLMProvider` trait.

---

## Context model (four slots)

| Slot | Source | Role |
|------|--------|------|
| `system_stable` | system partition | Identity — never changes within a run |
| `system_knowledge` | knowledge partition | Preloaded memory — low frequency |
| `turns[0]` | `task_state` + signals | Goal, plan, compression log, runtime signals |
| `turns[1..N]` | history | Conversation — **sole compression target** |

Set `system_prompt` for stable instructions and `initial_memory` for durable preloaded context in the complete `RuntimeOptions` value above.

See [docs/concepts/context-slots-compression.md](../docs/concepts/context-slots-compression.md).

---

## RuntimeOptions

The most frequently configured fields are `provider`, `execution_plane`, `session_log`,
`max_tokens`, `max_turns`, `skill_dir`, `knowledge_source`, `memory_store`, `agent_id`,
`resource_quota`, `memory_policy`, `governance`, and `signal_source`. Construct the complete
`RuntimeOptions` value as in the quick start, then set the fields that fit your Agent.

---

## Tools

```rust
use deepstrike_sdk::{RegisteredTool, read_file_tool, Governance};

let mut plane = LocalExecutionPlane::new();
plane.register(RegisteredTool::text("search", "Search.", schema, |args| Box::pin(async move { ... })));
plane.register(read_file_tool());
plane.unregister("search");

let mut gov = Governance::allow();
gov.block_tool("bash");
```

---

## Skills

Set `skill_dir` — the kernel auto-injects a `skill` meta-tool, and the LLM loads skills by name on demand.

Set `skill_dir: Some("./skills".into())` in the complete `RuntimeOptions` value. The Agent can then load Skills by name on demand.

---

## Knowledge

Implement `KnowledgeSource` — the kernel injects a `knowledge` meta-tool. Runtime retrieval → **history**; durable preload → Slot 2 via `initial_memory`.

```rust
use async_trait::async_trait;

struct VectorSearch;

#[async_trait]
impl KnowledgeSource for VectorSearch {
    async fn retrieve(&self, query: &str, top_k: usize) -> deepstrike_sdk::Result<Vec<String>> {
        Ok(vector_db.search(query, top_k).await)
    }

    async fn init(&self) -> deepstrike_sdk::Result<()> {
        Ok(())
    }
}
```

---

## Memory

### WorkingMemory (SDK-side scratch pad)

SDK helper — not the removed kernel `working` partition.

```rust
use deepstrike_sdk::WorkingMemory;

let mut mem = WorkingMemory::default();
mem.set("step", 1);
mem.get("step");  // Some(&json!(1))
mem.clear();
```

### MemoryStore (durable long-term memory)

```rust
#[async_trait]
impl MemoryStore for MyStore {
    async fn put(&self, agent_id: &str, record: MemoryRecord) -> Result<()> { ... }
    async fn get(&self, agent_id: &str, record_id: &str) -> Result<Option<MemoryRecord>> { ... }
    async fn delete(&self, agent_id: &str, record_id: &str) -> Result<()> { ... }
    async fn search(&self, agent_id: &str, query: &MemoryQuery) -> Result<Vec<MemoryRecall>> { ... }
    async fn save_session(&self, data: SessionData) -> Result<()> { ... }
}

// In-session: memory(query) → history tool result
// Preload:    initial_memory → Slot 2
// Post-session: the runner saves the transcript and extracts durable records.
```

---

## Governance

### SDK PermissionManager

```rust
use deepstrike_sdk::{PermissionManager, PermissionMode};

let mut pm = PermissionManager::new(PermissionMode::Default);
pm.grant("fs", "read");
pm.revoke("db", "drop");
pm.grant_with_approval("db", "write", "Needs DBA approval");
```

### Kernel GovernancePipeline

```rust
use deepstrike_core::governance::pipeline::GovernancePipeline;
use deepstrike_core::governance::permission::{PermissionAction, PermissionRule};

let mut pipeline = GovernancePipeline::new(PermissionAction::Allow);
pipeline.permission.add_rule(PermissionRule { tool_pattern: "danger.*".into(), action: PermissionAction::Deny });
pipeline.veto.block_tool("rm_rf");
pipeline.rate_limiter.set_limit("api", RateLimit { max_calls: 10, window_ms: 60_000 });
// Permission → Veto → RateLimit → Constraint → Audit
```

---

## Signals

Provide a `SignalSource` through `signal_source` in the complete `RuntimeOptions` value. Call
`runner.interrupt()` when the application needs to stop the active run immediately.

---

## Harness (evaluation framework)

```rust
use deepstrike_sdk::*;

let body = RuntimeAttemptBody::new(&runner);
let judge = LlmEvalJudge::new(eval_provider);
let attempt_loop = AttemptLoop::new(body, judge, StopPolicy::new(3))?;

let mut request = AttemptRequest::generated("Write a haiku");
request.criteria = vec![Criterion::required("Must be 3 lines")];
let outcome = attempt_loop.run(request).await?;
println!("{:?} {}", outcome.outcome, outcome.run_status);
```

---

## Stream events

| Variant | Fields |
|---------|--------|
| `TextDelta(String)` | text chunk |
| `ThinkingDelta(String)` | reasoning chunk |
| `ToolCall { id, name }` | tool invoked |
| `ToolResult { call_id, content, is_error }` | tool output |
| `Done { iterations, total_tokens, status }` | run complete |
| `Error(String)` | non-fatal error |

`status`: `completed` · `max_turns` · `token_budget` · `timeout` · `user_abort` · `error`
