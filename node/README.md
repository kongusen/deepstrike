<p align="center">
  <a href="https://github.com/kongusen/deepstrike">
    <img src="https://raw.githubusercontent.com/kongusen/deepstrike/main/docs/public/banner.png" alt="DeepStrike" width="420" />
  </a>
</p>

# DeepStrike Node.js SDK

Build Node.js Agents with providers, typed tools, memory, Skills, delegation, workflows, and durable sessions. The SDK keeps the Agent's long-running work explicit through stream events, SessionLog evidence, tool policies, and host-provided integrations.

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

### Package layout

The root export is the **intent layer** — what you reach for to run an agent, run a workflow, author a tool, or pick a provider (~30 symbols). Advanced machinery lives behind subpaths, so the common surface stays small and tree-shakeable:

| Import | Contains |
|--------|----------|
| `@deepstrike/sdk` | `runAgent` · `runFanout` · `RuntimeRunner` · `tool` · `LocalExecutionPlane` · `InMemorySessionLog`/`FileSessionLog` · `AnthropicProvider`/`OpenAIProvider`/`OpenAIResponsesProvider` · `createProvider` · `Governance` · `AgentPool` · `operationAbortSignal` · core types |
| `@deepstrike/sdk/providers` | backend factories (`deepseek`, `kimi`, `qwen`, `glm`, `minimax`, `gemini`, `ollama`), profiles, `CircuitBreaker` |
| `@deepstrike/sdk/workflow` | `SubAgentOrchestrator`, `spawnStandalone`, reducers, contracts, handoff/modes, agent + spec types |
| `@deepstrike/sdk/planes` | `WorktreeExecutionPlane`, `ProcessSandboxPlane`, `McpProxyPlane`, `RemoteVpcPlane`, archive/credential stores |
| `@deepstrike/sdk/memory` | `MemoryStore`, `WorkingMemory`, `InMemoryMemoryStore`, `rankMemories`, `extractSessionMemories`, `KnowledgeSource` |
| `@deepstrike/sdk/harness` | `AttemptLoop`, body/judge/carry policies, `judge` |
| `@deepstrike/sdk/os` | profiles, `KernelPrimitivesDashboard`, `primitiveForKind` / `KernelPrimitive`, signals, `PermissionManager`, replay-testing utilities |

> **Migration from 0.2.x:** the kernel-lowering converters (`*ToKernel`), low-level prompt/eval builders, and the `OpenAIChatProvider` alias are no longer exported from root; backend providers, planes, memory, harness, and OS utilities moved to the subpaths above. See [`MIGRATION-v0.2.30.md`](./MIGRATION-v0.2.30.md).

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
│  LLMProvider · ExecutionPlane · SessionLog · MemoryStore │
└───────────────────────────┬─────────────────────────────┘
                            │ durable prepare / append / commit
┌───────────────────────────▼─────────────────────────────┐
│  @deepstrike/core Canonical Kernel ABI                  │
│  P1 Syscall · P2 Sched · P3 MM · Proc · IPC             │
└─────────────────────────────────────────────────────────┘
```

The runner drives one durable operation loop:

1. The host prepares one canonical envelope and durably appends the exact core-produced record.
2. After commit, the kernel publishes typed **effects** such as provider, tool, or task work.
3. The SDK executes those effects and returns each outcome through the single `resolve_effect` input.
4. Typed observations and the terminal disposition are projected into `SessionLog` and the public stream.

Kernel session events carry an optional `category` tag (`syscall` · `sched` · `mm` · `proc` · `ipc`) for diagnostics and OS snapshot rebuilds.

### What this enables

The mechanisms above are not internal refactors — they change what you can build without custom runner code:

**Kernel-mediated runtime (M0–M4)**  
Tool calls, spawns, compression, and signals pass through one kernel gate with an explicit lifecycle (Ready / Running / Blocked / Suspended). You implement I/O; the kernel decides *when* and *whether*. Node, Python, and Rust share the same decision path, so `wake(sessionId)` and cross-language tooling see consistent behavior.

**Longer, sturdier sessions (external payloads + semantic page-out)**
The host atomically persists oversized tool results before submitting an `External` result. Core journals only the opaque locator, digest, size, and preview; `read_result` becomes a correlated `LoadPayload` effect. When pressure triggers semantic eviction, the SDK summarizes archived content into `MemoryStore`.

**Safety and governance by default (OS native profile)**  
Every run loads declarative `governancePolicy` (deny / ask_user / rate-limit / param rules) and in-kernel signal routing (`signalPolicy`, default queue 64). Dangerous tools, external interrupts, and approval flows are policy — not ad-hoc `if` checks in your handlers.

**Long-term memory as syscalls (Phase-7)**  
`writeMemory` and `queryMemory` run outside the main tool loop: kernel validation happens before `MemoryStore.put`, while queries call `MemoryStore.search` and journal `memory_retrieval_result`. Failed writes emit `memory_validation_failed` for audit; good memory is durable without polluting history.

**Multi-agent and multi-signal orchestration**  
Sub-agents register in the kernel process table (`agent_process_changed`); parent runs suspend explicitly until `sub_agent_completed`. Signals get disposition (Interrupt / Queue / Observe / Dropped) in-kernel, so gateways, cron, and heartbeats compose with the main loop instead of racing it.

**Observable like an OS log**  
Page-out, signals, processes, budgets, and memory events land in `SessionLog` with categories. Rebuild an OS snapshot (`pageOutCount`, `processByAgent`, memory counters) from one event stream; payload residency stays in the canonical journal.

| You need… | Use… |
|---|---|
| Policy before tools run | `governancePolicy` (default: allow-all native profile) |
| External interrupts | `signalSource` + in-kernel `signalPolicy` |
| Huge tool output | Canonical external payload; optional custom `payloadStore` |
| Durable recall across runs | `MemoryStore` + semantic `page_out` via `memorySummarizer` |
| Programmatic memory I/O | `runner.writeMemory()` / `runner.queryMemory()` |
| Debug / compliance | `SessionLog` events + OS snapshot helpers |

---

## Dynamic workflows

For a task that needs more than one Agent, describe a DAG and let the runtime run fresh-context specialists under the same budgets, policies, and session evidence as the parent. See the [workflow guide](../docs/en/guides/workflow.md) for the complete pattern catalog.

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
const completed = outcome.nodeOutcomes.filter(node => node.status === "completed")
const partial = outcome.nodeOutcomes.filter(node => node.status === "completed_partial")
const failed = outcome.nodeOutcomes.filter(node => node.status === "failed")
// outcome.outputs["wf-node3"] contains the skeptic's final text.
// outcome.rejection is present only when the whole workflow was rejected before any node ran.
```

`runWorkflow` works **standalone** — call it on a freshly-constructed runner (e.g. inside a stateless HTTP handler) and it auto-bootstraps a kernel that owns the DAG, drives it under the same governance/quota/attention policies a full `run()` gets, and tears it down on completion. Called *during* a `run()`, it instead drives the workflow on the active kernel. Either way every node's final text comes back in `outputs`, keyed by node agent-id.

A workflow node has no public `kind` field. Its control-flow shape is selected by one of four
mutually exclusive fields; omit all four for a normal spawn. The same executor drives every shape,
and every spawn passes the syscall gate:

| Public `WorkflowNodeSpec` field | Behavior |
|---|---|
| none (default) | Run the node's agent once |
| `loop: { maxIters }` | Re-run until the agent signals it's done, capped at `maxIters` |
| `classify: { branches }` | The classifier's result selects one branch; the rest are pruned |
| `tournament: { entrants }` | Generate N entrants, then a pairwise-judge bracket to one winner |
| `reducer: "concat"` | **Tokenless host-compute** — a pure function (`dedupe_lines` / `merge_json_arrays` / `concat` / `count`, or your own via the `reducers` runner option) over the node's `dependsOn` outputs |

Dependencies use `dependsOn: number[]`, where each number is a node index, and `depPolicy` controls
how upstream terminal states gate the node (`all_success` by default, plus `accept_partial`,
`all_terminal`, and `optional`).

### Workflow capabilities

- **Runtime fan-out** — register `submitWorkflowNodesTool` on the parent execution plane and a trusted node can append nodes to the live DAG mid-run (true loop-until-done; one verifier per discovered claim). The tool schema is exported from `@deepstrike/sdk/workflow`, not the package root. Submission events remain audit projections; checkpoint state owns recovery. Governance rejection fails the submitting node instead of acknowledging work that was never appended.
- **Quarantine, no escape** — set `trust: "quarantined"` on a node that reads untrusted content; it's denied write-capable isolation in-kernel, and any nodes it submits are coerced to quarantined too (no privilege escalation).
- **Structured output** — set `outputSchema` on a node; the runner instructs the agent, validates the result against the JSON-Schema subset, and re-runs once with the errors on mismatch. A node that never conforms fails (its dependents starve).
- **Budget as signal** — set `resourceQuota.maxWorkflowNodes` and/or `resourceQuota.maxConcurrentSubagents`; each spawned node's goal carries its remaining headroom so a coordinator can size its fan-out to fit.

`submitWorkflowNodesTool` is a `ToolSchema`, while execution planes register `RegisteredTool`
instances. Adapt it once on the parent plane; the runner intercepts the call before the placeholder
handler executes:

```typescript
import { tool } from "@deepstrike/sdk"
import { submitWorkflowNodesTool } from "@deepstrike/sdk/workflow"

plane.register(tool(
  submitWorkflowNodesTool.name,
  submitWorkflowNodesTool.description,
  JSON.parse(submitWorkflowNodesTool.parameters),
  async () => "", // intercepted by RuntimeRunner
))
```

Trusted workflow nodes inherit the parent plane, so the tool is visible to all of them. There is no
per-node tool allowlist on `WorkflowNodeSpec`; quarantined nodes use a filtered, deny-all plane.

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
  DEFAULT_NATIVE_SIGNAL_POLICY,
} from "@deepstrike/sdk/os"

const runner = new RuntimeRunner({
  provider,
  executionPlane: plane,
  sessionLog: new FileSessionLog(".deepstrike/sessions"),

  // Scheduler budget
  maxTokens: 128_000,
  maxTurns: 25,
  timeoutMs: 60_000,
  schedulerPolicy: {
    version: 1,
    criticalPathWeight: 1_000_000,
    fanoutWeight: 10_000,
    ageWeight: 1_000,
    tokenCostWeight: 1,
  },

  // Resource quotas (M2) — enforced at the kernel syscall trap. Opt-in; omit for unbounded.
  resourceQuota: {
    maxConcurrentSubagents: 4,                       // deny spawn while at cap
    maxTotalSubagents: 20,                           // cumulative cap across the RunGroup
    maxSpawnDepth: 2,                                // deny spawn past nesting depth
    maxWorkflowNodes: 32,                            // cap one live DAG, including submitted nodes
    memoryWritesPerWindow: { maxWrites: 20, windowMs: 60_000 }, // rate-limit writeMemory
  },

  // Canonical long-term memory policy — opt-in, kernel-enforced; omit for defaults.
  memoryPolicy: {
    staleWarningDays: 30,        // flag recalled memories older than this
    retrievalTopK: 5,            // kernel caps query_memory requested_k to this
    validationEnabled: true,     // false → admit writes without validation
    maxContentBytes: 10_000,     // override write_memory content-size limit
    maxNameLength: 100,          // override write_memory name-length limit
  },

  // Default governance and signal policy
  governancePolicy: DEFAULT_NATIVE_GOVERNANCE_POLICY,
  signalPolicy: DEFAULT_NATIVE_SIGNAL_POLICY, // SignalRouter queue size 64
  promptBudget: {
    promptOverheadTokens: 20,
    outputReserveTokens: 4096,
    safetyMarginTokens: 256,
  },

  // Host I/O
  extensions: { temperature: 0.1 },
  skillDir: "./skills",
  knowledgeSource: myKS,
  signalSource: gw,
  memoryStore: myStore,
  agentId: "my-agent",
  initialMemory: ["..."],

  // Memory paging & compression (SDK-side I/O)
  compressionStore: archiveStore,       // persist compressed transcript slices
  asyncSummarizer: mySummarizer,        // upgrade rule-based compression summaries
  memoryProvider: memoryLlm,             // LLM for durable-memory extraction
  memorySummarizer: myMemorySummarizer,   // LLM for semantic page_out → MemoryStore

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
| `governancePolicy` | Declarative deny / ask_user / rate-limit / param rules installed before canonical root start |
| `signalPolicy` | Versioned in-kernel signal queue/TTL policy (default queue 64) |
| `promptBudget` | Provider-envelope overhead, output reserve, and safety margin deducted from the context window |
| `resourceQuota` | M2 declarative limits — `maxConcurrentSubagents` / `maxTotalSubagents` / `maxSpawnDepth` / `maxWorkflowNodes` / `memoryWritesPerWindow` — enforced at the kernel syscall trap (`set_resource_quota`); over-quota spawns roll back, over-rate writes surface as `memory_validation_failed` |
| `memoryPolicy` | Canonical long-term memory policy: `validationEnabled: false` admits writes without validation, `maxContentBytes` / `maxNameLength` override validation limits, `retrievalTopK` caps `query_memory` breadth, and `staleWarningDays` controls stale recall policy. Storage belongs to the configured `memoryStore`. |
| `onPermissionRequest` | Resolves `tool_gated` + `suspended` → kernel `resume` with approved/denied call IDs |
| `compressionStore` | Writes archived messages on `compressed` observations |
| `payloadStore` | Resolves canonical opaque payload locators (default: `.payloads/`) |
| `asyncSummarizer` | Background LLM summary after compression; stored as `summary_upgraded` |
| `memorySummarizer` | Summarizes `page_out { tier_hint: "semantic" }` into `MemoryStore` during a run |
| `memoryProvider` | Separate LLM for durable-memory extraction (falls back to `provider`) |

Rebuild an OS diagnostics snapshot from session events:

```typescript
import { rebuildOsSnapshotFromSessionEvents } from "@deepstrike/sdk/os"

const events = (await sessionLog.read(sessionId)).map(e => e.event)
const snap = rebuildOsSnapshotFromSessionEvents(events)
// snap.pageOutCount, snap.signals, snap.processByAgent, …
```

---

## External tool payloads

When a tool result exceeds the configured inline threshold, the SDK persists the full body before sending the canonical `External` result. The kernel receives only `payload_ref`, `digest`, `original_size`, and a bounded preview.

The locator is opaque and never passed to ordinary file tools. The model calls `read_result`; core authorizes the reachable handle and emits `LoadPayload`, which the runner resolves through `PayloadStore`.

No configuration is required. Pass a `PayloadStore` through `RuntimeOptions.payloadStore` to use a different filesystem root or storage adapter.

---

## Tools

```typescript
import { tool } from "@deepstrike/sdk"
import { readFile } from "@deepstrike/sdk/workflow"

plane.register(tool("search", "Search.", schema, async (args) => ...))
plane.register(readFile)     // built-in: read files explicitly named by the caller
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
import { WorkingMemory } from "@deepstrike/sdk/memory"
const mem = new WorkingMemory()
mem.set("step", 1)
mem.get("step")  // 1
mem.clear()
```

### MemoryStore (long-term memory)

```typescript
import type { MemoryStore } from "@deepstrike/sdk/memory"

class MyStore implements MemoryStore {
  async put(agentId, record) { ... }         // the durable write path
  async get(agentId, recordId) { ... }       // Promise<MemoryRecord | null>
  async delete(agentId, recordId) { ... }    // idempotent deletion
  async search(agentId, query) { ... }       // Promise<MemoryRecall[]>
  async saveSession(session) { ... }         // completed transcript for extraction
}

const memoryScope = { tenant_id: "acme", namespace: "assistant" }
const runner = new RuntimeRunner({
  provider,
  executionPlane: plane,
  sessionLog: new FileSessionLog(".deepstrike/sessions"),
  maxTokens: 4096,
  memoryStore: new MyStore(),
  agentId: "my-agent",
  memoryScope,          // scopes run-start recall, queries, extraction, and semantic page-out
})
```

Four memory paths:

| Path | When | What happens |
|------|------|--------------|
| In-session `memory(query)` | LLM calls meta-tool | `MemoryStore.search()` → history tool result |
| `initialMemory` | Run start | Injected into Slot 2 (`systemKnowledge`) |
| `writeMemory(record)` | Host writes a durable record | Kernel validation / quota / dedup → `MemoryStore.put()` |
| Semantic `page_out` | Kernel evicts with `tier_hint: "semantic"` | SDK summarizes via `memorySummarizer` / `memoryProvider` → gated `writeMemory()` |

### Phase-7 memory syscalls (`writeMemory` / `queryMemory`)

Kernel-validated long-term memory I/O outside the main tool loop:

```typescript
await runner.writeMemory({
  record_id: crypto.randomUUID(),
  scope: { tenant_id: "acme", namespace: "assistant" },
  name: "prefers-small-tests",
  kind: "feedback",
  content: "User prefers focused unit tests for SDK behavior.",
  description: "Testing preference",
  provenance: { author: "host", trust: "user_asserted", evidence_refs: [] },
  created_at: Date.now(),
  updated_at: Date.now(),
  recall_count: 0,
  confidence: 1,
  links: [],
  pinned: false,
}, { sessionId: "my-session" })

const hits = await runner.queryMemory({
  scope: { tenant_id: "acme", namespace: "assistant" },
  query: "Need testing preferences",
  top_k: 5,
  kinds: ["feedback"],
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
import { SignalGateway, ScheduledPrompt } from "@deepstrike/sdk/os"

const gw = new SignalGateway()
gw.schedule(new ScheduledPrompt("standup", Date.now() + 3600_000))
gw.ingest({ kind: "alert", urgency: "normal", payload: { goal: "Check deploy" } })

const runner = new RuntimeRunner({
  provider,
  executionPlane: plane,
  sessionLog,
  signalSource: gw,
  signalPolicy: { queueMax: 64, ttlMs: 60_000 },
})

runner.interrupt() // cooperative abort → kernel timeout path
gw.destroy()
```

Each routed signal produces a correlated `signal_delivery_disposed` session event (`category: "ipc"`).

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

Requires an active parent run (`run()` / `wake()` in progress). The kernel emits `agent_process_changed`; the default `SubAgentOrchestrator` runs the child with a filtered execution plane and feeds `sub_agent_completed` back. If governance rejects the spawn before execution, the iterator yields one `error` event containing the denial reason; the parent turn is not rolled back.

---

## Harness (evaluation framework)

```typescript
import {
  AttemptLoop,
  RuntimeAttemptBody,
  LlmEvalJudge,
  freshWithFeedback,
} from "@deepstrike/sdk/harness"

const loop = new AttemptLoop({
  body: new RuntimeAttemptBody(runner),
  judge: new LlmEvalJudge(evalProvider, false),
  stop: { maxAttempts: 3 },
  // carry defaults to continueSession: stable session + journaled feedback signal.
  // Use `carry: freshWithFeedback` only when attempts must be isolated.
})
const outcome = await loop.run({
  sessionId: "task-42",
  goal: "Say hello",
  criteria: [{ text: "result greets the user", required: true }],
})

const runnerWithHarness = new RuntimeRunner({
  provider,
  executionPlane: plane,
  sessionLog,
  subAgentHarness: { evalProvider, maxAttempts: 3 },
})
```

`AttemptOutcome` keeps run health (`runStatus`) separate from evaluation (`verdict`) and reports a
terminal `outcome`: `passed`, `failed_judge`, `exhausted`, or `run_error`.

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
