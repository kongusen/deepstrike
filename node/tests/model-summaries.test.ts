// Plan task 8.4 · the kernel's `summarize_context` effect: a range compaction asks the runner's
// model for its summary, which replaces the rule summary before the next provider call.
import { collectText } from "../src/runtime/runner.js"
import { createRunner } from "./runtime/helpers.js"
import { tool } from "../src/tools/index.js"
import type { LLMProvider, ModelMessage, RenderedContext, StreamEvent } from "../src/types.js"

const SUMMARY = "MODEL SUMMARY: filled rows 1-6, 6 rows so far"

function scripted(summarize: (context: RenderedContext) => AsyncIterable<StreamEvent>) {
  const states: string[] = []
  let summaryCalls = 0
  let calls = 0
  const provider: LLMProvider = {
    async complete(): Promise<ModelMessage> {
      return { role: "assistant", content: "", toolCalls: [] }
    },
    async *stream(context: RenderedContext): AsyncIterable<StreamEvent> {
      if ((context.systemStable ?? "").startsWith("You compress")) {
        summaryCalls += 1
        yield* summarize(context)
        return
      }
      calls += 1
      states.push(JSON.stringify(context.stateTurn ?? ""))
      if (calls <= 12) {
        yield { type: "text_delta", delta: `row ${calls} filled; ${calls} rows so far. ` }
        yield { type: "tool_call", id: `c${calls}`, name: "fill", arguments: { n: calls } }
        return
      }
      yield { type: "text_delta", delta: "done" }
    },
  }
  return { provider, states, summaryCalls: () => summaryCalls }
}

const fill = tool("fill", "fill", { type: "object", properties: { n: { type: "number" } } }, () => "w".repeat(600))

describe("model summaries (plan task 8.4)", () => {
  it("puts the model's summary in the task state and records the call", async () => {
    const { provider, states, summaryCalls } = scripted(async function* (context) {
      expect(String(context.turns[0]?.content)).toContain("Messages to summarize")
      yield { type: "text_delta", delta: SUMMARY }
      yield { type: "usage", totalTokens: 120, inputTokens: 100, outputTokens: 20 }
    })
    const { runner, sessionLog } = createRunner(provider, [fill], { maxTokens: 1200, maxTurns: 30, modelSummaries: true })
    await collectText(runner.run({ sessionId: "model-summary", goal: "fill rows" }))

    const events = (await sessionLog.read("model-summary")).map(entry => entry.event)
    const summarized = events.filter(event => event.kind === "context_summarized")
    expect(summaryCalls()).toBeGreaterThan(0)
    expect(summarized.length).toBe(summaryCalls())
    expect(summarized[0]).toMatchObject({ status: "success", summary: SUMMARY, usage: { inputTokens: 100, outputTokens: 20 } })
    expect(states.some(state => state.includes("model summary") && state.includes(SUMMARY))).toBe(true)
    const report = events.find(event => event.kind === "context_lifecycle_reported") as { model_summaries?: number; history?: Array<{ origin: string; items: number }> }
    expect(report.model_summaries).toBe(summaryCalls())
    expect(report.history?.find(tally => tally.origin === "summary")?.items).toBeGreaterThanOrEqual(summaryCalls())
  })

  it("keeps the rule summary when the model call fails", async () => {
    const { provider, states, summaryCalls } = scripted(async function* () {
      yield { type: "error", message: "model unreachable" }
    })
    const { runner, sessionLog } = createRunner(provider, [fill], { maxTokens: 1200, maxTurns: 30, modelSummaries: true })
    await collectText(runner.run({ sessionId: "model-summary-failed", goal: "fill rows" }))

    const events = (await sessionLog.read("model-summary-failed")).map(entry => entry.event)
    expect(summaryCalls()).toBeGreaterThan(0)
    expect(events.find(event => event.kind === "context_summarized")).toMatchObject({ status: "failed", error: expect.stringContaining("model unreachable") })
    expect(states.some(state => state.includes("[Compressed:"))).toBe(true)
    expect(states.some(state => state.includes("model summary"))).toBe(false)
  })

  it("never asks for a summary unless enabled", async () => {
    const { provider, summaryCalls } = scripted(async function* () {
      yield { type: "text_delta", delta: SUMMARY }
    })
    const { runner, sessionLog } = createRunner(provider, [fill], { maxTokens: 1200, maxTurns: 30 })
    await collectText(runner.run({ sessionId: "model-summary-off", goal: "fill rows" }))
    const events = (await sessionLog.read("model-summary-off")).map(entry => entry.event)
    expect(events.some(event => event.kind === "compressed")).toBe(true)
    expect(summaryCalls()).toBe(0)
  })
})
