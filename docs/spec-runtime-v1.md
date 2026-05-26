# Runtime v1 — Session Event Log

Runtime v1 adds a thin layer between the SDK and `deepstrike-core` with three abstractions:

| Abstraction | Responsibility |
| --- | --- |
| `SessionLog` | Append-only event source of truth |
| `RuntimeRunner` | Stateless brain: `run()` / `wake()` |
| `ExecutionPlane` | Tool execution surface (`execute` → `ToolResult`) |

The kernel is unchanged except for `LoopStateMachine.resumeAfterPreload()` used during mid-run recovery.

---

## Session events (frozen v1)

Events are tagged with `kind` (snake_case). Only append new variants in future versions; do not rename or remove fields.

```typescript
interface ProviderReplay {
  native_blocks?: Array<Record<string, unknown>>  // Anthropic thinking/tool blocks
  reasoning_content?: string                       // DeepSeek / OpenAI-compatible reasoning
}

type SessionEvent =
  | { kind: "run_started"; run_id: string; goal: string; criteria: string[]; agent_id?: string; system_prompt?: string }
  | { kind: "llm_completed"; turn: number; content: string; token_count?: number; tool_calls: ToolCall[]; provider_replay?: ProviderReplay }
  | { kind: "tool_requested"; turn: number; calls: ToolCall[] }
  | { kind: "tool_completed"; turn: number; results: Array<{ call_id: string; output: string; is_error?: boolean; token_count?: number }> }
  | { kind: "compressed"; turn: number; archived_seq_range: [number, number] }
  | { kind: "run_terminal"; reason: string; turns_used: number; total_tokens: number }
```

**Recovery minimum set:** `run_started`, `llm_completed`, `tool_completed`, `run_terminal`.

Telemetry-only signals (permissions, signals, etc.) are not stored in the event log in v1.

---

## SessionLog interface

```typescript
interface SessionLog {
  append(sessionId: string, event: SessionEvent): Promise<number>  // returns seq (0-based)
  read(sessionId: string, fromSeq?: number): Promise<Array<{ seq: number; event: SessionEvent }>>
  latestSeq(sessionId: string): Promise<number>  // -1 if empty
}
```

Implementations:

- `InMemorySessionLog` — tests and ephemeral runs
- `FileSessionLog` — one JSONL file per session (`{dir}/{sessionId}.jsonl`)

**Concurrency:** v1 assumes a single writer per `sessionId`. `FileSessionLog` is not safe for concurrent writers on the same session.

---

## RuntimeRunner

```typescript
class RuntimeRunner {
  run(req: { sessionId: string; goal: string; criteria?: string[] }): AsyncIterable<StreamEvent>
  wake(sessionId: string): AsyncIterable<StreamEvent>
}
```

### `run()`

1. Read prior events for `sessionId`.
2. If the session is **mid-run** (events exist, no `run_terminal`), delegate to wake semantics (no new `run_started`).
3. Otherwise append `run_started`, preload prior transcript (if any completed runs), then `start()`.

### `wake()`

1. Read events; return immediately if `run_terminal` is present.
2. `preloadHistory(replay(events))` then `resumeAfterPreload()` — no duplicate user turn.
3. Continue the loop; append events each step; finish with `run_terminal`.

### Replay projection

`replay()` maps events to kernel `Message[]`:

- `run_started` → `user`
- `llm_completed` → `assistant` (always include `tool_calls: []` when empty)
- `llm_completed.provider_replay` → restored into provider native replay cache on preload/wake (thinking blocks, `reasoning_content`, etc.)
- `tool_completed` → `tool` messages with `contentParts`

Before preload/wake, SDKs run **`repairEventsForRecovery`** on the event log:

| Gap | Repair |
|-----|--------|
| Missing `tool_calls` on `llm_completed` | Default to `[]` |
| Missing `token_count` | Estimate from content length |
| Missing `provider_replay` on tool turns | Synthesize minimal `native_blocks` (text + tool_use) |
| Invalid / oversized content | UTF-8-safe sanitize (see below) |

On **write**, SDKs use `buildLlmCompletedEvent` / `buildRunTerminalEvent` so appended events already satisfy the recovery minimum set.

**Wake resume:** After `preloadHistory`, `resumeAfterPreload()` checks whether the last assistant turn has tool calls without matching tool results. If so, it returns `ExecuteTools` (runs pending tools) instead of calling the LLM again.

Context compression still happens inside the kernel; `compressed` events record that compression occurred and the archived session event seq range (summary body is not duplicated in the log).

**UTF-8 safety:** All text truncation in render/compress paths must cut on `char` boundaries, not raw byte indices (see `docs/issues/utf8-truncation-renderer.md`). SDK replay may apply `sanitize_replay_text` on `llm_completed.content` before preload as defense-in-depth.

---

## ExecutionPlane

```typescript
interface ExecutionPlane {
  register(...tools: RegisteredTool[]): this
  schemas(): ToolSchema[]
  executeAll(calls: ToolCall[], ctx: RunContext): AsyncIterable<StreamEvent>
}
```

`LocalExecutionPlane` is the default: same-process tools, governance, meta-tools (`skill` / `memory` / `knowledge`), concurrent regular tools.

Future planes (`ProcessSandboxPlane`, `McpProxyPlane`, `RemoteVpcPlane`) implement the same interface.

---

## Dream / memory

`RuntimeRunner.dream()` continues to consume `DreamStore` session snapshots (`SessionData.messages`), not the raw event log. Snapshots are written at the end of each successful run.

**Note on naming:** Runtime v1 uses `SessionLog` (append-only event log). This is distinct from the kernel `SessionStore` trait in `deepstrike-core` (`memory/durable.rs`), which backs durable memory for the kernel memory subsystem. All SDKs (Node, Python, Rust, WASM) use `RuntimeRunner` + `SessionLog` + `ExecutionPlane` as the public entry point.

---

## Node.js entry points

```typescript
import {
  RuntimeRunner,
  LocalExecutionPlane,
  InMemorySessionLog,
  FileSessionLog,
  collectText,
} from "@deepstrike/sdk"
```
