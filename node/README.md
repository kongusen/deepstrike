<p align="center">
  <a href="https://github.com/kongusen/deepstrike">
    <img src="https://raw.githubusercontent.com/kongusen/deepstrike/main/docs/public/banner.png" alt="DeepStrike" width="420" />
  </a>
</p>

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

### Package layout (v0.2.30)

The root export is the **intent layer** — what you reach for to run an agent, run a workflow, author a tool, or pick a provider (~30 symbols). Advanced machinery lives behind subpaths, so the common surface stays small and tree-shakeable:

| Import | Contains |
|--------|----------|
| `@deepstrike/sdk` | `runAgent` · `runFanout` · `RuntimeRunner` · `tool` · `LocalExecutionPlane` · `InMemorySessionLog`/`FileSessionLog` · `AnthropicProvider`/`OpenAIProvider`/`OpenAIResponsesProvider` · `createProvider` · `Governance` · `AgentPool` · core types |
| `@deepstrike/sdk/providers` | backend factories (`deepseek`, `kimi`, `qwen`, `glm`, `minimax`, `gemini`, `ollama`), profiles, `CircuitBreaker` |
| `@deepstrike/sdk/workflow` | `SubAgentOrchestrator`, `spawnStandalone`, reducers, contracts, handoff/modes, agent + spec types |
| `@deepstrike/sdk/planes` | `WorktreeExecutionPlane`, `ProcessSandboxPlane`, `McpProxyPlane`, `RemoteVpcPlane`, archive/credential stores |
| `@deepstrike/sdk/memory` | `DreamStore`, `WorkingMemory`, `InMemoryDreamStore`, `KnowledgeSource` |
| `@deepstrike/sdk/harness` | `SinglePassHarness`, `EvalLoopHarness`, `HarnessLoop`, `judge` |
| `@deepstrike/sdk/os` | profiles, `KernelPrimitivesDashboard`, signals, `PermissionManager`, replay-testing utilities |

> **Migration from 0.2.x:** the kernel-lowering converters (`*ToKernel`), low-level prompt/eval builders, and the `OpenAIChatProvider` alias are no longer exported from root; backend providers, planes, memory, harness, and OS utilities moved to the subpaths above. See [`MIGRATION-v0.2.300.md`](./MIGRATION-v0.2.300.md).

### Recipes — the canonical entry points

Most apps need one of three shapes. Start with the facades and drop down to `RuntimeRunner` only when you need streaming, signals, memory, or governance hooks.

```typescript
import { runAgent, runFanout } from "@deepstrike/sdk"

// 1) Single agent — one prompt, one model, the text back.
const answer = await runAgent({ provider, goal: "What is 17 + 28?", tools: [add] })

// 2) Parallel fan-out → synthesize — N workers, then a synthesis pass, over the kernel-gated DAG.
//    Bootstraps and tears down its own kernel, so it's safe from a stateless request handler.
const { synthesis } = await runFanout({
  provider,
  tasks: [
    "Summarize the security posture of the auth module",
    "Summarize the data-retention posture",
  ],
  synthesize: "Combine the worker findings into one risk summary.",
})

// 3) Full control — sub-agents, governance, signals, streaming, resume → use RuntimeRunner directly.
```

`runFanout` is sugar over the **standalone `runWorkflow`** path: with no active `run()`, `runner.runWorkflow(spec)` auto-bootstraps a kernel that owns the DAG (governed · resumable), drives it, and tears it down — exactly what a Vercel/Lambda handler needs. See [Dynamic workflows](#dynamic-workflows). For parallel work you can also give each worker its own `RuntimeRunner`; `RuntimeRunner` carries per-run state, so **never share one instance across concurrent runs** — use a fresh instance per worker (or the `AgentPool` primitive).

### Deploying to serverless / bundlers

`@deepstrike/core` is a native N-API addon; its platform binary ships via `optionalDependencies`. Bundlers (Next.js/Vercel, webpack, esbuild) don't trace `.node` files by default, so the function fails at runtime with `Cannot find module '@deepstrike/core'`. Tell your bundler to treat the package as external and trace its files:

```ts
// next.config.ts (Next.js / Vercel)
const nextConfig = {
  serverExternalPackages: ["@deepstrike/sdk"],
  outputFileTracingIncludes: {
    "/api/**": ["./node_modules/@deepstrike/**/*"],
  },
}
export default nextConfig
```

- **webpack:** add `@deepstrike/core` (and `@deepstrike/sdk`) to `externals`, or use `node-loader` for `.node` files.
- **esbuild:** `--external:@deepstrike/core --external:@deepstrike/sdk` and ensure the platform binary is copied next to the bundle.
- **Docker/standalone:** the build host's platform binary must match the runtime's (e.g. build on `linux-x64-gnu` for Vercel). Alpine images need the `-musl` binary.

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

### What Agent OS gives you

The mechanisms above are not internal refactors — they change what you can build without custom runner code:

**Kernel-mediated runtime (M0–M4)**  
Tool calls, spawns, compression, and signals pass through one kernel gate with an explicit lifecycle (Ready / Running / Blocked / Suspended). You implement I/O; the kernel decides *when* and *whether*. Node, Python, and Rust share the same decision path, so `wake(sessionId)` and cross-language tooling see consistent behavior.

**Longer, sturdier sessions (Layer-1 spool + semantic page-out)**  
Oversized tool results (> 50 KB) stay in context as a preview plus a `.spool/` reference — the model reads the full payload on demand via ordinary file tools. When pressure triggers semantic eviction, the SDK summarizes archived content into `DreamStore`. Long tasks survive token pressure instead of failing mid-run.

**Safety and governance by default (OS native profile)**  
Every run loads declarative `governancePolicy` (deny / ask_user / rate-limit / param rules) and in-kernel signal routing (`attentionPolicy`, default queue 64). Dangerous tools, external interrupts, and approval flows are policy — not ad-hoc `if` checks in your handlers.

**Long-term memory as syscalls (Phase-7)**  
`writeMemory` and `queryMemory` run outside the main tool loop: kernel validation before `DreamStore.commit`, search → `selectMemories` → `memory_retrieval_result` on query. Failed writes emit `memory_validation_failed` for audit; good memory is durable without polluting history.

**Multi-agent and multi-signal orchestration**  
Sub-agents register in the kernel process table (`agent_process_changed`); parent runs suspend explicitly until `sub_agent_completed`. Signals get disposition (Interrupt / Queue / Observe / Dropped) in-kernel, so gateways, cron, and heartbeats compose with the main loop instead of racing it.

**Observable like an OS log**  
Spool, page-out, signals, processes, budgets, and memory events land in `SessionLog` with categories. Rebuild an OS snapshot (`pageOutCount`, `spoolCount`, `processByAgent`, memory counters) from one event stream — replay still strips audit events when reconstructing LLM messages.

| You need… | Use… |
|---|---|
| Policy before tools run | `governancePolicy` (default: allow-all native profile) |
| External interrupts | `signalSource` + in-kernel `attentionPolicy` |
| Huge tool output | Automatic Layer-1 spool; optional custom `resultSpool` |
| Durable recall across runs | `DreamStore` + semantic `page_out` via `dreamSummarizer` |
| Programmatic memory I/O | `runner.writeMemory()` / `runner.queryMemory()` |
| Debug / compliance | `SessionLog` events + OS snapshot helpers |

---

## Dynamic workflows

Instead of planning **and** executing a hard task in one long context window, hand the kernel a declarative DAG and let it spawn a fresh-context sub-agent per node. The kernel owns the control flow (gate · budget · suspend-on-join · resume); your SDK runs the agents. See the [top-level overview](../README.md#the-six-harness-patterns-as-first-class-kernel-nodes) for the full pattern catalog.

```ts
// One fresh-context verifier per rule (no inherited author context → can't rubber-stamp),
// then a skeptic that reviews their flags. The kernel spawns the 3 verifiers as one gated
// batch, suspends on the join, and runs the skeptic once they complete.
const outcome = await runner.runWorkflow({
  nodes: [
    { task: "Rule: money is integer cents — violated?", role: "verify" },
    { task: "Rule: all errors propagate — violated?",    role: "verify" },
    { task: "Rule: timestamps are UTC — violated?",       role: "verify" },
    { task: "Skeptic: which flags are real violations?",  role: "verify", dependsOn: [0, 1, 2] },
  ],
})
// → { completed: ["wf-node0", … ], failed: [], outputs: { "wf-node3": "…" } }
```

`runWorkflow` works **standalone** — call it on a freshly-constructed runner (e.g. inside a stateless HTTP handler) and it auto-bootstraps a kernel that owns the DAG, drives it under the same governance/quota/attention policies a full `run()` gets, and tears it down on completion. Called *during* a `run()`, it instead drives the workflow on the active kernel. Either way every node's final text comes back in `outputs`, keyed by node agent-id. To resume an interrupted standalone run, pass the prior session id: `runner.resumeWorkflow(spec, { sessionId })`.

A node's `kind` selects the control-flow shape; the same executor drives them all, every spawn passing the syscall gate:

| Node `kind` | Behavior |
|---|---|
| `{ type: "spawn" }` (default) | Run the node's agent once |
| `{ type: "loop", maxIters }` | Re-run until the agent signals it's done, capped at `maxIters` |
| `{ type: "classify", branches }` | The classifier's result selects one branch; the rest are pruned |
| `{ type: "tournament", entrants }` | Generate N entrants, then a pairwise-judge bracket to one winner |
| `{ type: "reduce", reducer }` | **Tokenless host-compute** — a pure function (`dedupe_lines` / `merge_json_arrays` / `concat` / `count`, or your own via the `reducers` runner option) over the node's dependency outputs |

### 0.2.11 capabilities

- **Runtime fan-out** — give a node the `submitWorkflowNodesTool` and its agent can append nodes to the live DAG mid-run (true loop-until-done; one verifier per claim it discovers). Recorded and replayed on `resumeWorkflow`.
- **Quarantine, no escape** — set `trust: "quarantined"` on a node that reads untrusted content; it's denied write-capable isolation in-kernel, and any nodes it submits are coerced to quarantined too (no privilege escalation).
- **Structured output** — set `outputSchema` on a node; the runner instructs the agent, validates the result against the JSON-Schema subset, and re-runs once with the errors on mismatch. A node that never conforms fails (its dependents starve).
- **Budget as signal** — with a `maxWorkflowNodes` / `maxConcurrentSubagents` quota installed, each spawned node's goal carries its remaining headroom so a coordinator can size its fan-out to fit.

---

## Providers

The root package exports the three base providers — `AnthropicProvider`, `OpenAIProvider`,
`OpenAIResponsesProvider` — plus `createProvider`. **Every other backend is a factory function** in
`@deepstrike/sdk/providers`: one per backend, with a `protocol` option where a backend speaks both the
OpenAI- and Anthropic-compatible wire.

```typescript
import { deepseek, kimi, minimax } from "@deepstrike/sdk/providers"

const ds = deepseek({ apiKey })                          // OpenAI-compatible wire (default)
const dsA = deepseek({ apiKey, protocol: "anthropic" })  // Anthropic-compatible wire
const mm = minimax({ apiKey })                           // MiniMax defaults to the Anthropic wire
```

| Entry | Import from | Backend |
|-------|-------------|---------|
| `OpenAIProvider` | root | OpenAI Chat Completions (and any OpenAI-compatible `/v1`) |
| `OpenAIResponsesProvider` | root | OpenAI Responses API (`previous_response_id` continuation) |
| `AnthropicProvider` | root | Anthropic Messages API (`ThinkingDelta` support) |
| `deepseek` · `kimi` · `qwen` · `glm` · `minimax` · `gemini` · `ollama` | `@deepstrike/sdk/providers` | the respective vendor (factory functions) |

Providers take an **options object** and share a `CircuitBreaker`. `extensions` are forwarded in both
`complete()` and `stream()`; SDK-owned fields (`model`, `messages`, `tools`, streaming flags) stay protected.

**Custom OpenAI-compatible endpoint** (MiMo, DeepSeek, Kimi, Qwen, GLM via their `/v1` base URL): construct
`OpenAIProvider` with an options object — no more positional `baseURL` hole:

```typescript
import { OpenAIProvider } from "@deepstrike/sdk"

const provider = new OpenAIProvider({
  apiKey,
  model: "mimo-v2.5-pro",
  baseURL: "https://token-plan-cn.xiaomimimo.com/v1",
})
```

Prefer a dedicated backend class from `@deepstrike/sdk/providers` when one exists — they default the base
URL and add backend-specific reasoning handling. Any model can also be selected through the catalog: `createProvider` picks the protocol/endpoint for you:

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
| `systemKnowledge` | knowledge partition | Skill bodies, `initialMemory`, host-pinned durable refs — keyed, boundary-evicted, budgeted |
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

  // Resource quotas (M2) — enforced at the kernel syscall trap. Opt-in; omit for unbounded.
  resourceQuota: {
    maxConcurrentSubagents: 4,                       // deny spawn while at cap
    maxSpawnDepth: 2,                                // deny spawn past nesting depth
    memoryWritesPerWindow: { maxWrites: 20, windowMs: 60_000 }, // rate-limit writeMemory
  },

  // Long-term memory policy (set_memory_policy) — opt-in, kernel-enforced; omit for defaults.
  memoryPolicy: {
    memoryPath: "./.memory",     // where the SDK persists/scans memories (SDK-consumed)
    staleWarningDays: 30,        // flag recalled memories older than this (SDK-consumed)
    retrievalTopK: 5,            // kernel caps query_memory requested_k to this
    validationEnabled: true,     // false → admit writes without validation
    maxContentBytes: 10_000,     // override write_memory content-size limit
    maxNameLength: 100,          // override write_memory name-length limit
  },

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
| `resourceQuota` | M2 declarative limits — `maxConcurrentSubagents` / `maxSpawnDepth` / `memoryWritesPerWindow` — enforced at the kernel syscall trap (`set_resource_quota`); over-quota spawns roll back, over-rate writes surface as `memory_validation_failed` |
| `memoryPolicy` | Long-term memory config sent as `set_memory_policy` and **kernel-enforced**: `validationEnabled: false` admits writes without validation, `maxContentBytes` / `maxNameLength` override validation limits, `retrievalTopK` caps `query_memory` breadth; `memoryPath` / `staleWarningDays` are SDK-consumed (requires `dreamStore` + `agentId` to enable memory) |
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

A loaded skill's body is pinned into the durable `knowledge` slot (keyed `skill:<name>`) and its `allowed_tools` narrow the exposed toolset. Activation is **not** permanent: `runner.deactivateSkill(name)` re-widens the toolset at the next provider call and unpins the body at the next boundary, and `skillLeaseTurns` auto-deactivates every skill N turns after it loads — so a long multi-phase run doesn't monotonically accumulate early-phase skills. There is deliberately no model-facing unload (deactivation is host-driven only).

---

## Knowledge

Implement `KnowledgeSource` to connect any RAG system. The kernel injects a `knowledge` meta-tool that the LLM calls on demand. Runtime retrieval results land in **history** as tool results (single-use fact content that decays with the compression pyramid) — not in the durable `knowledge` partition.

To inject durable knowledge at startup (Slot 2, cacheable on Anthropic), use `initialMemory` or `runner.pushKnowledge(content, tokens?, { key, pinned })`. A **keyed** entry upserts on a repeated key and can be removed with `runner.removeKnowledge(key)`; both take effect at the next compaction/renewal boundary (where the `system[1]` cache prefix is rewritten anyway). Set `knowledgeBudgetRatio` (default 0.25 of `maxTokens`, 0 disables) to cap the partition — over budget, the oldest unpinned, non-skill entries are evicted at boundaries while `pinned: true` entries survive.

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
