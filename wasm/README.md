# DeepStrike WASM SDK

Agent framework built on a Rust kernel compiled to WebAssembly. Runs in browsers, Cloudflare Workers, Deno Deploy, and Vercel Edge — anywhere that supports `fetch` and WASM.

## Install

```bash
npm install @deepstrike/wasm
```

The Rust kernel is distributed as a pre-built `.wasm` binary (`@deepstrike/wasm-kernel`). Call `await init()` once before using any kernel classes.

---

## Quick start

```typescript
import init, { LoopStateMachine } from "@deepstrike/wasm-kernel"
import { Agent, AnthropicProvider, tool } from "@deepstrike/wasm"

await init() // load .wasm binary once

const add = tool("add", "Add two numbers.", {
  type: "object",
  properties: { x: { type: "number" }, y: { type: "number" } },
  required: ["x", "y"],
}, async ({ x, y }) => String((x as number) + (y as number)))

const agent = new Agent(
  new AnthropicProvider(apiKey),
  { maxTokens: 32_000, maxTurns: 10 },
)
agent.register(add)

await agent.run("What is 2 + 3?")
```

Streaming:

```typescript
for await (const event of agent.runStreaming("Summarize this page")) {
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
├── providers/        # LLM adapters (fetch-based SSE)
├── tools/            # tool() helper, executeTools (no fs/shell)
├── memory/           # WorkingMemory + MemorySource/Extractor interfaces
├── knowledge/        # KnowledgeSource interface
├── harness/          # SinglePassHarness, EvalLoopHarness, QualityGate
├── signals/          # RuntimeSignal, SignalSource, ScheduledPrompt
└── safety/           # PermissionManager
```

The kernel (`@deepstrike/wasm-kernel`, Rust/wasm-bindgen) owns:
- `LoopStateMachine` — drives `call_llm → execute_tools → load_skills → done`
- `ContextEngine` — 5-partition context with pressure-based compression
- `Governance` — tool veto authority
- `SignalRouter` — external interrupt queue

### WASM constraints vs Node SDK

| Capability | Browser | Cloudflare Worker | Node |
|---|---|---|---|
| `fs` read/write | no | no | yes |
| `bash` tool | no | no | yes |
| Long-term storage | IndexedDB | KV / D1 | SQLite |
| External signals | `postMessage` | event | any |

The WASM SDK ships **no `readFile` built-in**. Tools must be pure JS / serializable data. Skill loading is delegated to the host (fetch from a URL, read from IndexedDB, etc.).

---

## Providers

All providers use `fetch` — no Node.js `http` module.

| Class | Backend |
|-------|---------|
| `AnthropicProvider` | Anthropic API (SSE) |
| `OpenAIProvider` | OpenAI API (SSE) |
| `QwenProvider` | DashScope |
| `DeepSeekProvider` | DeepSeek API |
| `MiniMaxProvider` | MiniMax API |

```typescript
import { AnthropicProvider } from "@deepstrike/wasm"

const provider = new AnthropicProvider("sk-...", "claude-opus-4-7")
```

Thinking / reasoning:

```typescript
for await (const event of agent.runStreaming("...", undefined, { enable_thinking: true })) {
  if (event.type === "thinking_delta") console.log(event.delta)
}
```

---

## Tools

Tools must be pure functions — no shell, no filesystem.

```typescript
import { tool, Agent } from "@deepstrike/wasm"

const fetchUrl = tool("fetch_url", "Fetch a URL and return its text.", {
  type: "object",
  properties: { url: { type: "string" } },
  required: ["url"],
}, async ({ url }) => {
  const resp = await fetch(url as string)
  return resp.text()
})

agent.register(fetchUrl)
agent.blockTool("fetch_url")  // governance veto
```

---

## Memory

```typescript
import type { MemorySource, MemoryExtractor } from "@deepstrike/wasm"

class IndexedDBMemory implements MemorySource, MemoryExtractor {
  async load(goal: string): Promise<string[]> {
    return idb.get(goal) ?? []
  }
  async extract(goal: string, finalText: string, turns: number): Promise<void> {
    await idb.set(goal, [finalText])
  }
}

const agent = new Agent(provider, {
  maxTokens: 32_000,
  maxTurns: 10,
  memorySource: new IndexedDBMemory(),
  memoryExtractor: new IndexedDBMemory(),
})
```

`WorkingMemory` is an in-process scratch pad for within-run state:

```typescript
import { WorkingMemory } from "@deepstrike/wasm"

const mem = new WorkingMemory()
mem.set("step", 1)
mem.get("step")   // 1
```

---

## Knowledge

```typescript
import type { KnowledgeSource } from "@deepstrike/wasm"

class VectorSearch implements KnowledgeSource {
  async retrieve(goal: string, topK = 5): Promise<string[]> {
    const resp = await fetch(`/api/search?q=${encodeURIComponent(goal)}&k=${topK}`)
    return resp.json()
  }
}

const agent = new Agent(provider, {
  maxTokens: 32_000,
  maxTurns: 10,
  knowledgeSource: new VectorSearch(),
})
```

---

## Harness

```typescript
import { SinglePassHarness, EvalLoopHarness } from "@deepstrike/wasm"
import type { QualityGate, HarnessRequest, HarnessOutcome } from "@deepstrike/wasm"

// Single pass
const harness = new SinglePassHarness(agent)
const outcome = await harness.run({ goal: "Write a haiku" })

// Eval loop — retry until QualityGate passes (max 3 attempts)
class LengthGate implements QualityGate {
  async evaluate(_req: HarnessRequest, outcome: HarnessOutcome): Promise<boolean> {
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
import { ScheduledPrompt } from "@deepstrike/wasm"
import type { SignalSource, RuntimeSignal } from "@deepstrike/wasm"

// Interrupt from a UI button
document.getElementById("stop")!.onclick = () => agent.interrupt()

// Convert a scheduled prompt to a RuntimeSignal
const prompt = new ScheduledPrompt("Daily standup summary", 1_700_000_000_000)
const signal = prompt.toSignal()
// signal.kind === "scheduled"

// Feed signals from postMessage (browser) or Cloudflare event
class PostMessageSource implements SignalSource {
  private queue: RuntimeSignal[] = []
  constructor() {
    self.addEventListener("message", (e: MessageEvent) => {
      if (e.data?.kind) this.queue.push(e.data as RuntimeSignal)
    })
  }
  async nextSignal(): Promise<RuntimeSignal | null> {
    return this.queue.shift() ?? null
  }
}
```

---

## Permissions

```typescript
import { PermissionManager, PermissionMode } from "@deepstrike/wasm"

const pm = new PermissionManager(PermissionMode.DEFAULT)
pm.grant("fetch", "execute")
pm.grant("storage", "*")
pm.revoke("fetch", "execute")

const decision = pm.evaluate("fetch", "execute")
decision.allowed   // boolean
decision.reason    // string
```

Modes: `DEFAULT` (evaluate grants), `PLAN` (block all), `AUTO` (allow all).

---

## Edge runtime examples

### Cloudflare Worker

```typescript
import init from "@deepstrike/wasm-kernel"
import { Agent, AnthropicProvider } from "@deepstrike/wasm"
import wasmBinary from "@deepstrike/wasm-kernel/deepstrike_wasm_bg.wasm"

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    await init(wasmBinary)
    const agent = new Agent(new AnthropicProvider(env.ANTHROPIC_KEY), { maxTokens: 32_000, maxTurns: 10 })
    const result = await agent.run(await request.text())
    return new Response(result)
  },
}
```

### Browser (Vite / bundler)

```typescript
import init from "@deepstrike/wasm-kernel"
import { Agent, AnthropicProvider } from "@deepstrike/wasm"

await init()
const agent = new Agent(new AnthropicProvider(import.meta.env.VITE_ANTHROPIC_KEY), { maxTokens: 32_000, maxTurns: 10 })
```

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
