import { RuntimeRunner } from "../../src/runtime/runner.js"
import { InMemorySessionLog } from "../../src/runtime/session-log.js"
import { LocalExecutionPlane } from "../../src/runtime/execution-plane.js"
import { kernelObservationToSessionEvent } from "../../src/runtime/kernel-event-log.js"
import {
  InMemoryMemoryCandidateSink,
  type MemoryCandidate,
  type MemoryCandidateReceipt,
  type MemoryCandidateSink,
} from "../../src/memory/candidate-sink.js"
import type { LLMProvider, ModelMessage, StreamEvent } from "../../src/types.js"
import type { MemoryRuntimePolicy } from "../../src/memory/runtime-policy.js"
import type { MemoryStore } from "../../src/memory/protocols.js"

/** M2 · the candidate sink through the runner's PersistMemory effect path. */

const scope = { tenant_id: "agent-candidates", namespace: "candidates" }

const writesOff: MemoryRuntimePolicy = {
  retrieval: "disabled",
  modelWrites: "disabled",
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

describe("memory candidate sink through the runner (M2)", () => {
  it("refuses a candidate_sink path without a sink at construction", () => {
    expect(() => new RuntimeRunner({
      provider: textProvider(),
      sessionLog: new InMemorySessionLog(),
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 1024,
      memoryRuntimePolicy: { ...writesOff, modelWrites: "candidate_sink" },
    })).toThrow("needs RuntimeOptions.memoryCandidateSink")
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

    it("redelivers a lost acknowledgement as the same candidate", async () => {
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
      expect(String(lost.error)).toContain("connection reset")
      const redelivered = await persist("effect-3")
      const held = inner.candidates()
      expect(held).toHaveLength(1)
      expect(redelivered).toMatchObject({ status: "pending", candidate_id: held[0].candidate_id })
      expect(redelivered.receipt_id).toBe((await inner.submit("agent-candidates", held[0])).receipt_id)
    })

    it("uses a different candidate for a different effect", async () => {
      const sink = new InMemoryMemoryCandidateSink()
      const { persist } = runnerWith(sink, { ...writesOff, modelWrites: "candidate_sink" })
      const first = await persist("effect-3")
      const second = await persist("effect-4")
      expect(second.candidate_id).not.toBe(first.candidate_id)
      // The same fact proposed by another effect is not reviewed twice.
      expect(second).toMatchObject({ status: "rejected", reason_code: "already_pending_review" })
      expect(sink.candidates()).toHaveLength(1)
    })

    it("writes to the store when candidate routing is not selected", async () => {
      const sink = new InMemoryMemoryCandidateSink()
      const { persist, spy } = runnerWith(sink, undefined)
      const result = await persist("effect-3")
      expect(result).toMatchObject({ kind: "memory_persist_result", effect_id: "effect-3", record_ref: expect.any(String) })
      expect(spy.put).toBe(1)
      expect(sink.candidates()).toHaveLength(0)
    })

    it("maps the kernel pending observation to the child-request session event", () => {
      const event = kernelObservationToSessionEvent(
        { kind: "memory_candidate_pending", turn: 2, candidate_id: "cand-1", name: "brief-style" } as never,
        2,
      )
      expect(event).toEqual({
        kind: "memory_candidate_pending", turn: 2, candidate_id: "cand-1", source: "child_request", name: "brief-style",
      })
    })
  })

  it("audits a rejected child-request candidate without writing it", async () => {
    const rejecting: MemoryCandidateSink = {
      async submit(_agentId, candidate): Promise<MemoryCandidateReceipt> {
        return { status: "rejected", candidate_id: candidate.candidate_id, receipt_id: "r-1", reason_code: "source_unverifiable" }
      },
    }
    const { store, spy } = storeSpy()
    const runner = new RuntimeRunner({
      provider: textProvider(),
      sessionLog: new InMemorySessionLog(),
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 1024,
      agentId: "agent-candidates",
      memoryScope: scope,
      memoryStore: store,
      memoryCandidateSink: rejecting,
      memoryRuntimePolicy: { ...writesOff, modelWrites: "candidate_sink" },
    })
    const result = await (runner as unknown as {
      persistMemoryEffectResult(operationId: string, effectId: string, memory: Record<string, unknown>): Promise<Record<string, unknown>>
    }).persistMemoryEffectResult("op-1", "effect-3", {
      name: "brief-style", kind: "project", content: "proposal", accepted_at_ms: 1_700_000_000_000,
    })
    expect(result).toMatchObject({ status: "rejected", reason_code: "source_unverifiable" })
    expect(spy.put).toBe(0)
  })
})
