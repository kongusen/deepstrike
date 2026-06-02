# Architecture

DeepStrike separates computation from I/O at the language boundary. A pure-Rust kernel handles all stateful logic; language SDKs handle LLM calls, tool execution, file I/O, and storage.

---

## Layer overview

```text
┌──────────────────────────────────────────────────────────────────┐
│  Layer 4: Application                                            │
│  goals · custom tools · knowledge sources · UI / API surface     │
└──────────────────────────────┬───────────────────────────────────┘
                               │
┌──────────────────────────────▼───────────────────────────────────┐
│  Layer 3: Collaboration Modes                                     │
│                                                                   │
│  CreatorVerifierMode — executor + verifier, drift metrics        │
│  OrchestrationMode   — orchestrator → contract → execute → verify│
└──────────────────────────────┬───────────────────────────────────┘
                               │
┌──────────────────────────────▼───────────────────────────────────┐
│  Layer 2: Collaboration Primitives                                │
│                                                                   │
│  VerificationContract — what correct looks like, system partition│
│  AgentPool            — role-isolated agent instances            │
│  ContractDrivenHarness— executor/verifier isolation protocol     │
│  HandoffBus           — unified artifact between all transitions │
│  TaskLane             — parallelism hints on RuntimeTask         │
└──────────────────────────────┬───────────────────────────────────┘
                               │
┌──────────────────────────────▼───────────────────────────────────┐
│  Layer 1: SDK Layer  (I/O)                                        │
│                                                                   │
│  Node.js  @deepstrike/sdk   │  Python  deepstrike                │
│  Rust     deepstrike-sdk    │  WASM    @deepstrike/wasm           │
│                                                                   │
│  • LLM streaming  (provider.stream)                               │
│  • Tool execution (executeTools / execute_tools)                  │
│  • Skill file I/O                                                 │
│  • Memory storage (DreamStore)                                    │
│  • Knowledge retrieval (KnowledgeSource)                          │
│  • Signal ingestion (timers, webhooks, user events)               │
└──────────────────────────────┬───────────────────────────────────┘
                               │  FFI / WASM bridge
┌──────────────────────────────▼───────────────────────────────────┐
│  Layer 0: deepstrike-core  (pure Rust, zero I/O)                  │
│                                                                   │
│  P1 Syscall  — governance gate, tool/spawn/memory validation    │
│  P2 Sched    — TCB lifecycle, budgets, suspend/resume             │
│  P3 MM       — compression funnel, spool, page-out/in, memory     │
│  Proc        — sub-agent process table                            │
│  IPC         — in-kernel SignalRouter (default)                   │
│  ContextEngine · EvalPipeline · IdlePipeline · collaboration types│
└──────────────────────────────────────────────────────────────────┘
```

---

## Kernel (`deepstrike-core`)

The kernel is pure Rust with **zero async I/O**. It exposes a synchronous state machine: the SDK feeds typed events; the kernel returns typed actions.

### State machine interface

```text
sm.start(task)             → Action::CallLLM   { messages, tools }
sm.feed_llm_response(msg)  → Action::ExecTools { calls }
sm.feed_tool_results(res)  → Action::CallLLM   { ... }     ← next turn
                           → Action::Done      { result }
```

The kernel never touches the network, filesystem, or clock. All time-dependent behaviour (timeouts, scheduled signals) is driven by the SDK calling kernel APIs with wall-clock timestamps.

### Subsystems

| Subsystem | Responsibility |
| --- | --- |
| **Syscall / governance** | Declarative policy loaded via `load_governance_policy`; deny / ask_user / rate-limit / param rules before tool execution |
| **Scheduler / TCB** | Turn lifecycle (Ready / Running / Blocked / Suspended); `max_turns`, token and wall-clock budgets; suspend for approval or sub-agent await |
| **MM** | Four-tier history compression funnel; Layer-1 large-result spool decisions; `page_out` / `page_in_requested`; memory write validation (`WriteMemory` / `QueryMemory`) |
| **Proc** | Process table for spawned sub-agents; `agent_process_changed` observations |
| **IPC / signals** | In-kernel `SignalRouter` (default): dedup, attention queue, disposition → `signal_disposed` |
| `ContextEngine` | Four-slot context window; compresses **history only** under pressure |
| `EvalPipeline` | LLM-as-judge scoring; extracts `SkillCandidate` objects from passing runs |
| `IdlePipeline` | Post-session: analyses `SessionData`, synthesises insights, commits to `DreamStore` |
| **Event log** | Observations tagged `syscall` · `sched` · `mm` · `proc` · `ipc` for session log and OS snapshot rebuild |

See [Agent OS](../concepts/agent-os.md) for capability-oriented overview.

### Context slots (four-slot model)

```text
┌─────────────────────────────────────────────────────────────────────┐
│ Slot 1 — system_stable     Identity: rules, role, constraints        │  never changes
│ Slot 2 — system_knowledge  Knowledge: memory blocks, skill defs      │  low frequency
│ Slot 3 — turns[0]          State: task_state + ephemeral signals     │  every turn
│ Slot 4 — turns[1..N]       History: conversation transcript          │  compressed
└─────────────────────────────────────────────────────────────────────┘
```

On Anthropic, Slots 1–2 map to separate `system[]` blocks with `cache_control`. Slot 3 is rebuilt each call from `task_state.format_compact()` plus runtime `signals`. Slot 4 is the sole target of the four-tier compression pipeline (Snip → Micro → Collapse → Auto).

When total token usage approaches `max_tokens`, the kernel runs compression tiers on history only. Summaries append to `task_state.compression_log` and render back into Slot 3 so the model always sees recent compression history.

See [Context Slots & Compression](../concepts/context-slots-compression.md) for tier thresholds, renewal carryover, and renderer behavior.

---

## SDK layer

Each SDK wraps the kernel over a language-native FFI bridge and adds all I/O.

### Binding architecture

| SDK | Binding crate | Mechanism |
| --- | --- | --- |
| Node.js | `crates/deepstrike-node` | napi-rs (native `.node` addon) |
| Python | `crates/deepstrike-py` | PyO3 (`.so` / `.pyd`) |
| WASM | `crates/deepstrike-wasm` | wasm-bindgen + Tsify |
| Rust SDK | — | Links `deepstrike-core` directly |

### Runtime loop (detailed)

```text
RuntimeRunner.run({ sessionId, goal })  /  run_streaming (Rust)
│
├─ Startup (native profile defaults)
│   ├─ load_governance_policy(governancePolicy)     → in-kernel syscall gate
│   ├─ set_attention_policy(attentionPolicy)        → in-kernel signal router
│   ├─ scan skills/*.md          → set_available_skills([...])
│   └─ set_memory_enabled / set_knowledge_enabled
│
├─ Loop (each turn)
│   │
│   ├─ Signal ingestion
│   │   └─ signal_source → kernel feed(signal)
│   │       → signal_disposed { disposition, queue_depth }
│   │
│   ├─ Action::CallLLM
│   │   └─ provider.stream(messages, tools) → yield stream events
│   │
│   └─ Action::ExecTools
│       ├─ page_in_requested?  → DreamStore / KnowledgeSource → page_in
│       ├─ governance (in-kernel) → allow | deny | ask_user (suspend/resume)
│       ├─ meta tool "skill"      → read skills/<name>.md
│       ├─ meta tool "memory"     → dream_store.search(query)
│       ├─ meta tool "knowledge"  → knowledge_source.retrieve(query)
│       ├─ user tools             → execute_tools(calls)
│       └─ large_result_spooled?  → write .spool/ ref; preview in context
│
├─ MM observations (any step)
│   ├─ compressed / page_out { tier_hint }  → semantic → dreamSummarizer → DreamStore
│   └─ appendObservations → SessionLog (category: mm | syscall | …)
│
├─ Side-channel memory syscalls (outside tool loop)
│   ├─ writeMemory  → WriteMemory validation → DreamStore.commit
│   └─ queryMemory  → QueryMemory → search → memory_retrieval_result
│
└─ After session
    └─ runner.dream(agent_id) → IdlePipeline → dream_store.commit(entries)
```

### Message content model

All four SDKs share the kernel's `Content` type, which is either plain text or an array of typed content parts:

```text
Content
├─ Text(string)
└─ Parts([ContentPart, ...])
       ├─ ContentPart::Text  { text }
       ├─ ContentPart::Image { url?, data?, media_type?, detail? }
       ├─ ContentPart::Audio { data, media_type }
       └─ ContentPart::ToolResult { tool_use_id, content, is_error }
```

Provider serialisation is automatic. The SDK converts `ContentPart` to the correct wire format before sending:

| Provider | Image format | Audio format |
| --- | --- | --- |
| Anthropic | `{type:"image", source:{type:"url"\|"base64", ...}}` | placeholder text |
| OpenAI-compat | `{type:"image_url", image_url:{url, detail?}}` / data-URI | `{type:"input_audio", input_audio:{data, format}}` |
| Ollama | `images: [base64string]` array | not supported |

---

## WASM considerations

The WASM SDK targets browsers and edge runtimes. It shares the same kernel but differs from the server SDKs in a few ways:

- Uses `fetch()` instead of native HTTP clients — no Node.js `http` module, no `reqwest`
- No `OllamaProvider` (localhost is not reachable from the browser sandbox)
- `Message.content` is string-only in the current WASM public API; multimodal via content parts is in progress
- All provider implementations are pure TypeScript with SSE parsing, not wrapping SDK libraries

---

## Repository layout

```text
deepstrike/
├─ crates/
│   ├─ deepstrike-core/      # Rust kernel (pure computation)
│   ├─ deepstrike-node/      # napi-rs bindings for Node.js
│   ├─ deepstrike-py/        # PyO3 bindings for Python
│   └─ deepstrike-wasm/      # wasm-bindgen bindings for browser
├─ node/                     # @deepstrike/sdk  (TypeScript)
├─ python/                   # deepstrike        (Python)
├─ rust/                     # deepstrike-sdk    (Rust)
├─ wasm/                     # @deepstrike/wasm  (TypeScript)
├─ benches/                  # criterion benchmarks
└─ tests/                    # integration tests
```
