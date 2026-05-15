# DeepStrike Node.js SDK

Agent framework built on a Rust kernel. The kernel handles loop control, context compression, skill routing, governance, signal prioritization — the SDK handles all I/O.

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
import { Agent, OpenAIProvider, tool } from "@deepstrike/sdk"

const provider = new OpenAIProvider(process.env.OPENAI_API_KEY!, "gpt-5-mini")

const add = tool("add", "Add two numbers.", {
  type: "object",
  properties: { x: { type: "number" }, y: { type: "number" } },
  required: ["x", "y"],
}, async ({ x, y }) => String(Number(x) + Number(y)))

const agent = new Agent(provider, { maxTokens: 4096 })
agent.register(add)

const result = await agent.run("What is 17 + 28?")
console.log(result)
```

Streaming:

```typescript
for await (const event of agent.runStreaming("Summarize README.md")) {
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
| `OpenAIProvider` | OpenAI API | SSE tool-call accumulation |
| `AnthropicProvider` | Anthropic API | Native SSE, `ThinkingDelta` support |
| `QwenProvider` | DashScope | `enable_thinking` via extensions |
| `DeepSeekProvider` | DeepSeek API | Reasoner models strip tools automatically |
| `MiniMaxProvider` | MiniMax API | M1 reasoning via `expose_reasoning` |
| `OllamaProvider` | Local Ollama | `http://localhost:11434` default |
| `KimiProvider` | Moonshot API | |

All providers accept `RetryConfig` for exponential backoff and share a `CircuitBreaker`.

---

## Agent options

```typescript
const agent = new Agent(provider, {
  maxTokens: 4096,            // context window size
  maxTurns: 25,               // max turns (default 25)
  timeoutMs: 60_000,          // timeout in ms
  extensions: { temperature: 0.1 },  // pass-through to LLM
  skillDir: "./skills",       // skill .md files directory
  knowledgeSource: myKS,      // KnowledgeSource implementation
  signalSource: rx,           // SignalSource for external signals
  dreamStore: myStore,        // DreamStore for long-term memory
  agentId: "my-agent",        // required with dreamStore for memory meta-tool
  governance: gov,            // Governance pipeline instance
})
```

---

## Tools

```typescript
import { tool, readFile } from "@deepstrike/sdk"

agent.register(tool("search", "Search.", schema, async (args) => ...))
agent.register(readFile)     // built-in: read files from disk
agent.unregister("search")
```

---

## Skills

Skills are `.md` files with YAML frontmatter. Set `skillDir` on the agent — the kernel auto-injects a `skill` meta-tool, and the LLM loads skills by name on demand.

```typescript
const agent = new Agent(provider, { maxTokens: 4096, skillDir: "./skills" })
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

Implement `KnowledgeSource` to connect any RAG system. The kernel injects a `knowledge` meta-tool that the LLM calls on demand.

```typescript
const agent = new Agent(provider, {
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

### WorkingMemory (in-session scratch pad)

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

const agent = new Agent(provider, {
  maxTokens: 4096,
  dreamStore: new MyStore(),
  agentId: "my-agent",  // enables `memory` meta-tool
})

// In-session: LLM calls memory(query) → DreamStore.search()
// Post-session: trigger memory consolidation
const result = await agent.dream("my-agent", Date.now())
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

const agent = new Agent(provider, { maxTokens: 4096, governance: gov })
// Every tool call goes through: Permission → Veto → RateLimit → Constraint → Audit
```

---

## Signals

```typescript
import { SignalGateway, ScheduledPrompt } from "@deepstrike/sdk"

const gw = new SignalGateway()
gw.schedule(new ScheduledPrompt("standup", Date.now() + 3600_000))
gw.ingest({ kind: "interrupt", urgency: "critical", payload: {} })

const agent = new Agent(provider, { maxTokens: 4096, signalSource: gw })
// kind="interrupt" → immediately stops the running agent

agent.interrupt() // also works directly
gw.destroy()
```

---

## Harness (evaluation framework)

```typescript
import { SinglePassHarness, EvalLoopHarness, HarnessLoop } from "@deepstrike/sdk"

// 1. SinglePass — run once, always passes
const outcome = await new SinglePassHarness(agent).run({ goal: "Say hello" })

// 2. EvalLoop — retry until QualityGate passes
const harness = new EvalLoopHarness(agent, {
  gate: async (req, out) => out.result.includes("hello"),
  maxAttempts: 3,
})

// 3. HarnessLoop — LLM-as-judge with feedback injection + skill extraction
const loop = new HarnessLoop(agent, {
  evalProvider,
  maxAttempts: 3,
  skillDir: "./skills",
})
const out = await loop.run({ goal: "Write a haiku", criteria: ["Must be 3 lines"] })
console.log(out.passed, out.feedback)
```

---

## Stream events

| Event type | Key fields |
|------------|------------|
| `text_delta` | `delta` |
| `thinking_delta` | `delta` |
| `tool_call` | `id`, `name`, `arguments` |
| `tool_result` | `callId`, `content`, `isError` |
| `permission_request` | `toolName`, `reason` |
| `done` | `iterations`, `totalTokens`, `status` |
| `error` | `message` |

`status`: `completed` · `max_turns` · `token_budget` · `timeout` · `user_abort` · `error`
