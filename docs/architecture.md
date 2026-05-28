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
│  LoopStateMachine   — turn-by-turn control, termination policy    │
│  ContextEngine      — 4-slot context + tiered history compression     │
│  GovernancePipeline — Permission → Veto → RateLimit → Audit       │
│  SignalRouter       — priority queue, dedup, dispositions         │
│  EvalPipeline       — LLM-as-judge, skill candidate extraction    │
│  IdlePipeline       — post-session dreaming, memory curation      │
│  VerificationContract / TaskLane / HandoffArtifact (kernel types) │
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
| `LoopStateMachine` | Turn-by-turn control; enforces `max_turns`, `token_budget`, and `timeout` termination policy |
| `ContextEngine` | Manages a four-slot context window aligned with LLM APIs; compresses **history only** under pressure |
| `GovernancePipeline` | Evaluates every tool call: Permission → Veto → RateLimit → Constraint → Audit |
| `SignalRouter` | Priority dedup queue; maps incoming signals to dispositions (`interrupt_now`, `interrupt`, `queue`, `observe`, `dropped`) |
| `EvalPipeline` | LLM-as-judge scoring; extracts `SkillCandidate` objects from passing runs |
| `IdlePipeline` | Post-session: analyses `SessionData`, synthesises insights via LLM, deduplicates, commits to `DreamStore` |

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

See [Context Slots & Compression](./context-partition-compression.md) for tier thresholds, renewal carryover, and renderer behavior.

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
├─ Startup
│   ├─ scan  skills/*.md          → sm.set_available_skills([...])
│   ├─ sm.set_memory_enabled(true)
│   └─ sm.set_knowledge_enabled(true)
│
├─ Loop (each turn)
│   │
│   ├─ Signal ingestion
│   │   └─ signal_source.next_signal()  → router.ingest(signal)
│   │       → disposition: interrupt_now / interrupt / queue / observe / dropped
│   │
│   ├─ Action::CallLLM
│   │   └─ provider.stream(messages, tools)
│   │       ├─ TextDelta         → yield to caller
│   │       ├─ ThinkingDelta     → yield to caller
│   │       └─ ToolCallEvent     → buffer
│   │
│   └─ Action::ExecTools
│       ├─ governance.evaluate()
│       │     allow  → proceed
│       │     deny   → ToolResult { is_error: true, content: "denied" }
│       │     ask    → yield PermissionRequestEvent; pause until resolved
│       │
│       ├─ meta tool "skill"      → read skills/<name>.md
│       ├─ meta tool "memory"     → dream_store.search(query)
│       ├─ meta tool "knowledge"  → knowledge_source.retrieve(query)
│       └─ user tools             → execute_tools(calls)
│
└─ After session
    └─ runner.dream(agent_id)
        └─ idle_pipeline.run(session_data) → dream_store.commit(entries)
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
