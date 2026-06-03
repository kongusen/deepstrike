# DeepStrike WASM SDK

Runtime framework built on a Rust kernel compiled to WebAssembly. Runs in browsers, Cloudflare Workers, Deno Deploy, and Vercel Edge — anywhere that supports `fetch` and WASM.

## Install

```bash
npm install @deepstrike/wasm
```

The Rust kernel is distributed as a pre-built `.wasm` binary (`@deepstrike/wasm-kernel`), which is an indirect dependency — you never import from it directly.

---

## Quick start

```typescript
import {
  RuntimeRunner,
  collectText,
  InMemorySessionLog,
  LocalExecutionPlane,
  AnthropicProvider,
  tool,
} from "@deepstrike/wasm"

const add = tool("add", "Add two numbers.", {
  type: "object",
  properties: { x: { type: "number" }, y: { type: "number" } },
  required: ["x", "y"],
}, async ({ x, y }) => String((x as number) + (y as number)))

const plane = new LocalExecutionPlane()
plane.register(add)

const runner = new RuntimeRunner({
  provider: new AnthropicProvider(apiKey),
  sessionLog: new InMemorySessionLog(),
  executionPlane: plane,
  maxTokens: 32_000,
  maxTurns: 10,
  resourceQuota: {
    maxConcurrentSubagents: 4,
    maxSpawnDepth: 2,
    memoryWritesPerWindow: { maxWrites: 20, windowMs: 60_000 },
  },
})

const answer = await collectText(runner.run({ sessionId: "demo", goal: "What is 2 + 3?" }))
console.log(answer) // "5"
```

Streaming:

```typescript
for await (const event of runner.run({ sessionId: "demo", goal: "Summarize this page" })) {
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
├── runtime/          # RuntimeRunner, SessionLog, ExecutionPlane
├── types.ts          # Shared type definitions
├── providers/        # LLM adapters (fetch-based SSE)
├── tools/            # tool() helper, executeTools (no fs/shell)
├── memory/           # WorkingMemory + DreamStore interfaces
├── knowledge/        # KnowledgeSource interface
├── harness/          # SinglePassHarness, HarnessLoop
├── signals/          # RuntimeSignal, SignalSource, ScheduledPrompt
└── safety/           # PermissionManager
```

The kernel (`@deepstrike/wasm-kernel`, Rust/wasm-bindgen) owns:
- `KernelRuntime.step()` — drives `call_provider → execute_tool → evaluate_milestone → done`
- `ContextEngine` — 4-slot context with tiered history compression
- `Governance` — tool veto authority
- `SignalRouter` — external interrupt queue

### WASM constraints vs Node SDK

| Capability | Browser | Cloudflare Worker | Node |
|---|---|---|---|
| `fs` read/write | no | no | yes |
| `bash` tool | no | no | yes |
| Long-term storage | IndexedDB | KV / D1 | SQLite |
| External signals | `postMessage` | event | any |

The WASM SDK ships **no `readFile` built-in**. Tools must be pure JS / serializable data. Skills use `skillContentMap` on `RuntimeOptions` (no filesystem).

`resourceQuota` is supported in the runner and maps to the kernel `set_resource_quota` event during run setup. The write-rate quota is enforced for kernel memory-write syscalls; WASM does not yet expose runner-level `writeMemory` / `queryMemory` helpers.

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
for await (const event of runner.run({
  sessionId: "demo",
  goal: "...",
  extensions: { enable_thinking: true },
})) {
  if (event.type === "thinking_delta") console.log(event.delta)
}
```

---

## Tools

Tools must be pure functions — no shell, no filesystem.

```typescript
import { tool, LocalExecutionPlane } from "@deepstrike/wasm"

const fetchUrl = tool("fetch_url", "Fetch a URL and return its text.", {
  type: "object",
  properties: { url: { type: "string" } },
  required: ["url"],
}, async ({ url }) => {
  const resp = await fetch(url as string)
  return resp.text()
})

const plane = new LocalExecutionPlane()
plane.register(fetchUrl)
```

---

## Context model (four slots)

Same as Node/Python — only **history** is compressed. WASM exposes `systemStable`, `systemKnowledge`, and `turns` on `call_provider.context`.

```typescript
const runner = new RuntimeRunner({
  provider,
  sessionLog: new InMemorySessionLog(),
  executionPlane: plane,
  maxTokens: 32_000,
  initialMemory: ["User prefers concise answers."],  // → Slot 2
  systemPrompt: "You are a helpful assistant.",       // → Slot 1
})
```

IndexedDB / KV can back `DreamStore` for cross-session memory. Meta-tool retrieval still lands in **history**.

See [docs/concepts/context-slots-compression.md](../docs/concepts/context-slots-compression.md).

---

## Governance

```typescript
import { RuntimeRunner, AnthropicProvider, Governance } from "@deepstrike/wasm"

const gov = new Governance()
gov.blockTool("dangerous_tool")

const runner = new RuntimeRunner({
  provider: new AnthropicProvider(apiKey),
  sessionLog: new InMemorySessionLog(),
  executionPlane: new LocalExecutionPlane(),
  maxTokens: 32_000,
  governance: gov,
})
```

---

## Memory

`WorkingMemory` is an SDK-side scratch pad — not the removed kernel `working` partition. Structured task state renders into Slot 3 (`turns[0]`).

```typescript
import { WorkingMemory } from "@deepstrike/wasm"

const mem = new WorkingMemory()
mem.set("step", 1)
mem.get("step")   // 1
```

For cross-session recall, implement `DreamStore` and set `agentId` on `RuntimeRunner`. In-session `memory(query)` results appear in **history**; preload durable blocks with `initialMemory` → Slot 2.

---

## Knowledge

Runtime `knowledge(query)` results → **history** (tool results). Durable preload → Slot 2 via `initialMemory`.

```typescript
import type { KnowledgeSource } from "@deepstrike/wasm"

class VectorSearch implements KnowledgeSource {
  async retrieve(goal: string, topK = 5): Promise<string[]> {
    const resp = await fetch(`/api/search?q=${encodeURIComponent(goal)}&k=${topK}`)
    return resp.json()
  }
}

const runner = new RuntimeRunner({
  provider,
  sessionLog: new InMemorySessionLog(),
  executionPlane: new LocalExecutionPlane(),
  maxTokens: 32_000,
  maxTurns: 10,
  knowledgeSource: new VectorSearch(),
})
```

---

## Harness

```typescript
import { SinglePassHarness, HarnessLoop } from "@deepstrike/wasm"

// Single pass — always passes
const harness = new SinglePassHarness(runner)
const outcome = await harness.run({ goal: "Write a haiku" })
console.log(outcome.result)

// Eval loop — LLM-judges the output; retries up to 3 times
const loop = new HarnessLoop(runner, evalProvider, { maxAttempts: 3 })
for await (const event of loop.runStreaming({
  goal: "Write a haiku",
  criteria: [
    { text: "Exactly 3 lines", required: true },
    { text: "Contains a seasonal reference", required: false },
  ],
})) {
  if (event.type === "done") console.log(event.verdict.passed, event.verdict.overallScore)
}
```

---

## Signals & interrupts

Delivered signals fold into Slot 3 (`turns[0]`) and are cleared after each render — they do not survive renewal.

```typescript
import { ScheduledPrompt } from "@deepstrike/wasm"
import type { SignalSource, RuntimeSignal } from "@deepstrike/wasm"

// Interrupt from a UI button
document.getElementById("stop")!.onclick = () => runner.interrupt()

// Convert a scheduled prompt to a RuntimeSignal
const prompt = new ScheduledPrompt("Daily standup summary", 1_700_000_000_000)
const signal = prompt.toSignal()
// signal.source === "cron", signal.signalType === "job"

// Feed signals from postMessage (browser) or Cloudflare event
class PostMessageSource implements SignalSource {
  private queue: RuntimeSignal[] = []
  constructor() {
    self.addEventListener("message", (e: MessageEvent) => {
      if (e.data?.source) this.queue.push(e.data as RuntimeSignal)
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
import {
  RuntimeRunner,
  collectText,
  InMemorySessionLog,
  LocalExecutionPlane,
  AnthropicProvider,
} from "@deepstrike/wasm"
import wasmBinary from "@deepstrike/wasm-kernel/deepstrike_wasm_bg.wasm"

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    await init(wasmBinary)
    const runner = new RuntimeRunner({
      provider: new AnthropicProvider(env.ANTHROPIC_KEY),
      sessionLog: new InMemorySessionLog(),
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 32_000,
      maxTurns: 10,
    })
    const result = await collectText(runner.run({
      sessionId: crypto.randomUUID(),
      goal: await request.text(),
    }))
    return new Response(result)
  },
}
```

### Browser (Vite / bundler)

```typescript
import init from "@deepstrike/wasm-kernel"
import { RuntimeRunner, InMemorySessionLog, LocalExecutionPlane, AnthropicProvider } from "@deepstrike/wasm"

await init()
const runner = new RuntimeRunner({
  provider: new AnthropicProvider(import.meta.env.VITE_ANTHROPIC_KEY),
  sessionLog: new InMemorySessionLog(),
  executionPlane: new LocalExecutionPlane(),
  maxTokens: 32_000,
  maxTurns: 10,
})
```

---

## Stream events

| Event type | Fields |
|------------|--------|
| `text_delta` | `delta: string` |
| `thinking_delta` | `delta: string` |
| `usage` | `totalTokens: number` |
| `tool_call` | `id, name, arguments` |
| `tool_result` | `callId, name, content, isError` |
| `done` | `iterations, totalTokens, status` |
| `error` | `message: string` |

`status` mirrors the kernel termination reason: `completed` / `max_turns` / `token_budget` / `timeout` / `user_abort` / `error`.
