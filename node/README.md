# DeepStrike Node.js SDK

Runtime framework built on a Rust kernel. The kernel owns loop control, context compression, governance, signal routing, and memory paging — the SDK owns all I/O (LLM calls, tool execution, disk, long-term memory).

Node.js is the reference SDK for the **Agent OS native profile**: declarative governance and in-kernel signal routing are enabled by default on every run.

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

The correct platform package is selected automatically via `optionalDependencies`.

> **Note:** `@deepstrike/core` is the low-level N-API binding and is managed as an internal dependency of `@deepstrike/sdk`. When developing against a local kernel build, run `npm run test:local-core` from this directory to rebuild the native module from `../crates/deepstrike-node`.

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

Same-session continuity is explicit via `sessionId`:

```typescript
await collectText(runner.run({ sessionId: "chat-1", goal: "My name is Ada." }))
const reply = await collectText(runner.run({ sessionId: "chat-1", goal: "What is my name?" }))
```

Use `InMemorySessionLog` for process-local sessions or `FileSessionLog` when replay should survive restarts. `wake(sessionId)` resumes from the event log without inserting a duplicate `run_started` event.

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

## Architecture

```text
┌─────────────────────────────────────────────────────────┐
│  RuntimeRunner (Layer 1.5)                              │
│  LLMProvider · ExecutionPlane · SessionLog · DreamStore │
└───────────────────────────┬─────────────────────────────┘
                            │ step(JSON event) ↔ actions / observations
┌───────────────────────────▼─────────────────────────────┐
│  @deepstrike/core KernelRuntime                         │
│  P1 Syscall · P2 Sched · P3 MM · Proc · IPC             │
└─────────────────────────────────────────────────────────┘
```

The runner drives a single loop:

1. Kernel returns an **action** — `call_provider`, `execute_tool`, `evaluate_milestone`, or `done`.
2. SDK executes the action (stream LLM, run tools, call milestone verifier).
3. SDK feeds the result back as a kernel **event** (`provider_result`, `tool_results`, …).
4. Kernel **observations** (compression, page-out, spool, signals, …) are drained into `SessionLog`.

Kernel session events carry an optional `category` tag (`syscall` · `sched` · `mm` · `proc` · `ipc`) for diagnostics and OS snapshot rebuilds.

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

`extensions` are forwarded by every provider in both `complete()` and `stream()` while SDK-owned structural fields such as `model`, `messages`, `tools`, and streaming flags remain protected.

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
- Inbound signals are routed by the in-kernel attention policy and rendered into **Slot 3**
- Anthropic: Slots 1–2 get separate `cache_control` breakpoints

Full reference: [docs/concepts/context-slots-compression.md](../docs/concepts/context-slots-compression.md)

---

## Runtime options

```typescript
import {
  DEFAULT_NATIVE_GOVERNANCE_POLICY,
  DEFAULT_NATIVE_ATTENTION_POLICY,
} from "@deepstrike/sdk"

const runner = new RuntimeRunner({
  provider,
  executionPlane: plane,
  sessionLog: new FileSessionLog(".deepstrike/sessions"),

  // Scheduler budget
  maxTokens: 128_000,
  maxTurns: 25,
  timeoutMs: 60_000,
  schedulerBudget: { maxWallMs: 300_000 },

  // Agent OS native profile (defaults shown)
  governancePolicy: DEFAULT_NATIVE_GOVERNANCE_POLICY,
  attentionPolicy: DEFAULT_NATIVE_ATTENTION_POLICY, // SignalRouter queue size 64

  // Host I/O
  extensions: { temperature: 0.1 },
  skillDir: "./skills",
  knowledgeSource: myKS,
  signalSource: gw,
  dreamStore: myStore,
  agentId: "my-agent",
  initialMemory: ["..."],

  // Memory paging & compression (SDK-side I/O)
  compressionStore: archiveStore,       // persist compressed transcript slices
  asyncSummarizer: mySummarizer,        // upgrade rule-based compression summaries
  dreamProvider: dreamLlm,              // LLM for idle dream() synthesis
  dreamSummarizer: myDreamSummarizer,   // LLM for semantic page_out → DreamStore

  // Sub-agents
  runSpec: { role: "orchestrator", isolation: "process" },
  milestoneContract: myContract,
  milestonePolicy: "require_verifier",
  onMilestoneEvaluate: async ({ phaseId, criteria }) => ({ passed: true, phaseId }),
  subAgentHarness: { evalProvider, maxAttempts: 3 },

  // Governance UX (AskUser path)
  onPermissionRequest: async (req) => ({ approved: true }),

  // Diagnostics
  enableDiagnosticsDashboard: true,     // CLI view grouped by Syscall / Sched / MM
})
```

| Option | Purpose |
|--------|---------|
| `governancePolicy` | Declarative deny / ask_user / rate-limit / param rules loaded into the kernel before `start_run` |
| `attentionPolicy` | In-kernel signal router queue size (default 64) |
| `onPermissionRequest` | Resolves `tool_gated` + `suspended` → kernel `resume` with approved/denied call IDs |
| `compressionStore` | Writes archived messages on `compressed` observations |
| `asyncSummarizer` | Background LLM summary after compression; stored as `summary_upgraded` |
| `dreamSummarizer` | Summarizes `page_out { tier_hint: "semantic" }` into `DreamStore` during a run |
| `dreamProvider` | Separate LLM for `dream()` idle consolidation (falls back to `provider`) |

Rebuild an OS diagnostics snapshot from session events:

```typescript
import { rebuildOsSnapshotFromSessionEvents } from "@deepstrike/sdk"

const events = (await sessionLog.read(sessionId)).map(e => e.event)
const snap = rebuildOsSnapshotFromSessionEvents(events)
// snap.pageOutCount, snap.spoolCount, snap.signals, snap.processByAgent, …
```

---

## Large result spool (Layer 1)

When a single tool result exceeds **50 KB**, the kernel keeps a short preview in context and emits `large_result_spooled`. The SDK writes the full payload to `.spool/` under the process cwd (SHA-256 keyed files) and logs `spool_ref` in the session.

The model can retrieve full content via ordinary read tools — `LocalExecutionPlane` transparently resolves paths under `.spool/`:

```typescript
// Kernel context shows a preview + spool reference.
// LLM calls read_file({ path: ".spool/abc123…" }) → full content returned.
```

No configuration is required; customize the directory by passing a `resultSpool` instance when constructing `RuntimeRunner` (see tests under `tests/runtime/large-result-spool.test.ts`).

---

## Tools

```typescript
import { tool, readFile } from "@deepstrike/sdk"

plane.register(tool("search", "Search.", schema, async (args) => ...))
plane.register(readFile)     // built-in: read files from disk (also resolves .spool/ refs)
plane.unregister("search")
```

Execution planes:

| Plane | Use case |
|-------|----------|
| `LocalExecutionPlane` | In-process tools (default) |
| `FilteredExecutionPlane` | Capability-filtered sub-agent tools |
| `ProcessSandboxPlane` | OS subprocess isolation |
| `McpProxyPlane` | MCP server tools |
| `RemoteVpcPlane` | Remote execution |

Mount capabilities on an active run:

```typescript
runner.mountTool(schema)
runner.mountSkill("summarize", "Summarize text")
runner.unmountCapability("tool", "search")
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

Implement `KnowledgeSource` to connect any RAG system. The kernel injects a `knowledge` meta-tool that the LLM calls on demand. Runtime retrieval results land in **history** as tool results.

To inject durable knowledge at startup (Slot 2, cacheable on Anthropic), use `initialMemory` or `runner.pushKnowledge()`.

Before tool execution the kernel may emit `page_in_requested`; the SDK satisfies it from `DreamStore`, `KnowledgeSource`, and a local semantic page-out cache, then feeds `page_in` back to the kernel.

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

`WorkingMemory` is an SDK helper — not the kernel working partition. Kernel task state lives in `task_state` and renders into Slot 3 (`turns[0]`).

```typescript
import { WorkingMemory } from "@deepstrike/sdk"
const mem = new WorkingMemory()
mem.set("step", 1)
mem.get("step")  // 1
mem.clear()
```

### DreamStore (long-term memory)

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
  agentId: "my-agent",  // enables `memory` meta-tool + semantic page-out archival
})
```

Three memory paths:

| Path | When | What happens |
|------|------|--------------|
| In-session `memory(query)` | LLM calls meta-tool | `DreamStore.search()` → history tool result |
| `initialMemory` | Run start | Injected into Slot 2 (`systemKnowledge`) |
| Semantic `page_out` | Kernel evicts with `tier_hint: "semantic"` | SDK summarizes via `dreamSummarizer` / `dreamProvider` → `DreamStore.commit()` |
| `dream(agentId)` | Explicit idle call | `IdlePipeline` batch-consolidates past sessions |

```typescript
// Post-session batch consolidation
const result = await runner.dream("my-agent", Date.now())
```

### Phase-7 memory syscalls (`writeMemory` / `queryMemory`)

Kernel-validated long-term memory I/O outside the main tool loop:

```typescript
await runner.writeMemory({
  metadata: {
    name: "prefers-small-tests",
    description: "User prefers focused unit tests",
    kind: "feedback",
    created_at: Date.now(),
    updated_at: Date.now(),
  },
  content: "User prefers focused unit tests for SDK behavior.",
}, { sessionId: "my-session" })

const hits = await runner.queryMemory({
  current_context: "Need testing preferences",
  active_tools: [],
  already_surfaced: [],
  top_k: 5,
}, { sessionId: "my-session" })
```

Session events: `memory_written`, `memory_queried`, `memory_validation_failed`, `memory_retrieval_result`.

---

## Governance

### In-kernel declarative policy (preferred)

Every run loads `governancePolicy` into the kernel via `load_governance_policy`. The kernel enforces rules **before** tools execute:

```typescript
import type { GovernancePolicy } from "@deepstrike/sdk"

const policy: GovernancePolicy = {
  rules: [
    { pattern: "read_file", action: "allow" },
    { pattern: "write_file", action: "ask_user" },
    { pattern: "run_command", action: "ask_user" },
    { pattern: "*", action: "deny" },
  ],
  rateLimits: [{ tool: "api_call", maxCalls: 10, windowMs: 60_000 }],
}

const runner = new RuntimeRunner({
  provider,
  executionPlane: plane,
  sessionLog,
  governancePolicy: policy,
  onPermissionRequest: async (req) => {
    console.log(`Approve ${req.toolName}?`, req.arguments)
    return { approved: true }
  },
})
```

- `deny` → tool rejected with `tool_denied`
- `ask_user` → `tool_gated` + `suspended`; resolve via `onPermissionRequest`, then kernel `resume`

Default when omitted: allow-all (`DEFAULT_NATIVE_GOVERNANCE_POLICY`).

### Standalone Governance class

`Governance` wraps the native governance evaluator for SDK-side use (tests, custom gates). It is **not** wired automatically into `RuntimeRunner` — use `governancePolicy` for run-time enforcement.

```typescript
import { Governance } from "@deepstrike/sdk"

const gov = new Governance("allow")
gov.addPermissionRule("danger.*", "deny")
gov.blockTool("rm_rf")
gov.evaluate("read_file", '{"path":"x"}')
```

### SDK PermissionManager

`PermissionManager` is a separate SDK-side permission layer for apps that manage their own approval UX outside the kernel loop.

---

## Signals

Inbound signals are routed by the in-kernel attention policy (default queue size 64):

| Urgency | Typical disposition |
|---------|-------------------|
| `critical` / `high` | `interrupt_now` — may yield a new `call_provider` action |
| `normal` / `low` | `queue` — buffered; no action until dequeued |
| queue full | `dropped` |

```typescript
import { SignalGateway, ScheduledPrompt } from "@deepstrike/sdk"

const gw = new SignalGateway()
gw.schedule(new ScheduledPrompt("standup", Date.now() + 3600_000))
gw.ingest({ kind: "alert", urgency: "normal", payload: { goal: "Check deploy" } })

const runner = new RuntimeRunner({
  provider,
  executionPlane: plane,
  sessionLog,
  signalSource: gw,
  attentionPolicy: { maxQueueSize: 64 },
})

runner.interrupt() // cooperative abort → kernel timeout path
gw.destroy()
```

Each routed signal produces a `signal_disposed` session event (`category: "ipc"`).

---

## Sub-agents

Spawn isolated child agents through the kernel process table:

```typescript
for await (const evt of runner.spawnSubAgent({
  role: "researcher",
  isolation: "process",
  goal: "Find three sources on topic X",
  criteria: ["At least 3 URLs"],
})) {
  if (evt.type === "done") console.log(evt.status)
}
```

Requires an active parent run (`run()` / `wake()` in progress). The kernel emits `agent_process_changed`; the default `SubAgentOrchestrator` runs the child with a filtered execution plane and feeds `sub_agent_completed` back.

---

## Harness (evaluation framework)

```typescript
import { SinglePassHarness, EvalLoopHarness, HarnessLoop } from "@deepstrike/sdk"

const outcome = await new SinglePassHarness(runner).run({ goal: "Say hello" })

const harness = new EvalLoopHarness(runner, {
  async evaluate(_req, out) { return out.result.includes("hello") },
}, 3)

const loop = new HarnessLoop(runner, evalProvider, { maxAttempts: 3, skillDir: "./skills" })

const runnerWithHarness = new RuntimeRunner({
  provider,
  executionPlane: plane,
  sessionLog,
  subAgentHarness: { evalProvider, maxAttempts: 3 },
})
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

`status`: `completed` · `max_turns` · `token_budget` · `timeout` · `user_abort` · `error` · `milestone_pending`

---

## Further reading

- [SDK OS parity matrix](../docs/sdk-os-parity.md)
- [Kernel ABI reference](../docs/reference/kernel-abi.md)
- [Context slots & compression](../docs/concepts/context-slots-compression.md)
