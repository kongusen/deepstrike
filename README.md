# DeepStrike

**Agent OS microkernel for cross-language agent runtimes.**  
Version **0.2.0**

DeepStrike splits agent runtime into two layers with a hard boundary:

| Layer | Owns | Does not |
|-------|------|----------|
| **Kernel** (`deepstrike-core`) | State machine, context VM, capability bus, syscall governance, transactions, milestones, sub-agent isolation, audit semantics, host ABI | Direct I/O |
| **Host SDK** (Node / Python / Rust / WASM) | LLM providers, filesystem, processes, network, UI, human approval | Invent runtime behavior |

**Invariant:** Kernel owns agent semantics. SDK owns host effects.

The SDK feeds versioned `KernelInput` into `KernelRuntime.step()` and executes the `KernelAction`s the kernel returns. All loop, context, governance, and capability decisions live in the kernel — not in SDK glue code.

```
┌──────────────────────────────────────────────────────────────────────────┐
│  Host SDK (Node / Python / Rust / WASM)                                  │
│  Provider · ExecutionPlane · SessionLog · ArchiveStore · Orchestrator    │
└───────────────────────────────┬──────────────────────────────────────────┘
                                │  KernelInput / KernelAction  (JSON ABI v1)
                    ┌───────────▼───────────┐
                    │   deepstrike-core     │
                    │   KernelRuntime       │
                    │   Agent State Machine │
                    │   Context VM (6 sect) │
                    │   Capability Bus      │
                    │   Security LSM        │
                    │   Transaction Runtime │
                    │   Milestone Contracts │
                    │   Sub-Agent Isolation │
                    └───────────────────────┘
```

---

## Packages

| Package | Language | Role |
|---------|----------|------|
| `crates/deepstrike-core` | Rust | Agent OS kernel — pure computation, no I/O |
| `crates/deepstrike-node` | Rust/NAPI | Node.js FFI (`KernelRuntime.step`) |
| `crates/deepstrike-py` | Rust/PyO3 | Python FFI |
| `crates/deepstrike-wasm` | Rust/WASM | Browser / edge FFI |
| `node/` (`@deepstrike/sdk`) | TypeScript | Node host SDK |
| `python/` (`deepstrike`) | Python | Python host SDK |
| `rust/` (`deepstrike-sdk`) | Rust | Rust host SDK |

---

## Quick Start

**Node.js**

```bash
npm install @deepstrike/sdk@0.2.0
```

```typescript
import {
  AnthropicProvider,
  InMemorySessionLog,
  LocalExecutionPlane,
  RuntimeRunner,
  collectText,
  tool,
} from "@deepstrike/sdk"

const add = tool("add", "Add two numbers.", schema, async ({ x, y }) => String(x + y))
const plane = new LocalExecutionPlane().register(add)
const runner = new RuntimeRunner({
  provider: new AnthropicProvider("sk-..."),
  executionPlane: plane,
  sessionLog: new InMemorySessionLog(),
  maxTokens: 32_000,
})

await collectText(runner.run({ sessionId: "demo", goal: "What is 2 + 3?" }))
```

**Python**

```bash
pip install deepstrike==0.2.0
```

```python
from deepstrike import (
    AnthropicProvider,
    InMemorySessionLog,
    LocalExecutionPlane,
    RuntimeOptions,
    RuntimeRunner,
    collect_text,
    tool,
)

@tool
def add(x: int, y: int) -> int:
    """Add two numbers."""
    return x + y

plane = LocalExecutionPlane().register(add)
runner = RuntimeRunner(RuntimeOptions(
    provider=AnthropicProvider(api_key="..."),
    session_log=InMemorySessionLog(),
    execution_plane=plane,
    max_tokens=32_000,
))

await collect_text(runner.run(goal="What is 2 + 3?"))
```

**Rust**

```toml
[dependencies]
deepstrike-sdk = "0.2.2"
```

```rust
use std::sync::Arc;
use deepstrike_sdk::{
    AnthropicProvider, InMemorySessionLog, LocalExecutionPlane,
    RegisteredTool, RuntimeOptions, RuntimeRunner,
};

let mut plane = LocalExecutionPlane::new();
plane.register(/* RegisteredTool::text(...) */);
let runner = RuntimeRunner::new(RuntimeOptions {
    provider: Box::new(AnthropicProvider::new("sk-...")),
    execution_plane: Some(Box::new(plane)),
    session_log: Some(Arc::new(InMemorySessionLog::new())),
    max_tokens: 32_000,
    ..Default::default()
});
let answer = runner.execute("What is 2 + 3?").await?;
```

Public host surface: `RuntimeRunner` + `SessionLog` + `ExecutionPlane`. Internally, every turn is driven by `KernelRuntime.step()` — see [Kernel ABI](docs/spec-kernel-abi.md).

---

## Kernel (v0.2.0)

v0.2.0 replaces the v1 pattern where SDKs owned the loop and stitched context by hand. The kernel is now the **single control plane**; SDKs are **host I/O drivers**.

### Control flow

```
SDK                          KernelRuntime.step()
 │                                    │
 ├─ KernelInput (start_run,           ├─ KernelAction (call_provider,
 │   provider_result, tool_results,    │   execute_tool, evaluate_milestone,
 │   milestone_result, signal, ...)    │   done)
 │                                    ├─ KernelObservation (compressed,
 └─ execute actions, feed results       │   rollbacked, capability_changed,
    back as next KernelInput            │   milestone_*, agent_spawned, ...)
```

ABI version `1` is frozen as JSON across Node, Python, and WASM FFI. Canonical schema snapshots live in `tests/fixtures/abi/`.

### Subsystems

| Subsystem | Role |
|-----------|------|
| **Kernel ABI** | `KernelInput` / `KernelAction` / `KernelObservation` / `KernelRuntime::step()` — the only public kernel boundary |
| **Context VM** | Six partitions (`system`, `skill`, `memory`, `working`, `history`, `artifacts`) with per-section cache, pin, and compaction policy |
| **Capability Bus** | Runtime capability graph (tools, skills, MCP, sub-agents, …) with mount/unmount/replace/pin and provenance audit |
| **Security LSM** | Eight-stage `ToolDecisionPipeline` — classify → capability → constraint → permission → veto → rate limit → sandbox → audit; deny is monotonic |
| **Transaction Runtime** | Turn checkpoints, fatal-only rollback, `ToolErrorKind`, replay truncation at rollback events |
| **Milestone Contracts** | Verifier-driven phase gates (`machine`, `harness`, `llm_judge`, `human`, `external_ci`); unlock capabilities with provenance |
| **Sub-Agent Isolation** | `AgentRunSpec` + isolation manifest; capability filter enforced by host; parent-child audit lineage |
| **SignalRouter** | Priority queue with dedup; external signals routed to dispositions |
| **EvalPipeline** | LLM-as-judge; extracts reusable skill candidates |
| **IdlePipeline** | Post-session memory consolidation ("dreaming") |

### Context partitions

| Partition | Policy | Invalidation |
|-----------|--------|--------------|
| System | Immutable / static cache | Never |
| Skill | Session cached | On skill change |
| Memory | Dynamic retrieved, bounded | On memory refresh |
| Working | Volatile signal buffer | Every turn |
| History | Compressible / archival | On compact |
| Artifacts | Referenced, not inlined | — |

Large outputs go through `push_artifact`; the kernel keeps references, not full inline blobs.

### Milestones

Engineering agents advance through explicit phases, not implicit chat flow:

```
phase_id → criteria → verifier → required_evidence → unlock_capabilities
         → rollback_policy → retry_policy
```

Default policy requires a verifier (`EvaluateMilestone` → host runs verifier → `milestone_result`). Auto-pass is opt-in only.

When a run stops at a milestone (`status: "milestone_pending"`), resume it after external verification:

```typescript
// Run stops at milestone gate
for await (const evt of runner.run({ sessionId, goal })) {
  if (evt.type === "done" && evt.status === "milestone_pending") {
    // ... run external verifier, approve ...
    break
  }
}

// Resume the same session — kernel replays state, continues from gate
for await (const evt of runner.wake(sessionId)) { /* ... */ }
```

### Sub-agents

Multi-agent behavior is a **kernel contract**, not a prompt suggestion:

1. Host sends `spawn_sub_agent` with `AgentRunSpec` and `parent_session_id`.
2. Kernel emits `AgentSpawned` (role, isolation, context inheritance, permitted capabilities).
3. Host runs the child through `FilteredExecutionPlane` / `SubAgentOrchestrator`.
4. Host feeds `sub_agent_completed` back to the parent.

```typescript
// Active parent run — streams child events back to caller
for await (const evt of runner.spawnSubAgent(spec)) { /* handle StreamEvent */ }
// or collect final text only
const text = await collectText(runner.spawnSubAgent(spec))

// Standalone (harness / coordinator, no active parent loop)
import { spawnStandalone } from "@deepstrike/sdk"
const result = await spawnStandalone(parentOpts, parentSessionId, spec)
```

---

## Host SDK Layer

Each SDK wraps the kernel and performs all I/O:

```
RuntimeRunner.run({ sessionId, goal })   ← start or replay a session
RuntimeRunner.wake(sessionId)            ← resume after milestone_pending
│
├─ Startup (via KernelInput)
│   ├─ scan skill/*.md → set_available_skills
│   ├─ set_memory_enabled / set_knowledge_enabled
│   └─ capability_command / load_milestone_contract
│
├─ Each turn (KernelRuntime.step loop)
│   ├─ call_provider  → provider.stream()
│   ├─ execute_tool   → Governance.evaluate() → ExecutionPlane
│   └─ evaluate_milestone → host verifier → milestone_result
│
└─ Observations → SessionLog (audit / replay)
```

**Skills** — `.md` files with YAML frontmatter; kernel injects a `skill` meta-tool; model loads instructions on demand.

**Memory** — in-session `memory(query)` via `DreamStore`; post-session `runner.dream(agentId)` runs `IdlePipeline`.

**Knowledge** — read-only `knowledge(query)` through `KnowledgeSource` (RAG, APIs, docs).

**Harness** — `HarnessLoop` / `ContractDrivenHarness` wrap sessions with eval gates; successful runs can materialize skill candidates. All harness types expose both `run()` (collect outcome) and `stream()` (forward `StreamEvent`s).

**Signals** — `SignalGateway` ingests webhooks, cron, interrupts; kernel assigns dispositions (`interrupt_now`, `interrupt`, `queue`, `observe`, `dropped`).

**Safety** — kernel LSM evaluates every tool call; SDK `Governance` configures rules; `PermissionRequestEvent` surfaces ask-user flows.

---

## Providers

| Provider | Backend | Thinking / Reasoning |
|----------|---------|----------------------|
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
| `tool_delta` | `callId`, `name`, `delta?`, `chunk?` |
| `tool_suspend` | `callId`, `name`, `suspensionId`, `payload?` |
| `tool_result` | `callId`, `name`, `content`, `isError` |
| `permission_request` | `callId`, `toolName`, `arguments`, `reason` |
| `done` | `iterations`, `totalTokens`, `status` |
| `error` | `message` |

`status`: `completed` / `max_turns` / `token_budget` / `timeout` / `user_abort` / `milestone_pending` / `error`

Session log also records kernel audit events: `compressed`, `rollbacked`, `checkpoint_taken`, `capability_changed`, `milestone_*`, `agent_spawned`, `tool_denied`, etc.

---

## Documentation

| Document | Contents |
|----------|----------|
| [implementation-agent-os-kernel.md](docs/implementation-agent-os-kernel.md) | Kernel roadmap, phase gates, architecture |
| [spec-kernel-abi.md](docs/spec-kernel-abi.md) | `KernelInput` / `KernelAction` / `KernelObservation` contract |
| [spec-context-compression-v2.md](docs/spec-context-compression-v2.md) | Context VM, compaction, archive store |
| [sdk-kernel-driver-parity.md](docs/sdk-kernel-driver-parity.md) | Cross-SDK plan for aligning Node, Python, Rust, and WASM around the kernel-driver contract |
| [sdk-guide-nodejs.md](docs/sdk-guide-nodejs.md) | Node SDK guide |
| [sdk-guide-python.md](docs/sdk-guide-python.md) | Python SDK guide |

---

## Build

```bash
# Rust kernel + all bindings
cargo build

# Node.js SDK
cd node && npm install && npm run build

# Python SDK (requires maturin)
cd python && python3 -m venv .venv && source .venv/bin/activate
pip install -e ".[dev]" 2>/dev/null || pip install maturin pytest pytest-asyncio && maturin develop --release

# Run tests
cargo test
cd node && npm test
cd python && pytest
```

Requires Rust 1.85+, Node.js 18+, Python 3.10+.

---

## License

Apache-2.0 OR MIT
