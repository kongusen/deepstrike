# Agent OS

**Status:** Current (0.2.6+)  
**Related:** [Architecture overview](../architecture/overview.md) · [Kernel ABI](../reference/kernel-abi.md) · [SDK OS parity](../sdk-os-parity.md)

---

DeepStrike 0.2.6 treats the runtime as an **Agent OS**: a pure-Rust microkernel mediates scheduling, memory, governance, signals, and processes; host SDKs (Node, Python, Rust, WASM) own all side effects — LLM calls, tools, disk, long-term storage, and network.

## Before and after

| Before (≤ 0.2.4) | After (0.2.6) |
| :--- | :--- |
| Scheduling, compression, and permission logic scattered per SDK | Unified syscall trap, TCB lifecycle, and MM eviction funnel — same semantics across host languages |
| Large tool outputs and long sessions hit token walls | Layer-1 spool (preview + `.spool/` ref) and semantic page-out → long-term memory |
| Governance and signal routing were optional SDK plugins | Native profile defaults: declarative governance + in-kernel signal routing on every run |
| Long-term memory mostly via meta-tools and idle pipelines | `writeMemory` / `queryMemory` kernel syscalls with validation and audit events |
| Session logs skewed toward chat + tools | Full OS event stream and rebuildable OS snapshots |

## Three kernel primitives (M0–M4)

The kernel is organized around three responsibilities:

| Primitive | Module | Kernel decides | SDK executes |
| :--- | :--- | :--- | :--- |
| **Syscall trap (P1)** | `governance/`, tool gate | Whether a tool call, spawn, or memory write is allowed | Run tools, spawn child runners, commit to `DreamStore` |
| **TCB / scheduler (P2)** | `scheduler/` | Run lifecycle: Ready → Running → Blocked → Suspended → Terminated; budgets; suspend/resume | Feed time, approval results, sub-agent completion |
| **MM (P3)** | `mm/` | When to compress, spool, page-out, page-in; memory validation | Write `.spool/` files, summarize for DreamStore, satisfy `page_in` |

**M4 — Kernel event log:** Every decision emits categorized observations (`syscall` · `sched` · `mm` · `proc` · `ipc`) into `SessionLog`. Replay reconstructs LLM messages by **stripping** OS audit events.

```text
Host SDK                         Kernel (pure Rust)
────────                         ────────────────
feed(provider_result)     ──►    plan next action
feed(tool_results)        ◄──    execute_tool / call_provider / done
drain observations        ◄──    compressed, page_out, spool, signal_disposed, …
writeMemory / queryMemory ──►    validate → observation → SDK I/O
```

## What you can build

### Kernel-mediated runtime

Tool calls and spawns pass through one gate before your execution plane runs. Compression and signal disposition are kernel decisions with stable semantics in Node, Python, and Rust. `wake(sessionId)` resumes through the same path as a fresh run.

### Longer, sturdier sessions

**Layer-1 spool** — Single tool results above the size threshold stay in context as a preview plus a spool reference. Full payloads live under `.spool/`; the model reads them on demand via ordinary file tools.

**Semantic page-out** — When pressure triggers `page_out { tier_hint: "semantic" }`, the SDK summarizes archived content into `DreamStore` and later satisfies `page_in_requested` before meta-tool execution.

See [Context Slots & Compression](./context-slots-compression.md#layer-1-large-result-spool) for tier thresholds and paging detail.

### Safety and governance by default

Every run loads declarative `governancePolicy` (default: allow-all native profile) and in-kernel `attentionPolicy` (default queue size 64). Rules enforce deny / ask_user / rate-limit / param constraints **before** tools execute. External signals receive disposition (Interrupt / Queue / Observe / Dropped) in-kernel.

See [Core Concepts — Safety](./core-concepts.md#safety--permission-boundaries) and [Signals](./core-concepts.md#signals--external-interrupts).

### Long-term memory as syscalls (Phase-7)

Outside the main tool loop:

| Syscall | Flow |
| :--- | :--- |
| `writeMemory` | Kernel `WriteMemory` validation → `DreamStore.commit()` or `memory_validation_failed` |
| `queryMemory` | Kernel `QueryMemory` → search → `selectMemories` → `memory_retrieval_result` |

Memory kinds (User / Feedback / Project / Reference) and forbidden-pattern rules live in the kernel; storage and LLM selection stay in the SDK.

See [Core Concepts — Memory syscalls](./core-concepts.md#phase-7-memory-syscalls).

### Multi-agent and multi-signal orchestration

Sub-agents register in the kernel process table (`agent_process_changed`); parent runs enter `Suspended` until `sub_agent_completed`. Signals from gateways, cron, or user events compose with the main loop instead of racing SDK-internal state.

See [Collaboration guide](../guides/collaboration.md).

### Observable like an OS log

Rebuild a read-only **OS snapshot** from session events: spool counts, page-out counts, signal timeline, process table, memory counters. Use categories for dashboards, compliance, and debugging without re-instantiating the kernel.

```typescript
// Node.js — illustrative
import { rebuildOsSnapshotFromSessionEvents } from "@deepstrike/sdk"

const snap = rebuildOsSnapshotFromSessionEvents(sessionEvents)
// snap.pageOutCount, snap.spoolCount, snap.processByAgent, …
```

Parity matrix: [SDK OS parity](../sdk-os-parity.md).

## Quick reference

| You need… | Mechanism |
| :--- | :--- |
| Policy before tools run | `governancePolicy` / `governance_policy` |
| External interrupts | `signalSource` + in-kernel `attentionPolicy` |
| Huge tool output | Automatic Layer-1 spool; optional custom `resultSpool` |
| Durable recall across runs | `DreamStore` + semantic page-out via `dreamSummarizer` |
| Programmatic memory I/O | `writeMemory()` / `queryMemory()` (Node); `write_memory()` / `query_memory()` (Python) |
| Debug / compliance | `SessionLog` + OS snapshot rebuild |

## SDK entry points

| SDK | Deep dive |
| :--- | :--- |
| Node.js / TypeScript | [Node SDK guide](../guides/sdk-nodejs.md) · [node/README.md](../../node/README.md) |
| Python | [Python SDK guide](../guides/sdk-python.md) · [python/README.md](../../python/README.md) |
| Rust | [Rust SDK guide](../guides/sdk-rust.md) |
| WASM | Event mapping only for memory syscalls; runner APIs in progress |

## Optional strict validation

`osProfile: "native"` / `os_profile: "native"` runs static fail-fast checks: required policies present, legacy governance instances forbidden. **Behavioral defaults in 0.2.6 already use the native profile** — explicit `osProfile` is for tests and strict deployments.
