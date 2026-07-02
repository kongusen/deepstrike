/**
 * Strict dynamic context control (mirrors node/tests/knowledge-skill-pin.test.ts +
 * pre-query-memory.test.ts): a loaded SKILL is method content reused for the rest of the run, so
 * it gets pinned into the durable `knowledge` slot on top of the ordinary tool_result already
 * headed for `history`. A `preQueryMemory` prefetch, by contrast, is single-use retrieval content
 * — it lands in `history` only and is never pinned into `knowledge`.
 *
 * The WASM jest suite runs against a hand-written mock kernel (tests/__mocks__/kernel.ts) that
 * doesn't actually render accumulated `knowledge`/`history` content (it returns a fixed `render()`
 * per event, mirroring only the handful of behaviors other WASM tests need). So instead of
 * asserting on rendered context — which the mock can't produce faithfully — these tests inspect
 * the mock's captured `kernelEvents` stream to prove the SDK emits the correct kernel event for
 * each content class, exactly as `smoke.test.ts`'s `configure_run` test does.
 */
import { RuntimeRunner, InMemorySessionLog, LocalExecutionPlane } from "../src/runtime/index.js"
import type { DreamStore, MemoryEntry } from "../src/memory/index.js"
import type { LLMProvider, Message, StreamEvent } from "../src/types.js"
import { kernelEvents } from "@deepstrike/wasm-kernel"

describe("skill content is pinned into durable knowledge on activation", () => {
  // The mock kernel's `provider_result` only dispatches ONE execute_tool round (phase 0→1, then
  // any further tool_calls are treated as final) — it can't drive a genuine repeat-activation
  // round trip. The dedupe guard (`knowledgePushedSkills`) itself is covered against a REAL kernel
  // by the Node/Python equivalents of this test; this one confirms the wiring fires at all.
  it("emits add_knowledge_message with the skill's resolved content on activation", async () => {
    kernelEvents.length = 0
    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "unused", toolCalls: [] }
      },
      async *stream(): AsyncIterable<StreamEvent> {
        yield { type: "tool_call", id: "s1", name: "skill", arguments: { name: "debug" } }
      },
    }

    const runner = new RuntimeRunner({
      provider,
      sessionLog: new InMemorySessionLog(),
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 2048,
      maxTurns: 6,
      skillContentMap: new Map([["debug", "---\nname: debug\n---\nDebug guidance."]]),
    })

    for await (const _e of runner.run({ sessionId: "knowledge-pin", goal: "debug it" })) { /* drain */ }

    const knowledgePushes = kernelEvents.filter((e: { kind?: string }) => e.kind === "add_knowledge_message")
    expect(knowledgePushes.length).toBe(1)
    expect((knowledgePushes[0] as { content?: string }).content).toContain("Debug guidance.")
    expect(kernelEvents.some((e: { kind?: string }) => e.kind === "skill_activated")).toBe(true)
  })
})

describe("preQueryMemory prefetch lands in history, not knowledge", () => {
  it("emits add_history_message, never add_knowledge_message or page_in", async () => {
    kernelEvents.length = 0
    const dreamStore: DreamStore = {
      loadSessions: async () => [],
      loadMemories: async () => [],
      commit: async () => {},
      saveSession: async () => {},
      search: async () => [{ text: "PREFETCHED_LONGTERM_FACT", score: 0.9, metadata: null } satisfies MemoryEntry],
    }

    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "unused", toolCalls: [] }
      },
      async *stream(): AsyncIterable<StreamEvent> {
        yield { type: "text_delta", delta: "done" }
      },
    }

    const runner = new RuntimeRunner({
      provider,
      sessionLog: new InMemorySessionLog(),
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 2048,
      agentId: "agent-prequery",
      dreamStore,
      preQueryMemory: () => ["past facts"],
    })

    for await (const _e of runner.run({ sessionId: "prequery", goal: "use the fact" })) { /* drain */ }

    const historyPushes = kernelEvents.filter((e: { kind?: string }) => e.kind === "add_history_message")
    expect(historyPushes.length).toBe(1)
    expect(JSON.stringify(historyPushes[0])).toContain("PREFETCHED_LONGTERM_FACT")
    expect(kernelEvents.some((e: { kind?: string }) => e.kind === "add_knowledge_message")).toBe(false)
    expect(kernelEvents.some((e: { kind?: string }) => e.kind === "page_in")).toBe(false)
  })
})
