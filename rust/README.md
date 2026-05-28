# DeepStrike Rust SDK

Runtime framework built on `deepstrike-core`. The kernel handles loop control, context compression, skill routing, governance, signal prioritization — the SDK handles all I/O.

> **Runtime v1:** Use `RuntimeRunner` + `SessionLog` + `LocalExecutionPlane` (same model as Node/Python/WASM).

## Add to your project

```toml
[dependencies]
deepstrike-sdk = "0.1"
tokio = { version = "1", features = ["full"] }
futures = "0.3"
```

---

## Quick start

```rust
use std::sync::Arc;
use deepstrike_sdk::{
    InMemorySessionLog, LocalExecutionPlane, OpenAIProvider,
    RegisteredTool, RuntimeOptions, RuntimeRunner,
};

#[tokio::main]
async fn main() {
    let provider = OpenAIProvider::with_base_url("sk-...", "gpt-5-mini", "https://api.openai.com/v1");
    let plane = LocalExecutionPlane::new();
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
        session_id: None,
        max_tokens: 32_000,
        max_turns: Some(10),
        timeout_ms: None,
        extensions: None,
        agent_id: None,
        system_prompt: None,
        initial_memory: vec![],
        skill_dir: None,
        dream_store: None,
        knowledge_source: None,
        signal_source: None,
        governance: None,
        on_tool_suspend: None,
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

```rust
RuntimeOptions {
    system_prompt: Some("You are helpful.".into()),           // Slot 1
    initial_memory: vec!["User prefers chartreuse.".into()], // Slot 2
    // ...
}
```

See [docs/context-partition-compression.md](../docs/context-partition-compression.md).

---

## RuntimeOptions

```rust
RuntimeOptions {
    provider: Box::new(provider),
    execution_plane: Some(Box::new(plane)),
    session_log: Some(Arc::new(InMemorySessionLog::new())),
    max_tokens: 4096,
    max_turns: Some(25),
    timeout_ms: Some(60_000),
    extensions: Some(json!({"temperature": 0.1})),
    skill_dir: Some("./skills".into()),
    knowledge_source: Some(Box::new(my_ks)),
    signal_source: Some(Box::new(rx)),
    dream_store: Some(Box::new(my_store)),
    agent_id: Some("my-agent".into()),
    initial_memory: vec![],     // preloaded blocks → Slot 2
    governance: None,
    on_tool_suspend: None,
    // ...
}
```

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

```rust
let runner = RuntimeRunner::new(RuntimeOptions {
    skill_dir: Some("./skills".into()),
    max_tokens: 4096,
    /* provider, execution_plane, session_log, ... */
});
```

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

### DreamStore (long-term memory + dreaming pipeline)

```rust
#[async_trait]
impl DreamStore for MyStore {
    async fn load_sessions(&self, agent_id: &str) -> Result<Vec<SessionData>> { ... }
    async fn load_memories(&self, agent_id: &str) -> Result<Vec<MemoryEntry>> { ... }
    async fn commit(&self, agent_id: &str, result: CurationResult, existing: &[MemoryEntry]) -> Result<()> { ... }
    async fn search(&self, agent_id: &str, query: &str, top_k: usize) -> Result<Vec<MemoryEntry>> { ... }
}

// In-session: memory(query) → history tool result
// Preload:    initial_memory → Slot 2
// Post-session:
let result = runner.dream("my-agent", now_ms).await?;
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

```rust
use deepstrike_sdk::{SignalGateway, ScheduledPrompt, RuntimeSignal};

let gw = SignalGateway::new();
let rx = gw.subscribe();

gw.schedule(ScheduledPrompt::new("standup", target_ms));
gw.ingest(RuntimeSignal { kind: "interrupt".into(), payload: json!({}), priority: 10 });

let runner = RuntimeRunner::new(RuntimeOptions {
    signal_source: Some(Box::new(rx)),
    max_tokens: 4096,
    /* ... */
});

runner.interrupt(); // direct interrupt
gw.destroy();
```

---

## Harness (evaluation framework)

```rust
use deepstrike_sdk::*;

// 1. SinglePass — run once, always passes
let outcome = SinglePassHarness::new(&runner).run(HarnessRequest::new("Say hello")).await?;

// 2. EvalLoop — retry until QualityGate passes
let harness = EvalLoopHarness::new(&runner, MyGate, 3);

// 3. HarnessLoop — LLM-as-judge with feedback injection + skill extraction
let harness = HarnessLoop::new(&runner, eval_provider, 3, Some("./skills".into()));
let outcome = harness.run(HarnessRequest { goal: "Write a haiku".into(), criteria: vec!["Must be 3 lines".into()], .. }).await?;
println!("{} {}", outcome.passed, outcome.feedback.unwrap_or_default());
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
