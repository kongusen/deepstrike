import { collectText } from "../../src/runtime/runner.js"
import { measurementForPlan } from "../../src/providers/request-plan.js"
import { createRunner } from "./helpers.js"
import type { LLMProvider, Message, RenderedContext, StreamEvent, ToolSchema } from "../../src/types.js"

class MeasuredProvider implements LLMProvider {
  countCalls = 0
  streamCalls = 0

  descriptor() {
    return {
      provider: "test",
      protocol: "openai-chat" as const,
      model: "fixture",
      reasoning: { supported: false, preserveAcrossToolTurns: false },
      toolCalls: { supported: false, requiresStrictPairing: false },
    }
  }

  async countTokens(_context: RenderedContext, _tools: ToolSchema[]) {
    this.countCalls += 1
    return { inputTokens: 12, source: { kind: "native" as const, provider: "test" }, confidence: "exact" as const }
  }

  async complete(): Promise<Message> {
    return { role: "assistant", content: "done", toolCalls: [] }
  }

  async *stream(): AsyncIterable<StreamEvent> {
    this.streamCalls += 1
    yield { type: "text_delta", delta: "done" }
  }
}

describe("spc_015-08 host prompt measurement", () => {
  it("records one native measurement before provider execution", async () => {
    const provider = new MeasuredProvider()
    const { runner, sessionLog } = createRunner(provider, [], { maxTokens: 256 })

    await expect(collectText(runner.run({ sessionId: "measurement-native", goal: "hello" }))).resolves.toBe("done")
    expect(provider.countCalls).toBe(1)
    expect(provider.streamCalls).toBe(1)
    const events = await sessionLog.read("measurement-native")
    expect(events.filter(entry => entry.event.kind === "prompt_measured")).toHaveLength(1)
    expect(JSON.stringify(events)).not.toContain("apiKey")
  })

  it("falls back to a durable heuristic when native measurement fails", async () => {
    const provider = new MeasuredProvider()
    provider.countTokens = async () => { provider.countCalls += 1; throw new Error("meter unavailable") }
    const { runner, sessionLog } = createRunner(provider, [], { maxTokens: 256 })

    await expect(collectText(runner.run({ sessionId: "measurement-fallback", goal: "hello" }))).resolves.toBe("done")
    const measured = (await sessionLog.read("measurement-fallback"))
      .map(entry => entry.event)
      .find(event => event.kind === "prompt_measured")
    expect(measured).toMatchObject({ measurement: { source: { kind: "heuristic" }, confidence: "low_confidence" } })
  })

  it("fails before streaming when the measured prompt exceeds the context budget", async () => {
    const provider = new MeasuredProvider()
    provider.countTokens = async () => {
      provider.countCalls += 1
      return { inputTokens: 128, source: { kind: "native" as const, provider: "test" }, confidence: "exact" as const }
    }
    const { runner } = createRunner(provider, [], { maxTokens: 64 })

    await expect(collectText(runner.run({ sessionId: "measurement-overflow", goal: "hello" }))).resolves.toBe("")
    expect(provider.countCalls).toBe(1)
    expect(provider.streamCalls).toBe(0)
  })

  it("rejects malformed recorded measurements instead of treating them as authoritative", async () => {
    const plan = { fingerprint: "sha256:test" as const }
    expect(measurementForPlan(plan, {
      version: 1, requestFingerprint: plan.fingerprint, inputTokens: -1,
      source: { kind: "heuristic" }, confidence: "low_confidence",
    } as never)).toBeUndefined()
  })
})
