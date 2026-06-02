# DeepStrike Node.js SDK

Runtime framework built on a Rust kernel. The kernel handles loop control, context compression, skill routing, governance, signal prioritization — the SDK handles all I/O.

## Install

```bash
npm install @deepstrike/sdk
```

Requires Node.js 18+.

### Platform support

Pre-built native addons are available for the following platforms:

| Platform | Package |
| -------- | ------- |
| macOS x64 | `@deepstrike/core-darwin-x64` |
| macOS ARM64 (Apple Silicon) | `@deepstrike/core-darwin-arm64` |
| Linux x64 (glibc) | `@deepstrike/core-linux-x64-gnu` |
| Linux x64 (musl / Alpine) | `@deepstrike/core-linux-x64-musl` |
| Linux ARM64 (glibc) | `@deepstrike/core-linux-arm64-gnu` |
| Linux ARM64 (musl / Alpine) | `@deepstrike/core-linux-arm64-musl` |
| Windows x64 | `@deepstrike/core-win32-x64-msvc` |

The correct platform package is selected and installed automatically via `optionalDependencies`. No postinstall download is required.

> **Note:** `@deepstrike/core` is the low-level native addon package and is not intended for direct use. It is an internal dependency automatically managed by `@deepstrike/sdk`. Direct installation is only relevant when building from Rust source.

---

## Quick start

```typescript
import {
  FileSessionLog,
  LocalExecutionPlane,
  RuntimeRunner,
  OpenAIResponsesProvider,
  collectText,
  tool,
} from "@deepstrike/sdk"

const provider = new OpenAIResponsesProvider(process.env.OPENAI_API_KEY!, "gpt-5-mini")

const add = tool("add", "Add two numbers.", {
  type: "object",
  properties: { x: { type: "number" }, y: { type: "number" } },
  required: ["x", "y"],
}, async ({ x, y }) => String(Number(x) + Number(y)))

const plane = new LocalExecutionPlane().register(add)
const runner = new RuntimeRunner({
  provider,
  executionPlane: plane,
  sessionLog: new FileSessionLog(".deepstrike/sessions"),
  maxTokens: 4096,
})

const result = await collectText(runner.run({
  sessionId: "math-1",
  goal: "What is 17 + 28?",
}))
console.log(result)
```

Same-session conversation continuity is explicit via `sessionId`:

```typescript
await collectText(runner.run({ sessionId: "chat-1", goal: "My name is Ada." }))
const reply = await collectText(runner.run({ sessionId: "chat-1", goal: "What is my name?" }))
```

Use `InMemorySessionLog` for process-local sessions or `FileSessionLog` when event replay should survive restarts. `wake(sessionId)` resumes from the event log without inserting a duplicate user start event.

Streaming:

```typescript
for await (const event of runner.run({ sessionId: "readme-1", goal: "Summarize README.md" })) {
  if (event.type === "text_delta") process.stdout.write(event.delta)
  else if (event.type === "tool_call") console.log(`\n[→ ${event.name}]`)
  else if (event.type === "tool_result") console.log(`  = ${event.content}`)
  else if (event.type === "done") console.log(`\ndone in ${event.iterations} turns (${event.status})`)
}
```

---

## Providers

| Class | Backend | Notes |
|-------|---------|-------|
| `OpenAIChatProvider` | OpenAI Chat Completions API | SSE tool-call accumulation |
| `OpenAIProvider` | OpenAI Chat Completions API | Compatibility alias for `OpenAIChatProvider` |
| `OpenAIResponsesProvider` | OpenAI Responses API | Native `previous_response_id` continuation |
| `AnthropicProvider` | Anthropic API | Native SSE, `ThinkingDelta` support |
| `QwenProvider` | DashScope | `enable_thinking` via extensions |
| `DeepSeekProvider` | DeepSeek API | V4 thinking controls + reasoning replay across tool turns |
| `MiniMaxProvider` | MiniMax API | Anthropic-compatible M2.7/M2.5 path |
| `OllamaProvider` | Local Ollama | `http://localhost:11434` default |
| `KimiProvider` | Moonshot API | K2.6 default; K2.5 also supported |

All providers accept `RetryConfig` for exponential backoff and share a `CircuitBreaker`.

`extensions` are forwarded by every provider in both `complete()` and `stream()` while SDK-owned structural fields such as `model`, `messages`, `tools`, and streaming flags remain protected. Provider-specific controls still keep their native spellings: for example Anthropic `thinking` / `betas`, OpenAI Responses `reasoning`, Gemini `generationConfig`, Ollama `think` / `options`, DeepSeek `thinking` + `reasoningEffort`, and Qwen `enableThinking` + `thinkingBudget`.

OpenAI can also be selected through the provider catalog:

```typescript
import { createProvider } from "@deepstrike/sdk"

const provider = createProvider({
  model: "openai/gpt-5-mini",
  apiKey: process.env.OPENAI_API_KEY!,
})
```

---

## Context model (four slots)

The kernel renders context as four LLM API slots — only **history** is compressed.

| Slot | Source | Role |
|------|--------|------|
| `systemStable` | system partition | Identity, rules — never changes within a run |
| `systemKnowledge` | knowledge partition | Preloaded memory, skill defs — low frequency |
| `turns[0]` | `task_state` + signals | Goal, plan, progress, compression log, runtime signals |
| `turns[1..N]` | history | Conversation transcript |

```typescript
const runner = new RuntimeRunner({
  // ...
  initialMemory: ["User prefers chartreuse."],  // → Slot 2 (systemKnowledge)
  systemPrompt: "You are a helpful assistant.", // → Slot 1 (systemStable)
})
```

- `memory(query)` / `knowledge(query)` meta-tool results → **history** (tool results)
- External signals → **Slot 3** via `push_signal()`, cleared after each render
- Anthropic: Slots 1–2 get separate `cache_control` breakpoints

Full reference: [docs/concepts/context-slots-compression.md](../docs/concepts/context-slots-compression.md)

---

## Runtime options

```typescript
const plane = new LocalExecutionPlane()
const runner = new RuntimeRunner({
  provider,
  executionPlane: plane,
  sessionLog: new FileSessionLog(".deepstrike/sessions"),
  maxTokens: 4096,            // context window size
  maxTurns: 25,               // max turns (default 25)
  timeoutMs: 60_000,          // timeout in ms
  extensions: { temperature: 0.1 },  // provider-native controls, passed through to the LLM
  skillDir: "./skills",       // skill .md files directory
  knowledgeSource: myKS,      // KnowledgeSource implementation
  signalSource: rx,           // SignalSource for external signals
  dreamStore: myStore,        // DreamStore for long-term memory
  agentId: "my-agent",        // required with dreamStore for memory meta-tool
  initialMemory: ["..."],     // preloaded blocks → Slot 2 (systemKnowledge)
  subAgentHarness: {          // optional: sub-agents run through HarnessLoop
    evalProvider,
    maxAttempts: 3,
  },
  governance: gov,            // Governance pipeline instance
})
```

---

## Tools

```typescript
import { tool, readFile } from "@deepstrike/sdk"

plane.register(tool("search", "Search.", schema, async (args) => ...))
plane.register(readFile)     // built-in: read files from disk
plane.unregister("search")
```

---

## Skills

Skills are `.md` files with YAML frontmatter. Set `skillDir` on the runner — the kernel auto-injects a `skill` meta-tool, and the LLM loads skills by name on demand.

```typescript
const runner = new RuntimeRunner({
  provider,
  executionPlane: plane,
  sessionLog: new FileSessionLog(".deepstrike/sessions"),
  maxTokens: 4096,
  skillDir: "./skills",
})
```

```markdown
---
name: summarize
description: Summarize text into 2-3 concise bullet points
when_to_use: When you need to condense long text
effort: 1
---
1. Identify the 2-3 most important points
2. Express each as a concise bullet
```

---

## Knowledge

Implement `KnowledgeSource` to connect any RAG system. The kernel injects a `knowledge` meta-tool that the LLM calls on demand. **Runtime retrieval results land in history** as tool results.

To inject durable knowledge at startup (Slot 2, cacheable on Anthropic), use `initialMemory` or kernel `add_knowledge_message`.

```typescript
const runner = new RuntimeRunner({
  provider,
  executionPlane: plane,
  sessionLog: new FileSessionLog(".deepstrike/sessions"),
  maxTokens: 4096,
  knowledgeSource: {
    async retrieve(query: string, topK: number): Promise<string[]> {
      return vectorDb.search(query, topK)
    }
  }
})
```

---

## Memory

### WorkingMemory (SDK-side scratch pad)

`WorkingMemory` is an SDK helper — not the kernel `working` partition (removed). Kernel task state lives in `task_state` and renders into Slot 3 (`turns[0]`).

```typescript
import { WorkingMemory } from "@deepstrike/sdk"
const mem = new WorkingMemory()
mem.set("step", 1)
mem.get("step")  // 1
mem.clear()
```

### DreamStore (long-term memory + dreaming pipeline)

```typescript
import type { DreamStore } from "@deepstrike/sdk"

class MyStore implements DreamStore {
  async loadSessions(agentId) { ... }
  async loadMemories(agentId) { ... }
  async commit(agentId, result, existing) { ... }
  async search(agentId, query, topK) { ... }
}

const runner = new RuntimeRunner({
  provider,
  executionPlane: plane,
  sessionLog: new FileSessionLog(".deepstrike/sessions"),
  maxTokens: 4096,
  dreamStore: new MyStore(),
  agentId: "my-agent",  // enables `memory` meta-tool
})

// In-session: LLM calls memory(query) → DreamStore.search() → history tool result
// Preload:    initialMemory → Slot 2 (systemKnowledge)
// Post-session: trigger memory consolidation
const result = await runner.dream("my-agent", Date.now())
```

---

## Governance

### SDK PermissionManager

```typescript
import { PermissionManager, PermissionMode } from "@deepstrike/sdk"

const pm = new PermissionManager(PermissionMode.DEFAULT)
pm.grant("fs", "read")
pm.grantWithApproval("db", "write", "Needs DBA approval")
pm.revoke("db", "drop")
pm.evaluate("fs", "read")  // { allowed: true, ... }
```

### Kernel Governance (full pipeline)

```typescript
import { Governance } from "@deepstrike/sdk"

const gov = new Governance("allow")
gov.addPermissionRule("danger.*", "deny")
gov.blockTool("rm_rf")
gov.setRateLimit("api_call", 10, 60_000)
gov.requireParam("write_file", "path")
gov.allowParamValues("set_mode", "mode", ["read", "write"])
gov.limitParamRange("sleep", "seconds", 0, 10)

const runner = new RuntimeRunner({
  provider,
  executionPlane: plane,
  sessionLog: new FileSessionLog(".deepstrike/sessions"),
  maxTokens: 4096,
  governance: gov,
})
// Every tool call goes through: Permission → Veto → RateLimit → Constraint → Audit
```

---

## Signals

```typescript
import { SignalGateway, ScheduledPrompt } from "@deepstrike/sdk"

const gw = new SignalGateway()
gw.schedule(new ScheduledPrompt("standup", Date.now() + 3600_000))
gw.ingest({ kind: "interrupt", urgency: "critical", payload: {} })

const runner = new RuntimeRunner({
  provider,
  executionPlane: plane,
  sessionLog: new FileSessionLog(".deepstrike/sessions"),
  maxTokens: 4096,
  signalSource: gw,
})
// kind="interrupt" → immediately stops the running runner

runner.interrupt() // also works directly
gw.destroy()
```

---

## Harness (evaluation framework)

```typescript
import { SinglePassHarness, EvalLoopHarness, HarnessLoop } from "@deepstrike/sdk"

// 1. SinglePass — run once, always passes
const outcome = await new SinglePassHarness(runner).run({ goal: "Say hello" })

// 2. EvalLoop — retry until QualityGate passes
const harness = new EvalLoopHarness(runner, {
  async evaluate(_req, out) { return out.result.includes("hello") },
}, 3)

// 3. HarnessLoop — LLM-as-judge with feedback injection + skill extraction
const loop = new HarnessLoop(runner, evalProvider, { maxAttempts: 3, skillDir: "./skills" })

// Sub-agents: pass subAgentHarness on RuntimeRunner to auto-evaluate spawned children
const runnerWithHarness = new RuntimeRunner({
  provider,
  executionPlane: plane,
  sessionLog,
  subAgentHarness: { evalProvider, maxAttempts: 3 },
})
for await (const event of loop.runStreaming({
  goal: "Write a haiku",
  criteria: [{ text: "Must be 3 lines", required: true }],
})) {
  if (event.type === "done") console.log(event.verdict.passed, event.verdict.feedback)
}
```

---

## Stream events

| Event type | Key fields |
|------------|------------|
| `text_delta` | `delta` |
| `thinking_delta` | `delta` |
| `tool_call` | `id`, `name`, `arguments` |
| `tool_delta` | `callId`, `delta?`, `chunk?` |
| `tool_suspend` | `callId`, `suspensionId`, `payload?` |
| `tool_result` | `callId`, `content`, `isError` |
| `permission_request` | `toolName`, `reason` |
| `done` | `iterations`, `totalTokens`, `status` |
| `error` | `message` |

`status`: `completed` · `max_turns` · `token_budget` · `timeout` · `user_abort` · `error`
