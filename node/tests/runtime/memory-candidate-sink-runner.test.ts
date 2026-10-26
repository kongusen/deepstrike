import { RuntimeRunner, collectText } from "../../src/runtime/runner.js"
import { InMemorySessionLog, type SessionEvent } from "../../src/runtime/session-log.js"
import { LocalExecutionPlane } from "../../src/runtime/execution-plane.js"
import { kernelObservationToSessionEvent } from "../../src/runtime/kernel-event-log.js"
import {
  InMemoryMemoryCandidateSink,
  type MemoryCandidate,
  type MemoryCandidateReceipt,
  type MemoryCandidateSink,
} from "../../src/memory/candidate-sink.js"
import { tool } from "./helpers.js"
import type { LLMProvider, MemorySummarizer, ModelMessage, StreamEvent } from "../../src/types.js"
import type { MemoryRuntimePolicy } from "../../src/memory/runtime-policy.js"
import type { MemoryStore } from "../../src/memory/protocols.js"

/** M2 · the candidate sink end to end through the runner: S03 (a candidate is not remembered) on
 *  the two host write paths that run today, and S04 (a lost acknowledgement) on the PersistMemory
 *  effect path. */

const scope = { tenant_id: "agent-candidates", namespace: "candidates" }

const writesOff: MemoryRuntimePolicy = {
  retrieval: "disabled",
  sessionExtraction: "disabled",
  modelWrites: "disabled",
  semanticArchive: "disabled",
}

function textProvider(): LLMProvider {
  return {
    async complete(): Promise<ModelMessage> {
      return { role: "assistant", content: "", toolCalls: [] }
    },
    async *stream(): AsyncIterable<StreamEvent> {
      yield { type: "text_delta", delta: "done" }
    },
  }
}

/** The auxiliary provider session extraction calls: proposes two memories. */
function extractingProvider(): LLMProvider {
  const memories = {
    memories: [
      { name: "brief-style", kind: "project", content: "prefers numbered sections", description: "brief formatting" },
      { name: "brief-length", kind: "project", content: "keeps briefs under a page", description: "brief length" },
    ],
  }
  return {
    async complete(): Promise<ModelMessage> {
      return { role: "assistant", content: "", toolCalls: [] }
    },
    async *stream(): AsyncIterable<StreamEvent> {
      yield { type: "text_delta", delta: JSON.stringify(memories) }
    },
  }
}

function storeSpy() {
  const spy = { put: 0 }
  const store: MemoryStore = {
    put: async () => { spy.put += 1 },
    get: async () => null,
    delete: async () => {},
    saveSession: async () => {},
    search: async () => [],
  }
  return { store, spy }
}

async function kinds(sessionLog: InMemorySessionLog, sessionId: string): Promise<SessionEvent[]> {
  return (await sessionLog.read(sessionId)).map(entry => entry.event)
}

describe("memory candidate sink through the runner (M2)", () => {
  it("refuses a candidate_sink path without a sink at construction", () => {
    expect(() => new RuntimeRunner({
      provider: textProvider(),
      sessionLog: new InMemorySessionLog(),
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 1024,
      memoryRuntimePolicy: { ...writesOff, sessionExtraction: "candidate_sink" },
    })).toThrow('sessionExtraction = "candidate_sink" needs RuntimeOptions.memoryCandidateSink')
  })

  it("S03 · session extraction hands its writes to the sink: pending, never written to the store", async () => {
    const sink = new InMemoryMemoryCandidateSink()
    const { store, spy } = storeSpy()
    const sessionLog = new InMemorySessionLog()
    const runner = new RuntimeRunner({
      provider: textProvider(),
      sessionLog,
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 1024,
      agentId: "agent-candidates",
      memoryScope: scope,
      memoryStore: store,
      memoryProvider: extractingProvider(),
      memoryCandidateSink: sink,
      memoryRuntimePolicy: { ...writesOff, sessionExtraction: "candidate_sink" },
    })
    await collectText(runner.run({ sessionId: "candidate-extraction", goal: "say done" }))

    expect(spy.put).toBe(0)
    const held = sink.candidates("agent-candidates")
    expect(held.map(candidate => candidate.record.name)).toEqual(["brief-style", "brief-length"])
    expect(held.every(candidate => candidate.source === "session_extraction")).toBe(true)
    expect(new Set(held.map(candidate => candidate.candidate_id)).size).toBe(2)

    const events = await kinds(sessionLog, "candidate-extraction")
    const pending = events.filter(event => event.kind === "memory_candidate_pending")
    expect(pending).toHaveLength(2)
    expect(pending[0]).toMatchObject({ source: "session_extraction", name: "brief-style", receipt_id: expect.any(String) })
    expect(events.some(event => event.kind === "memory_written")).toBe(false)
  })

  it("S03 control · without a candidate_sink policy the same extraction writes to the store", async () => {
    const sink = new InMemoryMemoryCandidateSink()
    const { store, spy } = storeSpy()
    const runner = new RuntimeRunner({
      provider: textProvider(),
      sessionLog: new InMemorySessionLog(),
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 1024,
      agentId: "agent-candidates",
      memoryScope: scope,
      memoryStore: store,
      memoryProvider: extractingProvider(),
      memoryCandidateSink: sink,
    })
    await collectText(runner.run({ sessionId: "candidate-extraction-control", goal: "say done" }))
    expect(spy.put).toBe(2)
    expect(sink.candidates()).toHaveLength(0)
  })

  it("two runs in the same session produce distinct extraction candidates", async () => {
    const sink = new InMemoryMemoryCandidateSink()
    const { store, spy } = storeSpy()
    const sessionLog = new InMemorySessionLog()
    const runner = new RuntimeRunner({ provider: textProvider(), sessionLog,
      executionPlane: new LocalExecutionPlane(), maxTokens: 1024,
      agentId: "agent-candidates", memoryScope: scope, memoryStore: store,
      memoryProvider: extractingProvider(), memoryCandidateSink: sink,
      memoryRuntimePolicy: { ...writesOff, sessionExtraction: "candidate_sink" } })
    await collectText(runner.run({ sessionId: "reused", goal: "first" }))
    await collectText(runner.run({ sessionId: "reused", goal: "second" }))
    expect(sink.candidates()).toHaveLength(4)
    expect(new Set(sink.candidates().map(c => c.candidate_id)).size).toBe(4)
    expect(spy.put).toBe(0)
    expect((await kinds(sessionLog, "reused")).filter(e => e.kind === "memory_candidate_rejected")).toHaveLength(0)
  })

  it("a candidate the sink rejects is audited as rejected, with the sink's reason", async () => {
    const rejecting: MemoryCandidateSink = {
      async submit(_agentId, candidate): Promise<MemoryCandidateReceipt> {
        return { status: "rejected", candidate_id: candidate.candidate_id, receipt_id: "r-1", reason_code: "source_unverifiable" }
      },
    }
    const sessionLog = new InMemorySessionLog()
    const { store, spy } = storeSpy()
    const runner = new RuntimeRunner({
      provider: textProvider(),
      sessionLog,
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 1024,
      agentId: "agent-candidates",
      memoryScope: scope,
      memoryStore: store,
      memoryProvider: extractingProvider(),
      memoryCandidateSink: rejecting,
      memoryRuntimePolicy: { ...writesOff, sessionExtraction: "candidate_sink" },
    })
    await collectText(runner.run({ sessionId: "candidate-rejected", goal: "say done" }))
    expect(spy.put).toBe(0)
    const rejected = (await kinds(sessionLog, "candidate-rejected"))
      .filter(event => event.kind === "memory_candidate_rejected")
    expect(rejected).toHaveLength(2)
    expect(rejected[0]).toMatchObject({ reason_code: "source_unverifiable", receipt_id: "r-1" })
  })

  it("S03 · a semantic archive summary goes to the sink as a candidate, not to the store", async () => {
    const memorySummarizer: MemorySummarizer = { summarize: async () => "the archived turns pinged" }
    const sink = new InMemoryMemoryCandidateSink()
    const { store, spy } = storeSpy()
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
    const plane = new LocalExecutionPlane()
    plane.register(tool("fill", "fill", { type: "object", properties: { n: { type: "number" } } }, () => "w".repeat(200)))
    const sessionLog = new InMemorySessionLog()
    const runner = new RuntimeRunner({
      provider,
      sessionLog,
      executionPlane: plane,
      // The tool must be in the exposure baseline, or its calls never run and nothing is compacted.
      baselineToolIds: ["fill"],
      maxTokens: 400,
      maxTurns: 20,
      agentId: "agent-candidates",
      memoryScope: scope,
      memoryStore: store,
      memorySummarizer,
      memoryCandidateSink: sink,
      memoryRuntimePolicy: { ...writesOff, semanticArchive: "candidate_sink" },
    })
    await collectText(runner.run({ sessionId: "candidate-archive", goal: "fill until compact" }))

    expect(spy.put).toBe(0)
    const held = sink.candidates("agent-candidates")
    expect(held.length).toBeGreaterThan(0)
    expect(held.every(candidate => candidate.source === "semantic_archive")).toBe(true)
    const events = await kinds(sessionLog, "candidate-archive")
    expect(events.filter(event => event.kind === "memory_candidate_pending")).toHaveLength(held.length)
    expect(events.some(event => event.kind === "semantic_archive_completed")).toBe(false)
    expect(events.some(event => event.kind === "memory_written")).toBe(false)
  })

  describe("the PersistMemory effect path", () => {
    function runnerWith(sink: MemoryCandidateSink | undefined, policy: MemoryRuntimePolicy | undefined) {
      const { store, spy } = storeSpy()
      const runner = new RuntimeRunner({
        provider: textProvider(),
        sessionLog: new InMemorySessionLog(),
        executionPlane: new LocalExecutionPlane(),
        maxTokens: 1024,
        agentId: "agent-candidates",
        memoryScope: scope,
        memoryStore: store,
        ...(sink ? { memoryCandidateSink: sink } : {}),
        ...(policy ? { memoryRuntimePolicy: policy } : {}),
      })
      const persist = (effectId: string, content = "prefers numbered sections") =>
        (runner as unknown as {
          persistMemoryEffectResult(
            operationId: string, effectId: string, memory: Record<string, unknown>,
          ): Promise<Record<string, unknown>>
        }).persistMemoryEffectResult("op-1", effectId, {
          name: "brief-style", kind: "project", content, accepted_at_ms: 1_700_000_000_000,
        })
      return { persist, spy }
    }

    it("answers with a pending candidate result naming the sink's receipt", async () => {
      const sink = new InMemoryMemoryCandidateSink()
      const { persist, spy } = runnerWith(sink, { ...writesOff, modelWrites: "candidate_sink" })
      const result = await persist("effect-3")
      expect(result).toMatchObject({
        kind: "memory_candidate_result",
        effect_id: "effect-3",
        status: "pending",
        candidate_id: expect.stringMatching(/^candidate:/),
        receipt_id: expect.any(String),
      })
      expect(spy.put).toBe(0)
      expect(sink.candidates()[0]?.source).toBe("child_request")
    })

    it("S04 · a lost acknowledgement is redelivered as the same candidate and gets the original receipt", async () => {
      // The sink stores the candidate, then the answer is lost on the way back.
      const inner = new InMemoryMemoryCandidateSink()
      let dropNextAnswer = true
      const lossy: MemoryCandidateSink = {
        async submit(agentId: string, candidate: MemoryCandidate) {
          const receipt = await inner.submit(agentId, candidate)
          if (dropNextAnswer) {
            dropNextAnswer = false
            throw new Error("connection reset after the sink stored the candidate")
          }
          return receipt
        },
      }
      const { persist } = runnerWith(lossy, { ...writesOff, modelWrites: "candidate_sink" })

      const lost = await persist("effect-3")
      expect(lost).toMatchObject({ kind: "memory_candidate_result", effect_id: "effect-3" })
      expect(String(lost.error)).toContain("connection reset")

      const redelivered = await persist("effect-3")
      const held = inner.candidates()
      expect(held).toHaveLength(1)
      expect(redelivered).toMatchObject({ status: "pending", candidate_id: held[0].candidate_id })
      expect(redelivered.receipt_id).toBe((await inner.submit("agent-candidates", held[0])).receipt_id)
    })

    it("a different effect is a different candidate", async () => {
      const sink = new InMemoryMemoryCandidateSink()
      const { persist } = runnerWith(sink, { ...writesOff, modelWrites: "candidate_sink" })
      const first = await persist("effect-3")
      const second = await persist("effect-4")
      expect(second.candidate_id).not.toBe(first.candidate_id)
      expect(sink.candidates()).toHaveLength(2)
    })

    it("without a candidate_sink policy the effect still persists to the store", async () => {
      const sink = new InMemoryMemoryCandidateSink()
      const { persist, spy } = runnerWith(sink, undefined)
      const result = await persist("effect-3")
      expect(result).toMatchObject({ kind: "memory_persist_result", effect_id: "effect-3", record_ref: expect.any(String) })
      expect(spy.put).toBe(1)
      expect(sink.candidates()).toHaveLength(0)
    })

    it("the kernel's pending observation becomes the same session event the host paths write", () => {
      const event = kernelObservationToSessionEvent(
        { kind: "memory_candidate_pending", turn: 2, candidate_id: "cand-1", name: "brief-style" } as never,
        2,
      )
      expect(event).toEqual({
        kind: "memory_candidate_pending", turn: 2, candidate_id: "cand-1", source: "child_request", name: "brief-style",
      })
    })
  })
})
