# DeepStrike

Cross-language agent runtime. A Rust kernel handles all pure computation — loop control, context compression, skill routing, governance, signal prioritization — while language SDKs handle I/O: LLM calls, tool execution, file access, and storage.

```
┌─────────────────────────────────────────────────────────┐
│  Node.js SDK   │  Python SDK   │  Rust SDK   │  WASM   │
└────────────────┴───────────────┴─────────────┴─────────┘
                         │
              ┌──────────▼──────────┐
              │   deepstrike-core   │  (Rust, pure computation)
              │  LoopStateMachine   │
              │  ContextManager     │
              │  GovernancePipeline │
              │  SignalRouter       │
              │  EvalPipeline       │
              │  IdlePipeline       │
              └─────────────────────┘
```

---

## Packages

| Package | Language | Description |
|---------|----------|-------------|
| `crates/deepstrike-core` | Rust | Kernel — state machines, context, governance |
| `crates/deepstrike-node` | Rust/NAPI | Node.js bindings |
| `crates/deepstrike-py` | Rust/PyO3 | Python bindings |
| `crates/deepstrike-wasm` | Rust/WASM | Browser/edge bindings |
| `node/` (`@deepstrike/sdk`) | TypeScript | Node.js SDK |
| `python/` (`deepstrike`) | Python | Python SDK |
| `rust/` (`deepstrike-sdk`) | Rust | Rust SDK |

---

## Quick Start

**Node.js**

```bash
npm install @deepstrike/sdk
```

```typescript
import { Agent, AnthropicProvider, tool } from "@deepstrike/sdk"

const agent = new Agent(new AnthropicProvider("sk-..."), { maxTokens: 32_000 })
agent.register(tool("add", "Add two numbers.", schema, async ({ x, y }) => String(x + y)))
await agent.run("What is 2 + 3?")
```

**Python**

```bash
pip install deepstrike
```

```python
from deepstrike import Agent, AnthropicProvider, tool

@tool
def add(x: int, y: int) -> int:
    """Add two numbers."""
    return x + y

agent = Agent(AnthropicProvider(api_key="..."), max_tokens=32_000)
agent.register(add)
await agent.run("What is 2 + 3?")
```

**Rust**

```toml
[dependencies]
deepstrike-sdk = "0.1.9"
```

```rust
let agent = Agent::new(AnthropicProvider::new("sk-..."), AgentOptions::new(32_000));
let result = agent.run("What is 2 + 3?").await?;
```

---

## Architecture

### Kernel (deepstrike-core)

The kernel is pure Rust with no I/O. It exposes a state machine interface: the SDK feeds events, the kernel returns actions.

```
sm.start(task)          → CallLLM { messages, tools }
sm.feedLlmResponse(msg) → ExecuteTools { calls }
sm.feedToolResults(res) → CallLLM { ... }   (next turn)
                        → Done { result }
```

**Key subsystems:**

| Subsystem | Responsibility |
|-----------|---------------|
| `LoopStateMachine` | Turn-by-turn loop control, termination policy |
| `ContextManager` | 5-partition context (system / working / memory / history / skill) with pressure-based compression |
| `GovernancePipeline` | Permission → Veto → RateLimit → Constraint → Audit |
| `SignalRouter` | Priority queue with dedup; routes external signals to dispositions |
| `EvalPipeline` | LLM-as-judge evaluation; extracts reusable skill candidates |
| `IdlePipeline` | Idle-time memory consolidation (the "dreaming" cycle) |

### SDK Layer

Each SDK wraps the kernel and handles all I/O:

```
Agent.runStreaming(goal)
│
├─ Startup
│   ├─ scan skill/*.md → sm.setAvailableSkills()   [skills]
│   ├─ sm.setMemoryEnabled(true)                    [memory]
│   └─ sm.setKnowledgeEnabled(true)                 [knowledge]
│
├─ Each turn
│   ├─ SignalSource.nextSignal() → router.ingest()  [signals]
│   ├─ call_llm  → provider.stream()
│   └─ execute_tools
│       ├─ Governance.evaluate()                    [safety]
│       ├─ skill(name)     → read .md file          [skills]
│       ├─ memory(query)   → DreamStore.search()    [memory]
│       ├─ knowledge(query)→ KnowledgeSource.retrieve() [knowledge]
│       └─ regular tools   → executeTools()
│
└─ After session
    └─ agent.dream(agentId) → IdlePipeline → DreamStore [memory]
```

---

## Core Concepts

### Skills — *how to do things*

Skills are `.md` files with YAML frontmatter. The kernel injects a `skill` meta-tool into every LLM call; the model calls `skill(name="X")` on demand to load the full instructions.

```markdown
---
name: debug
description: Step-by-step debugging guide
when_to_use: error, traceback, exception
effort: 2
estimated_tokens: 800
---

## Debug protocol
1. Read the traceback carefully...
```

Skills can be created at runtime by `HarnessLoop` when a successful run produces a reusable pattern.

### Memory — *what was learned*

Two-phase pipeline:

1. **In-session**: LLM calls `memory(query)` → `DreamStore.search()` returns relevant past experiences
2. **Post-session**: `agent.dream(agentId)` runs `IdlePipeline` — sessions are analyzed, insights synthesized by LLM, curated (dedup + conflict resolution), and committed to `DreamStore`

Implement `DreamStore` to connect any storage backend (vector DB, Postgres, etc.).

### Knowledge — *external facts*

`KnowledgeSource.retrieve(query)` is called when the LLM invokes the `knowledge` meta-tool. Connect any RAG system, API, or document store. Unlike memory, knowledge is read-only and not updated by the agent.

### Harness — *quality control*

`HarnessLoop` wraps a full agent session with LLM-as-judge evaluation:

```
attempt 1 → EvalPipeline → failed, feedback="missing error handling"
attempt 2 → agent.run(goal + feedback) → EvalPipeline → passed
          → write skill_candidate to skill/*.md
```

The feedback loop is closed: failed attempts inform the next attempt; successful patterns become reusable skills.

### Signals — *external interrupts*

`SignalGateway` is the entry point for all external events (webhooks, cron, user interrupts). Signals are routed through `SignalRouter` (kernel) which assigns dispositions:

| Disposition | Meaning |
|-------------|---------|
| `interrupt_now` | Stop immediately (Critical urgency) |
| `interrupt` | Finish current tool, then stop (High urgency) |
| `queue` | Buffer for next turn (Normal urgency) |
| `observe` | Record but don't interrupt (Low urgency) |
| `dropped` | Queue full — backpressure signal |

`ScheduledPrompt` fires at a specified `runAtMs` timestamp, deduplicated by goal+time.

### Safety — *permission boundaries*

`GovernancePipeline` (kernel) evaluates every tool call through four stages:

```
Permission → Veto → RateLimit → Constraint → Audit
```

The SDK `PermissionManager` provides a simpler grant/revoke interface. `PermissionRequestEvent` is emitted when a tool requires user approval before execution.

---

## Providers

| Provider | Backend | Thinking/Reasoning |
|----------|---------|-------------------|
| `AnthropicProvider` | Anthropic API | `ThinkingDelta` via `enable_thinking` |
| `OpenAIProvider` | OpenAI API | — |
| `QwenProvider` | DashScope | `enable_thinking` |
| `DeepSeekProvider` | DeepSeek API | Reasoner models |
| `MiniMaxProvider` | MiniMax API | `expose_reasoning` |
| `OllamaProvider` | Local Ollama | — |

All providers share `RetryConfig` (exponential backoff) and `CircuitBreaker`.

---

## Stream Events

| Event | Fields |
|-------|--------|
| `text_delta` | `delta` |
| `thinking_delta` | `delta` |
| `tool_call` | `id`, `name`, `arguments` |
| `tool_result` | `callId`, `name`, `content`, `isError` |
| `permission_request` | `callId`, `toolName`, `arguments`, `reason` |
| `done` | `iterations`, `totalTokens`, `status` |
| `error` | `message` |

`status`: `completed` / `max_turns` / `token_budget` / `timeout` / `user_abort` / `error`

---

## Build

```bash
# Rust kernel + all bindings
cargo build

# Node.js SDK
cd node && npm install && npm run build

# Python SDK (requires maturin)
cd python && maturin develop

# Run tests
cargo test
cd node && npm test
cd python && pytest
```

Requires Rust 1.85+, Node.js 18+, Python 3.10+.

---

## License

Apache-2.0 OR MIT
