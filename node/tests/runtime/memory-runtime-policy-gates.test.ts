import { RuntimeRunner, collectText } from "../../src/runtime/runner.js"
import { InMemorySessionLog } from "../../src/runtime/session-log.js"
import { LocalExecutionPlane } from "../../src/runtime/execution-plane.js"
import { tool } from "./helpers.js"
import type { LLMProvider, ModelMessage, RenderedContext, StreamEvent } from "../../src/types.js"
import type { MemoryRuntimePolicy } from "../../src/memory/runtime-policy.js"
import type { MemoryRecall, MemoryStore } from "../../src/memory/protocols.js"

const scope = { tenant_id: "agent-gates", namespace: "gates" }
const RECALL = "GATED_LONGTERM_FACT"

const recallHit: MemoryRecall = {
  record: {
    record_id: "record-gated", scope, name: "gated", kind: "reference", content: RECALL,
    description: "gate fixture",
    provenance: { author: "host", trust: "host_verified", evidence_refs: [] },
    created_at: 1, updated_at: 1, recall_count: 0, confidence: 0.9, links: [], pinned: false,
  },
  score: 0.9,
  why: "fixture",
}

/** Replies with plain text and no tool calls, so the run ends after one turn with new messages. */
function textProvider(sawContext?: (context: RenderedContext) => void): LLMProvider {
  return {
    async complete(): Promise<ModelMessage> {
      return { role: "assistant", content: "", toolCalls: [] }
    },
    async *stream(context: RenderedContext): AsyncIterable<StreamEvent> {
      sawContext?.(context)
      yield { type: "text_delta", delta: "done" }
    },
  }
}

function memoryStoreSpy(withHits = true) {
  const spy = { put: 0, search: 0, saveSession: 0 }
  const store: MemoryStore = {
    put: async () => { spy.put += 1 },
    get: async () => null,
    delete: async () => {},
    saveSession: async () => { spy.saveSession += 1 },
    search: async () => {
      spy.search += 1
      return withHits ? [recallHit] : []
    },
  }
  return { store, spy }
}

const recallOnly: MemoryRuntimePolicy = {
  retrieval: "host",
  modelWrites: "disabled",
}

describe("memory runtime policy gates (M1, end-to-end through the runner)", () => {
  it("refuses candidate_sink at runner construction when no candidate sink is supplied", () => {
    expect(() => new RuntimeRunner({
      provider: textProvider(),
      sessionLog: new InMemorySessionLog(),
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 1024,
      memoryRuntimePolicy: { ...recallOnly, modelWrites: "candidate_sink" },
    })).toThrow("needs RuntimeOptions.memoryCandidateSink")
  })

  it("model writes disabled: the completed session is still saved", async () => {
    const { store, spy } = memoryStoreSpy()
    const runner = new RuntimeRunner({
      provider: textProvider(),
      sessionLog: new InMemorySessionLog(),
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 1024,
      agentId: "agent-gates",
      memoryScope: scope,
      memoryStore: store,
      memoryRuntimePolicy: recallOnly,
    })
    await collectText(runner.run({ sessionId: "gates-writes-off", goal: "say done" }))
    expect(spy.saveSession).toBe(1)
    expect(spy.put).toBe(0)
  })

  it("retrieval disabled: the store is never searched and the recall never reaches the model", async () => {
    const { store, spy } = memoryStoreSpy()
    let sawRecall = false
    const runner = new RuntimeRunner({
      provider: textProvider(context => {
        if (JSON.stringify(context.turns).includes(RECALL)) sawRecall = true
      }),
      sessionLog: new InMemorySessionLog(),
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 1024,
      agentId: "agent-gates",
      memoryScope: scope,
      memoryStore: store,
      preQueryMemory: () => [{ scope, query: "past facts", top_k: 5, kinds: [] }],
      memoryRuntimePolicy: { ...recallOnly, retrieval: "disabled" },
    })
    await collectText(runner.run({ sessionId: "gates-retrieval-off", goal: "use the fact" }))
    expect(spy.search).toBe(0)
    expect(sawRecall).toBe(false)
  })

  it("retrieval enabled: the same prequery surfaces the recall", async () => {
    const { store, spy } = memoryStoreSpy()
    let sawRecall = false
    const runner = new RuntimeRunner({
      provider: textProvider(context => {
        if (JSON.stringify(context.turns).includes(RECALL)) sawRecall = true
      }),
      sessionLog: new InMemorySessionLog(),
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 1024,
      agentId: "agent-gates",
      memoryScope: scope,
      memoryStore: store,
      preQueryMemory: () => [{ scope, query: "past facts", top_k: 5, kinds: [] }],
      memoryRuntimePolicy: recallOnly,
    })
    await collectText(runner.run({ sessionId: "gates-retrieval-on", goal: "use the fact" }))
    expect(spy.search).toBeGreaterThan(0)
    expect(sawRecall).toBe(true)
  })

  it("page-out persists compression and payload data without creating a memory record", async () => {
    const { store, spy } = memoryStoreSpy(false)
    let callCount = 0
    const provider: LLMProvider = {
      async complete(): Promise<ModelMessage> {
        return { role: "assistant", content: "", toolCalls: [] }
      },
      async *stream(): AsyncIterable<StreamEvent> {
        callCount += 1
        if (callCount <= 8) {
          yield { type: "tool_call", id: `c${callCount}`, name: "fill", arguments: { n: callCount } }
          return
        }
        yield { type: "text_delta", delta: "done" }
      },
    }
    const sessionLog = new InMemorySessionLog()
    const plane = new LocalExecutionPlane()
    plane.register(tool("fill", "fill", { type: "object", properties: { n: { type: "number" } } }, () => "w".repeat(200)))
    const runner = new RuntimeRunner({
      provider,
      sessionLog,
      executionPlane: plane,
      baselineToolIds: ["fill"],
      maxTokens: 400,
      maxTurns: 20,
      agentId: "agent-gates",
      memoryScope: scope,
      memoryStore: store,
      memoryRuntimePolicy: { ...recallOnly, retrieval: "disabled" },
    })
    await collectText(runner.run({ sessionId: "gates-page-out", goal: "fill until compact" }))
    expect(spy.put).toBe(0)
    const events = await sessionLog.read("gates-page-out")
    expect(events.some(e => e.event.kind === "semantic_archive_pending")).toBe(false)
    expect(events.some(e => e.event.kind === "compressed")).toBe(true)
  })
})
