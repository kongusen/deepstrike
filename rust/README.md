# DeepStrike Rust SDK

Agent framework built on `deepstrike-core`. The kernel handles loop control, context compression, skill selection, and termination — the SDK handles all I/O.

## Add to your project

```toml
[dependencies]
deepstrike-sdk = { path = "../deepstrike-sdk" }
tokio = { version = "1", features = ["full"] }
```

---

## Quick start

```rust
use deepstrike_sdk::{Agent, AgentOptions, AnthropicProvider};

#[tokio::main]
async fn main() {
    let provider = AnthropicProvider::new("sk-...");
    let agent = Agent::new(provider, AgentOptions::new(32_000));
    let result = agent.run("What is 2 + 3?").await.unwrap();
    println!("{result}");
}
```

Streaming:

```rust
use deepstrike_sdk::{Agent, AgentOptions, AnthropicProvider, RunEvent};
use futures::StreamExt;

let mut stream = agent.run_streaming("Summarize README.md", &[], None).await?;
while let Some(evt) = stream.next().await {
    match evt? {
        RunEvent::TextDelta(d)           => print!("{d}"),
        RunEvent::ToolCall { name, .. }  => println!("\n[→ {name}]"),
        RunEvent::Done { iterations, status, .. } =>
            println!("\ndone in {iterations} turns ({status})"),
        _ => {}
    }
}
```

---

## Architecture

```
crates/deepstrike-sdk/src/
├── lib.rs          # Public re-exports
├── agent.rs        # Agent + SinglePassHarness + EvalLoopHarness
├── providers/      # LLMProvider trait + Anthropic/OpenAI impls
├── tools.rs        # RegisteredTool, execute_tools, read_file_tool
├── memory.rs       # WorkingMemory + MemorySource/Extractor traits
├── knowledge.rs    # KnowledgeSource trait
├── harness.rs      # Harness, HarnessRequest, HarnessOutcome, QualityGate
├── signals.rs      # RuntimeSignal, SignalSource, ScheduledPrompt
└── safety.rs       # PermissionManager + PermissionMode
```

The kernel (`deepstrike-core`) owns:
- `LoopStateMachine` — drives `CallLLM → ExecuteTools → LoadSkills → Done`
- `ContextManager` — 5-partition context with pressure-based compression
- `GovernancePipeline` — tool veto authority
- `SignalRouter` — external interrupt queue

---

## Providers

| Constructor | Backend |
|-------------|---------|
| `AnthropicProvider::new(api_key)` | Anthropic API (SSE) |
| `AnthropicProvider::with_model(api_key, model)` | Anthropic, custom model |
| `OpenAIProvider::new(api_key)` | OpenAI API |
| `OpenAIProvider::with_base_url(key, model, url)` | Any OpenAI-compatible endpoint |
| `qwen(api_key)` | DashScope |
| `deepseek(api_key)` | DeepSeek API |
| `minimax(api_key)` | MiniMax API |
| `ollama(model)` | Local Ollama (`http://localhost:11434`) |

```rust
use deepstrike_sdk::providers::anthropic::AnthropicProvider;

let provider = AnthropicProvider::with_model("sk-...", "claude-opus-4-7");
```

Thinking / reasoning:

```rust
use serde_json::json;

let ext = json!({ "enable_thinking": true });
let mut stream = agent.run_streaming("...", &[], Some(&ext)).await?;
while let Some(evt) = stream.next().await {
    if let RunEvent::ThinkingDelta(d) = evt? { print!("{d}") }
}
```

---

## Tools

```rust
use deepstrike_sdk::{Agent, AgentOptions, RegisteredTool};

let search = RegisteredTool::new(
    "search",
    "Search the knowledge base.",
    serde_json::json!({
        "type": "object",
        "properties": { "query": { "type": "string" } },
        "required": ["query"],
    }),
    |args| Box::pin(async move {
        let query = args["query"].as_str().unwrap_or("").to_string();
        Ok(my_search(&query).await)
    }),
);

let mut agent = Agent::new(provider, AgentOptions::new(32_000));
agent.register(search);
agent.unregister("search");
agent.block_tool("bash");
```

Built-in tools: `read_file_tool()`.

---

## Skills

Skills are `.md` files with YAML frontmatter. The kernel selects them automatically; the SDK loads them from disk.

```markdown
---
name: debug
description: Step-by-step debugging guide
when_to_use: error, traceback, exception
effort: 2
estimated_tokens: 800
---

## Debug protocol
1. Read the traceback carefully ...
```

The SDK reads skill files from the path `{name}.md` relative to the working directory when the kernel requests `LoadSkills`.

---

## Memory

Implement `MemorySource` to inject persistent context before a run, and `MemoryExtractor` to persist what was learned after.

```rust
use async_trait::async_trait;
use deepstrike_sdk::{MemorySource, MemoryExtractor, AgentOptions};

struct MyMemory;

#[async_trait]
impl MemorySource for MyMemory {
    async fn load(&self, goal: &str) -> deepstrike_sdk::Result<Vec<String>> {
        Ok(db.query(goal).await)
    }
}

#[async_trait]
impl MemoryExtractor for MyMemory {
    async fn extract(&self, goal: &str, final_text: &str, turns: u32) -> deepstrike_sdk::Result<()> {
        db.save(goal, final_text).await;
        Ok(())
    }
}

let mut options = AgentOptions::new(32_000);
options.memory_source = Some(Box::new(MyMemory));
options.memory_extractor = Some(Box::new(MyMemory));
```

`WorkingMemory` is an in-process scratch pad for within-run state:

```rust
use deepstrike_sdk::WorkingMemory;

let mut mem = WorkingMemory::default();
mem.set("step", 1);
mem.get("step"); // Some(1)
```

---

## Knowledge

```rust
use async_trait::async_trait;
use deepstrike_sdk::{KnowledgeSource, AgentOptions};

struct VectorSearch;

#[async_trait]
impl KnowledgeSource for VectorSearch {
    async fn retrieve(&self, goal: &str, top_k: usize) -> deepstrike_sdk::Result<Vec<String>> {
        Ok(vector_db.search(goal, top_k).await)
    }
}

let mut options = AgentOptions::new(32_000);
options.knowledge_source = Some(Box::new(VectorSearch));
```

---

## Harness

```rust
use deepstrike_sdk::{Agent, AgentOptions, SinglePassHarness, EvalLoopHarness};
use deepstrike_sdk::harness::{HarnessRequest, HarnessOutcome, QualityGate};
use async_trait::async_trait;

// Single pass
let harness = SinglePassHarness::new(&agent);
let outcome = harness.run(HarnessRequest::new("Write a haiku")).await?;

// Eval loop — retry until QualityGate passes (max 3 attempts)
struct LengthGate;

#[async_trait]
impl QualityGate for LengthGate {
    async fn evaluate(&self, _req: &HarnessRequest, outcome: &HarnessOutcome) -> deepstrike_sdk::Result<bool> {
        Ok(outcome.result.len() > 50)
    }
}

let harness = EvalLoopHarness::new(&agent, LengthGate, 3);
let outcome = harness.run(HarnessRequest::new("Write a haiku")).await?;
println!("{} in  turns", outcome.passed, outcome.iterations);
```

---

## Signals & interrupts

```rust
use deepstrike_sdk::{ScheduledPrompt, AgentOptions};
use deepstrike_sdk::signals::{RuntimeSignal, SignalSource};
use async_trait::async_trait;

// Interrupt from another thread
let agent = std::sync::Arc::new(agent);
let agent_clone = agent.clone();
tokio::spawn(async move {
    tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    agent_clone.interrupt();
});

// Convert a scheduled prompt to a RuntimeSignal
let prompt = ScheduledPrompt::new("Daily standup summary", 1_700_000_000_000);
let signal = prompt.to_signal();
// signal.kind == "scheduled"

// Feed signals from any external source
struct WebhookSource;

#[async_trait]
impl SignalSource for WebhookSource {
    async fn next_signal(&self) -> deepstrike_sdk::Result<Option<RuntimeSignal>> {
        Ok(webhook_queue.try_recv().ok())
    }
}
```

---

## Permissions

```rust
use deepstrike_sdk::{PermissionManager, PermissionMode};

let mut pm = PermissionManager::new(PermissionMode::Default);
pm.grant("fs", "read");
pm.grant("fs", "*");       // wildcard: all actions on fs
pm.revoke("fs", "read");

let decision = pm.evaluate("fs", "read");
decision.allowed  // bool
decision.reason   // &'static str
```

Modes: `Default` (evaluate grants), `Plan` (block all), `Auto` (allow all).

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

`status` mirrors the kernel termination reason: `completed` / `max_turns` / `token_budget` / `timeout` / `user_abort` / `error`.
