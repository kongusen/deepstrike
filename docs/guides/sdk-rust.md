# DeepStrike Rust SDK — API 使用指南

## 目录

1. [快速开始](#1-快速开始)
2. [Provider 配置](#2-provider-配置)
3. [RuntimeRunner 基础](#3-runtimerunner-基础)
4. [工具调用 (Tools)](#4-工具调用-tools)
5. [技能 (Skills)](#5-技能-skills)
6. [知识检索 (Knowledge)](#6-知识检索-knowledge)
7. [记忆系统 (Memory)](#7-记忆系统-memory)
8. [治理管线 (Governance)](#8-治理管线-governance)
9. [信号系统 (Signals)](#9-信号系统-signals)
10. [评估框架 (Harness)](#10-评估框架-harness)
11. [内核层直接使用](#11-内核层直接使用)

---

## 1. 快速开始

```toml
# Cargo.toml
[dependencies]
deepstrike-sdk  = { path = "rust" }          # SDK 层（RuntimeRunner, Provider, Tools）
deepstrike-core = { path = "crates/deepstrike-core" }  # 内核层（可选，直接访问状态机）
tokio = { version = "1", features = ["full"] }
futures = "0.3"
serde_json = "1"
```

```rust
use std::sync::Arc;
use deepstrike_sdk::{
    InMemorySessionLog, LocalExecutionPlane, OpenAIProvider,
    RuntimeOptions, RuntimeRunner, collect_text,
};

#[tokio::main]
async fn main() {
    let provider = OpenAIProvider::with_base_url(
        "sk-your-api-key",
        "gpt-5-mini",
        "https://api.openai.com/v1",
    );

    let runner = RuntimeRunner::new(RuntimeOptions {
        provider: Box::new(provider),
        execution_plane: Some(Box::new(LocalExecutionPlane::new())),
        session_log: Some(Arc::new(InMemorySessionLog::new())),
        session_id: None,
        max_tokens: 4096,
        max_turns: Some(25),
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

    let text = collect_text(
        runner.run_streaming("用一句话解释什么是 Rust", &[], None, None).await.unwrap(),
    )
    .await
    .unwrap();
    println!("{text}");
}
```

---

## 2. Provider 配置

SDK 提供 OpenAI 兼容 Provider 和多个快捷工厂函数：

```rust
use deepstrike_sdk::OpenAIProvider;
use deepstrike_sdk::providers::openai::{qwen, deepseek, minimax, ollama, kimi};

// OpenAI / 兼容代理
let provider = OpenAIProvider::with_base_url("key", "gpt-5-mini", "https://xiaoai.plus/v1");

// 快捷构造
let qwen_provider     = qwen("your-key");       // 通义千问
let deepseek_provider = deepseek("your-key");    // DeepSeek
let minimax_provider  = minimax("your-key");     // MiniMax
let ollama_provider   = ollama("llama3");        // 本地 Ollama（无需 key）
let kimi_provider     = kimi("your-key");        // Moonshot Kimi
```

### 自定义 Provider

实现 `LLMProvider` trait：

```rust
use async_trait::async_trait;
use deepstrike_sdk::providers::{LLMProvider, StreamEvent};
use deepstrike_core::types::message::{Message, ToolSchema};

struct MyProvider;

#[async_trait]
impl LLMProvider for MyProvider {
    async fn stream(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        extensions: Option<&serde_json::Value>,
    ) -> deepstrike_sdk::Result<Box<dyn futures::Stream<Item = deepstrike_sdk::Result<StreamEvent>> + Send + Unpin>> {
        // 实现流式调用...
        todo!()
    }
}
```

---

## 3. RuntimeRunner 基础

### 3.1 同步运行

```rust
let text = runner.execute("Say hello").await.unwrap();
assert!(!text.is_empty());
```

### 3.2 流式运行

```rust
use deepstrike_sdk::RunEvent;
use futures::StreamExt;

let mut stream = runner.run_streaming("What is 2+2?", &[], None, None).await.unwrap();
let mut text = String::new();

while let Some(evt) = stream.next().await {
    match evt.unwrap() {
        RunEvent::TextDelta(delta) => {
            print!("{delta}");
            text.push_str(&delta);
        }
        RunEvent::Done { iterations, total_tokens, status } => {
            println!("\n--- done: {iterations} turns, {total_tokens} tokens, {status}");
        }
        _ => {}
    }
}
```

### 3.3 带 Criteria 运行

```rust
let criteria = vec!["必须包含 hello".to_string(), "不超过 20 字".to_string()];
let mut stream = runner.run_streaming("打个招呼", &criteria, None, None).await.unwrap();
```

### 3.4 Extensions（温度等参数透传）

```rust
let runner = RuntimeRunner::new(RuntimeOptions {
    provider: Box::new(provider),
    execution_plane: Some(Box::new(LocalExecutionPlane::new())),
    session_log: Some(Arc::new(InMemorySessionLog::new())),
    extensions: Some(serde_json::json!({"temperature": 0.1, "top_p": 0.9})),
    max_tokens: 4096,
    max_turns: Some(25),
    timeout_ms: None,
    session_id: None,
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
```

### 3.5 中断

```rust
use std::sync::Arc;

let runner = Arc::new(/* RuntimeRunner::new(...) */);

let r2 = runner.clone();
tokio::spawn(async move {
    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
    r2.interrupt();
});
```

### 3.6 RuntimeOptions 主要字段

```rust
RuntimeOptions {
    provider: Box::new(provider),
    execution_plane: Some(Box::new(plane)),
    session_log: Some(Arc::new(InMemorySessionLog::new())),
    session_id: None,
    max_tokens: 4096,
    max_turns: Some(25),
    timeout_ms: Some(60_000),
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
}
```

---

## 4. 工具调用 (Tools)

### 4.1 注册工具

```rust
use deepstrike_sdk::RegisteredTool;

let add_tool = RegisteredTool::new(
    "add",
    "Add two integers and return the sum.",
    serde_json::json!({
        "type": "object",
        "properties": {
            "x": { "type": "integer", "description": "First number" },
            "y": { "type": "integer", "description": "Second number" }
        },
        "required": ["x", "y"]
    }),
    |args| Box::pin(async move {
        let x = args["x"].as_i64().unwrap_or(0);
        let y = args["y"].as_i64().unwrap_or(0);
        Ok(format!("{}", x + y))
    }),
);

let mut plane = LocalExecutionPlane::new();
plane.register(add_tool);
let runner = RuntimeRunner::new(/* RuntimeOptions { execution_plane: Some(Box::new(plane)), ... } */);
```

### 4.2 内置工具

```rust
use deepstrike_sdk::read_file_tool;

plane.register(read_file_tool()); // 读文件工具
```

### 4.3 手动执行工具

```rust
use deepstrike_sdk::execute_tools;
use deepstrike_core::types::message::ToolCall;
use compact_str::CompactString;

let call = ToolCall {
    id: CompactString::new("c1"),
    name: CompactString::new("add"),
    arguments: serde_json::json!({"x": 3, "y": 7}),
};
let results = execute_tools(&[call], &registry).await;
assert_eq!(results[0].output.as_text(), Some("10"));
```

### 4.4 屏蔽工具

```rust
// 通过 Governance::block_tool 屏蔽（见治理章节）
gov.block_tool("dangerous_tool");
```

---

## 5. 技能 (Skills)

技能是 Markdown 文件，带 YAML 前置数据：

```markdown
---
name: summarize
description: Summarize text into 2-3 concise bullet points
when_to_use: When you need to condense long text into key points
effort: 1
estimated_tokens: 200
---

To summarize text effectively:
1. Identify the 2-3 most important points
2. Express each as a concise bullet starting with "•"
```

### 使用方式

```rust
let runner = RuntimeRunner::new(RuntimeOptions {
    skill_dir: Some(std::path::PathBuf::from("./skills")),
    max_tokens: 4096,
    /* provider, execution_plane, session_log, ... */
});
// 内核自动注入 `skill` meta-tool，LLM 可按名称加载技能
```

流式事件中会看到：

```rust
RunEvent::ToolCall { id, name: "skill".into() }  // LLM 请求加载技能
RunEvent::ToolResult { call_id, content, .. }     // 技能内容返回
```

---

## 6. 知识检索 (Knowledge)

内核采用**四槽模型**（见 [context-slots-compression.md](../concepts/context-slots-compression.md)）。`knowledge(query)` 结果进入 history；`initial_memory` 预加载到 Slot 2。

### 6.1 实现 KnowledgeSource

```rust
use async_trait::async_trait;
use deepstrike_sdk::KnowledgeSource;

struct MyKnowledge {
    documents: Vec<String>,
}

#[async_trait]
impl KnowledgeSource for MyKnowledge {
    async fn retrieve(&self, query: &str, top_k: usize) -> deepstrike_sdk::Result<Vec<String>> {
        // 实际场景：向量搜索、Elasticsearch 等
        Ok(self.documents.iter().take(top_k).cloned().collect())
    }
}
```

### 6.2 配置 RuntimeRunner

```rust
let runner = RuntimeRunner::new(RuntimeOptions {
    knowledge_source: Some(Box::new(MyKnowledge {
        documents: vec!["DeepStrike 是一个 agent 运行时框架。".into()],
    })),
    max_tokens: 4096,
    /* ... */
});
// 内核注入 `knowledge` meta-tool，LLM 按需调用检索
```

---

## 7. 记忆系统 (Memory)

### 7.1 WorkingMemory（SDK 侧临时存储）

SDK 辅助类型，**不是**内核已删除的 `working` 分区。

```rust
use deepstrike_sdk::WorkingMemory;

let mut mem = WorkingMemory::default();
mem.set("user_name", "Alice");
mem.set("step", 3);
assert_eq!(mem.get("step"), Some(&serde_json::json!(3)));
mem.clear();
```

### 7.2 DreamStore（长期记忆）

实现 `DreamStore` trait 以支持记忆持久化和 `RuntimeRunner::dream()` 管线：

```rust
use async_trait::async_trait;
use deepstrike_sdk::{DreamStore, DreamResult};
use deepstrike_core::memory::{curator::CurationResult, durable::SessionData, semantic::MemoryEntry};

struct PostgresStore { /* ... */ }

#[async_trait]
impl DreamStore for PostgresStore {
    async fn load_sessions(&self, agent_id: &str) -> deepstrike_sdk::Result<Vec<SessionData>> { todo!() }
    async fn load_memories(&self, agent_id: &str) -> deepstrike_sdk::Result<Vec<MemoryEntry>> { todo!() }
    async fn commit(&self, agent_id: &str, result: CurationResult, existing: &[MemoryEntry]) -> deepstrike_sdk::Result<()> { todo!() }
    async fn search(&self, agent_id: &str, query: &str, top_k: usize) -> deepstrike_sdk::Result<Vec<MemoryEntry>> { todo!() }
}
```

### 7.3 启用记忆检索 + dream 管线

```rust
let runner = RuntimeRunner::new(RuntimeOptions {
    dream_store: Some(Box::new(my_store)),
    agent_id: Some("my-agent-1".into()),
    initial_memory: vec!["Prior: user prefers RS256.".into()],  // Slot 2
    max_tokens: 4096,
    /* ... */
});

// 会话中：memory(query) → history tool result；initial_memory → Slot 2
// 空闲时：触发记忆整合
let dream_result = runner.dream("my-agent-1", now_ms).await?;
println!("处理 {} 个 session，提取 {} 条洞察",
    dream_result.sessions_processed,
    dream_result.insights_extracted,
);
```

---

## 8. 治理管线 (Governance)

### 8.1 SDK 层 PermissionManager

```rust
use deepstrike_sdk::{PermissionManager, PermissionMode};

let mut pm = PermissionManager::new(PermissionMode::Default);
pm.grant("fs", "read");                              // 允许 fs:read
pm.grant("fs", "*");                                 // 允许 fs 的所有操作
pm.revoke("db", "drop");                             // 显式拒绝
pm.grant_with_approval("db", "write", "需要 DBA 审批"); // 需要人工审批

let decision = pm.evaluate("fs", "read");
assert!(decision.allowed);

// 模式
// PermissionMode::Auto  — 自动允许所有
// PermissionMode::Plan  — 阻止所有执行
// PermissionMode::Default — 按规则评估
```

### 8.2 内核层 GovernancePipeline

```rust
use deepstrike_core::governance::pipeline::GovernancePipeline;
use deepstrike_core::governance::permission::{PermissionAction, PermissionRule};
use deepstrike_core::governance::rate_limit::RateLimit;

let mut pipeline = GovernancePipeline::new(PermissionAction::Allow);

// 权限规则（模式匹配）
pipeline.permission.add_rule(PermissionRule {
    tool_pattern: "danger.*".into(),
    action: PermissionAction::Deny,
});

// Veto 硬阻断
pipeline.veto.block_tool("rm_rf");
pipeline.veto.add_check(|call, _caller| {
    if call.name.as_str().contains("eval") {
        Some("eval is forbidden".into())
    } else {
        None
    }
});

// 频率限制
pipeline.rate_limiter.set_limit("api_call", RateLimit {
    max_calls: 10,
    window_ms: 60_000,
});

// 评估（必须先 set_time）
pipeline.set_time(now_ms);
let verdict = pipeline.evaluate(&tool_call, &caller_context);
// GovernanceVerdict::Allow | Deny { stage, reason } | RateLimited { retry_after_ms } | AskUser { reason }
```

**管线执行顺序**：Permission → Veto → RateLimit → Constraint，任一阶段 deny 即终止。

### 8.3 工具级屏蔽

```rust
let gov = Arc::new(tokio::sync::Mutex::new(Governance::allow()));
gov.lock().await.block_tool("dangerous_tool");
// RuntimeOptions.governance = Some(gov)
// LLM 调用被屏蔽的工具 → 返回 RunEvent::Error
```

---

## 9. 信号系统 (Signals)

### 9.1 SignalGateway

```rust
use deepstrike_sdk::{SignalGateway, ScheduledPrompt, RuntimeSignal};

let gw = SignalGateway::new();

// 订阅
let rx = gw.subscribe(); // 实现了 SignalSource trait

// 定时调度
gw.schedule(ScheduledPrompt::new("daily standup", run_at_ms));

// 外部注入（Webhook 等）
gw.ingest(RuntimeSignal {
    kind: "interrupt".into(),
    payload: serde_json::json!({"reason": "user request"}),
    priority: 10,
});

// 取消调度
gw.cancel("daily standup", run_at_ms);

// 清理
gw.destroy();
```

### 9.2 RuntimeRunner 集成

```rust
let gw = SignalGateway::new();
let rx = gw.subscribe();

let runner = RuntimeRunner::new(RuntimeOptions {
    signal_source: Some(Box::new(rx)),
    max_tokens: 4096,
    /* ... */
});
// kind="interrupt" 的信号 → 立即中断运行
```

### 9.3 内核 SignalRouter

```rust
use deepstrike_core::signals::router::SignalRouter;
use deepstrike_core::types::signal::{RuntimeSignal, SignalSource, SignalType, Urgency};
use deepstrike_core::types::policy::SignalDisposition;

let mut router = SignalRouter::new(256); // 队列容量

let sig = RuntimeSignal::new(SignalSource::Cron, SignalType::Event, Urgency::Normal, "tick")
    .with_dedupe("cron-tick-1")
    .with_payload(serde_json::json!({"run": 42}));

match router.ingest(sig, /* is_running */ false) {
    SignalDisposition::Queue => { /* 已入队 */ }
    SignalDisposition::InterruptNow => { /* 中断 */ }
    SignalDisposition::Ignore => { /* 去重过滤 */ }
    SignalDisposition::Dropped => { /* 队列满 */ }
    _ => {}
}

// 紧急度排序：Low < Normal < High < Critical
// Critical/High → InterruptNow/Interrupt
// Normal → Queue
// Low → Queue 或 Observe
```

---

## 10. 评估框架 (Harness)

### 10.1 SinglePassHarness

单次执行，始终通过：

```rust
use deepstrike_sdk::{SinglePassHarness, HarnessRequest};

let runner = RuntimeRunner::new(/* RuntimeOptions */);
let harness = SinglePassHarness::new(&runner);

let outcome = harness.run(HarnessRequest::new("Say hello")).await?;
assert!(outcome.passed);
println!("Result: {}", outcome.result);
```

### 10.2 EvalLoopHarness（自定义 QualityGate）

重试直到 QualityGate 通过：

```rust
use async_trait::async_trait;
use deepstrike_sdk::{EvalLoopHarness, QualityGate, HarnessRequest, HarnessOutcome};

struct ContainsHello;

#[async_trait]
impl QualityGate for ContainsHello {
    async fn evaluate(&self, _req: &HarnessRequest, out: &HarnessOutcome) -> deepstrike_sdk::Result<bool> {
        Ok(out.result.to_lowercase().contains("hello"))
    }
}

let harness = EvalLoopHarness::new(&runner, ContainsHello, /* max_attempts */ 3);
let outcome = harness.run(HarnessRequest::new("Greet the user")).await?;
```

### 10.3 HarnessLoop（LLM-as-Judge）

使用第二个 LLM 作为评判者，带反馈注入和技能提取：

```rust
use deepstrike_sdk::HarnessLoop;

let eval_provider = OpenAIProvider::with_base_url("key", "gpt-5-mini", "https://...");
let harness = HarnessLoop::new(
    &runner,
    eval_provider,
    /* max_attempts */ 3,
    /* skill_dir */ Some("./skills".into()), // 通过时自动提取技能
);

let mut req = HarnessRequest::new("Write a haiku about the ocean");
req.criteria = vec!["Must be exactly 3 lines".into()];

let outcome = harness.run(req).await?;
println!("Passed: {}, Feedback: {:?}", outcome.passed, outcome.feedback);
```

---

## 11. 内核层直接使用

对于需要细粒度控制的场景，可以直接操作 `deepstrike-core` 的状态机：

### 11.1 LoopStateMachine

```rust
use deepstrike_core::scheduler::state_machine::*;
use deepstrike_core::scheduler::policy::LoopPolicy;
use deepstrike_core::types::task::RuntimeTask;
use deepstrike_core::types::message::Message;

let policy = LoopPolicy {
    max_tokens: 128_000,
    max_turns: 10,
    ..LoopPolicy::default()
};
let mut sm = LoopStateMachine::new(policy);

// 启动
let task = RuntimeTask::new("Write a poem")
    .with_criteria(vec!["Must rhyme".into()]);
let action = sm.start(task);

// 主循环：SDK 层执行 I/O，内核处理状态转换
loop {
    match action {
        LoopAction::CallLLM { messages, tools } => {
            // 调用 LLM，得到响应
            let response = call_my_llm(&messages, &tools).await;
            action = sm.feed(LoopEvent::LLMResponse { message: response });
        }
        LoopAction::ExecuteTools { calls } => {
            // 执行工具
            let results = execute_my_tools(&calls).await;
            action = sm.feed(LoopEvent::ToolResults { results });
        }
        LoopAction::Done { result } => {
            println!("Terminated: {:?}, turns: {}", result.termination, result.turns_used);
            break;
        }
    }
}
```

### 11.2 ContextManager（四槽模型）

```rust
use deepstrike_core::context::manager::ContextManager;

let mut ctx = ContextManager::new(128_000);

// 四槽：system / knowledge / task_state+signals / history
// 仅 history 被压缩；渲染输出 RenderedContext { system_stable, system_knowledge, turns }
println!("Pressure: {:.2}", ctx.rho());

ctx.push_knowledge(Message::system("domain facts"), 100);
ctx.push_signal("[INTERRUPT] priority changed".into());
ctx.push_history(Message::user("Hello"), 5);

let action = ctx.should_compress();
if action != PressureAction::None {
    ctx.compress(action);  // 摘要写入 task_state.compression_log
}

let rendered = ctx.render();  // RenderedContext — 映射到 provider API
```

详见 [context-slots-compression.md](../concepts/context-slots-compression.md)。

### 11.3 评估原语（generate → evaluate 质量门）

> **0.5.x fold（#6）：** 旧的 `EvalPipeline` 状态机类已移除，质量门改为**无状态纯函数** +
> `gen_eval` workflow 模板。`HarnessLoop` 仍是迭代「重试-带反馈」的驱动器（公开接口不变）。

```rust
use deepstrike_core::harness::{build_eval_messages, parse_verdict, Criterion};

// Phase 1: 内核构建评估 prompt（无状态）
let messages = build_eval_messages(
    "Write tests",
    &[Criterion::required("Cover edge cases")],
    &agent_output,
    /* attempt */ 1,
    /* extract_skill_on_pass */ true,
);

// Phase 2: SDK 调用 LLM 获取评判结果
let eval_text = call_llm(&messages).await;

// Phase 3: 内核解析裁定
let verdict = parse_verdict(&eval_text);
println!("Passed: {}, Feedback: {}", verdict.passed, verdict.feedback);
if let Some(skill) = verdict.skill_candidate {
    println!("Extracted skill: {}", skill.name);
}
```

声明式形态是 `gen_eval` 模板（`Loop` worker + 带 `verdict_output_schema` 的偏见隔离 `Verify`
eval 节点），可直接交给 `runWorkflow` 跑：

```rust
use deepstrike_core::orchestration::workflow::gen_eval;
let spec = gen_eval(worker_task, eval_task, /* max_iters */ 3, /* extract_skill_on_pass */ true);
```

---

## 流式事件一览

| 事件 | 说明 |
|------|------|
| `RunEvent::TextDelta(String)` | LLM 输出文本片段 |
| `RunEvent::ThinkingDelta(String)` | 思维链片段（需 `expose_reasoning: true`） |
| `RunEvent::ToolCall { id, name }` | LLM 请求调用工具 |
| `RunEvent::ToolResult { call_id, content, is_error }` | 工具执行结果 |
| `RunEvent::Done { iterations, total_tokens, status }` | 运行结束 |
| `RunEvent::Error(String)` | 错误（如被屏蔽的工具） |

## 错误处理

```rust
use deepstrike_sdk::Error;

match runner.execute("...").await {
    Ok(result) => println!("{result}"),
    Err(Error::Provider(msg)) => eprintln!("LLM 错误: {msg}"),
    Err(Error::Tool(msg)) => eprintln!("工具错误: {msg}"),
    Err(Error::Io(e)) => eprintln!("IO 错误: {e}"),
    Err(Error::Other(msg)) => eprintln!("其他: {msg}"),
}
```

---

## 12. 进阶特性 (Milestones, Sub-agents, Artifacts)

### 12.1 里程碑合约 (Milestones)

里程碑合约可以将 Agent 的运行划分为多个阶段（Phases），并且每个阶段需要显式验证。

```rust
use std::sync::Arc;
use deepstrike_sdk::{
    RuntimeRunner, RuntimeOptions, MilestonePolicy,
    MilestoneEvaluationContext
};
use deepstrike_core::types::milestone::{
    MilestoneCheckResult, MilestoneContract, MilestonePhase,
};

let milestone_contract = MilestoneContract::new().phase(
    MilestonePhase::new("phase-1")
        .with_criterion("生成符合规范的方案草案")
        .requiring_evidence("draft_design.md")
);

let runner = RuntimeRunner::new(RuntimeOptions {
    provider: Box::new(provider),
    milestone_policy: MilestonePolicy::RequireVerifier,
    milestone_contract: Some(milestone_contract),
    on_milestone_evaluate: Some(Arc::new(|ctx: MilestoneEvaluationContext| {
        Box::pin(async move {
            println!("正在验证阶段 {}: {:?}", ctx.phase_id, ctx.criteria);
            Ok(MilestoneCheckResult {
                phase_id: ctx.phase_id,
                passed: true,
                reason: None,
            })
        })
    })),
    // ... 其他参数
});
```

如果未配置 `on_milestone_evaluate` 并且策略是 `RequireVerifier`，当运行到达里程碑需要验证时，runner 运行会挂起并返回 `milestone_pending` 状态：
```rust
use deepstrike_sdk::RunEvent;
use futures::StreamExt;

let mut stream = runner.run_streaming("write a design", &[], None, None).await.unwrap();
while let Some(evt) = stream.next().await {
    if let Ok(RunEvent::Done { status, .. }) = evt {
        if status == "milestone_pending" {
            // 运行挂起，可通过 wake_streaming 恢复
        }
    }
}
```

### 12.2 子智能体隔离与生成 (Sub-agents)

> [!NOTE]
> Rust SDK 目前仅在底层内核支持 Isolation Manifest 与子智能体生命周期，**v0.2.x 版本暂不直接提供 SDK 层的子智能体运行编排 API (SubAgentOrchestrator)**。该功能已规划在 v0.2.3.0 中推出。

### 12.3 产物推送 (Artifacts)

> **已移除（Rust/Python SDK 0.3+）。** 四槽重构后 artifacts 分区已移除。请使用 `initial_memory` → Slot 2，或依赖 history 压缩 tier 处理大输出。
