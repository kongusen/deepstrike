# DeepStrike Node.js SDK

Agent framework built on a Rust kernel. The kernel handles loop control, context compression, skill selection, and termination — the SDK handles all I/O.

## Install

```bash
npm install @deepstrike/sdk
```

Requires Node.js 18+. The Rust kernel is distributed as a pre-built native addon (`@deepstrike/core`).

---

## Quick start

```typescript
import { Agent, AnthropicProvider, tool } from "@deepstrike/sdk"

const add = tool("add", "Add two numbers.", {
  type: "object",
  properties: { x: { type: "number" }, y: { type: "number" } },
  required: ["x", "y"],
}, async ({ x, y }) => String((x as number) + (y as number)))

const agent = new Agent(
  new AnthropicProvider("sk-..."),
  { maxTokens: 32_000, maxTurns: 10 },
)
agent.register(add)

await agent.run("What is 2 + 3?")
```

Streaming:

```typescript
for await (const event of agent.runStreaming("Summarize README.md")) {
  if (event.type === "text_delta") process.stdout.write(event.delta)
  else if (event.type === "tool_call") console.log(`\n[→ ${event.name}]`)
  else if (event.type === "done") console.log(`\ndone in ${event.iterations} turns (${event.status})`)
}
```

---

## Architecture

```
src/
├── index.ts          # Public exports
├── agent.ts          # Agent — top-level entry point
├── types.ts          # Shared type definitions
├── providers/        # LLM adapters (HTTP + streaming)
├── tools/            # tool() helper, executeTools, built-ins
├── skills/           # SkillLoader — .md file loading
├── memory/           # WorkingMemory + MemorySource/Extractor interfaces
├── knowledge/        # KnowledgeSource interface
├── harness/          # SinglePassHarness, EvalLoopHarness, QualityGate
├── signals/          # RuntimeSignal, SignalSource, ScheduledPrompt
└── safety/           # PermissionManager
```

The kernel (`@deepstrike/core`, Rust/NAPI) owns:
- `LoopStateMachine` — drives `call_llm → execute_tools → load_skills → done`
- `ContextEngine` — 5-partition context with pressure-based compression
- `Governance` — tool veto authority
- `SignalRouter` — external interrupt queue

---

## Providers

| Class | Backend | Notes |
|-------|---------|-------|
| `AnthropicProvider` | Anthropic API | Native SSE, `ThinkingDelta` support |
| `OpenAIProvider` | OpenAI API | SSE tool-call accumulation |
| `QwenProvider` | DashScope | `enable_thinking` via extensions |
| `DeepSeekProvider` | DeepSeek API | Reasoner models strip tools automatically |
| `MiniMaxProvider` | MiniMax API | M1 reasoning via `expose_reasoning` |
| `OllamaProvider` | Local Ollama | `http://localhost:11434` default |

```typescript
import { AnthropicProvider } from "@deepstrike/sdk"

const provider = new AnthropicProvider("sk-...", "claude-opus-4-7", {
  maxRetries: 5,
  baseDelay: 2000,
})
```

Thinking / reasoning:

```typescript
for await (const event of agent.runStreaming("...", undefined, { enable_thinking: true })) {
  if (event.type === "thinking_delta") process.stdout.write(event.delta)
}
```

---

## Tools

```typescript
import { tool, Agent } from "@deepstrike/sdk"

const search = tool("search", "Search the knowledge base.", {
  type: "object",
  properties: { query: { type: "string" }, topK: { type: "number" } },
  required: ["query"],
}, async ({ query, topK }) => mySearch(query as string, topK as number))

agent.register(search)
agent.unregister("search")
agent.blockTool("bash")   // governance veto — permanent for this agent instance
```

Built-in tools: `readFile`.

---

## Skills

Skills are `.md` files with YAML frontmatter. The kernel selects and injects them automatically.

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

```typescript
import { Agent, SkillLoader } from "@deepstrike/sdk"

const loader = new SkillLoader("~/.deepstrike/skills")
const agent = new Agent(provider, { maxTokens: 32_000, maxTurns: 10, skillLoader: loader })
```

---

## Memory

Implement `MemorySource` to inject persistent context before a run, and `MemoryExtractor` to persist what was learned after.

```typescript
import type { MemorySource, MemoryExtractor } from "@deepstrike/sdk"

class MyMemorySource implements MemorySource {
  async load(goal: string): Promise<string[]> {
    return db.query(goal)
  }
}

class MyMemoryExtractor implements MemoryExtractor {
  async extract(goal: string, finalText: string, turns: number): Promise<void> {
    await db.save(goal, finalText)
  }
}

const agent = new Agent(provider, {
  maxTokens: 32_000,
  maxTurns: 10,
  memorySource: new MyMemorySource(),
  memoryExtractor: new MyMemoryExtractor(),
})
```

`WorkingMemory` is an in-process scratch pad for within-run state:

```typescript
import { WorkingMemory } from "@deepstrike/sdk"

const mem = new WorkingMemory()
mem.set("step", 1)
mem.get("step")   // 1
```

---

## Knowledge

Inject run-scoped evidence (RAG results, API responses) without polluting long-term memory:

```typescript
import type { KnowledgeSource } from "@deepstrike/sdk"

class VectorSearch implements KnowledgeSource {
  async retrieve(goal: string, topK = 5): Promise<string[]> {
    return vectorDb.search(goal, topK)
  }
}

const agent = new Agent(provider, {
  maxTokens: 32_000,
  maxTurns: 10,
  knowledgeSource: new VectorSearch(),
})
```

Snippets are prepended as a system message before the first LLM call.

---

## Harness

Control how runs are attempted:

```typescript
import { Agent, SinglePassHarness, EvalLoopHarness, HarnessRequest } from "@deepstrike/sdk"
import type { QualityGate, HarnessOutcome } from "@deepstrike/sdk"

// Single pass
const harness = new SinglePassHarness(agent)
const outcome = await harness.run({ goal: "Write a haiku" })

// Eval loop — retry until QualityGate passes (max 3 attempts)
class LengthGate implements QualityGate {
  async evaluate(request: HarnessRequest, outcome: HarnessOutcome): Promise<boolean> {
    return outcome.result.length > 50
  }
}

const evalHarness = new EvalLoopHarness(agent, new LengthGate(), 3)
const result = await evalHarness.run({ goal: "Write a haiku" })
console.log(result.passed, result.iterations)
```

---

## Signals & interrupts

```typescript
import { ScheduledPrompt } from "@deepstrike/sdk"
import type { SignalSource, RuntimeSignal } from "@deepstrike/sdk"

// Interrupt a running agent
setTimeout(() => agent.interrupt(), 30_000)

// Convert a scheduled prompt to a RuntimeSignal
const prompt = new ScheduledPrompt("Daily standup summary", 1_700_000_000_000)
const signal = prompt.toSignal()
// signal.kind === "scheduled"
// signal.payload === { goal: "Daily standup summary", criteria: [], runAtMs: ... }
```

Implement `SignalSource` to feed signals from any external source:

```typescript
class WebhookSource implements SignalSource {
  async nextSignal(): Promise<RuntimeSignal | null> {
    const event = await webhookQueue.get()
    return { kind: "external", payload: event }
  }
}
```

---

## Permissions

```typescript
import { PermissionManager, PermissionMode } from "@deepstrike/sdk"

const pm = new PermissionManager(PermissionMode.DEFAULT)
pm.grant("bash", "execute")
pm.grant("fs", "*")          // wildcard: all actions on fs
pm.revoke("bash", "execute")

const decision = pm.evaluate("bash", "execute")
decision.allowed   // boolean
decision.reason    // string
```

Modes: `DEFAULT` (evaluate grants), `PLAN` (block all), `AUTO` (allow all).

---

## Stream events

| Event type | Fields |
|------------|--------|
| `text_delta` | `delta: string` |
| `thinking_delta` | `delta: string` |
| `tool_call` | `id, name, arguments` |
| `tool_result` | `callId, name, content, isError` |
| `done` | `iterations, totalTokens, status` |
| `error` | `message: string` |

`status` mirrors the kernel termination reason: `completed` / `max_turns` / `token_budget` / `timeout` / `user_abort` / `error`.
