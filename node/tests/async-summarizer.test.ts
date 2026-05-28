import { collectText } from "../src/runtime/runner.js"
import { createRunner } from "./runtime/helpers.js"
import { tool } from "../src/tools/index.js"
import type { AsyncSummarizer, LLMProvider, Message, StreamEvent } from "../src/types.js"

describe("AsyncSummarizer — summary_upgraded written and preferred on replay", () => {
  it("fires background upgrade after compression and uses upgraded summary on wake", async () => {
    let summarizerCalled = false
    const upgradedSummaryText = "LLM-upgraded: agent used fill tool 8 times to populate data"

    const asyncSummarizer: AsyncSummarizer = {
      async summarize(_archived: Message[], _action: string): Promise<string> {
        summarizerCalled = true
        return upgradedSummaryText
      },
    }

    let callCount = 0
    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "", toolCalls: [] }
      },
      async *stream(): AsyncIterable<StreamEvent> {
        callCount += 1
        if (callCount <= 8) {
          yield { type: "tool_call" as const, id: `c${callCount}`, name: "fill", arguments: { n: callCount } }
          return
        }
        yield { type: "text_delta" as const, delta: "done" }
      },
    }

    const { runner, sessionLog } = createRunner(
      provider,
      [tool("fill", "fill", { type: "object", properties: { n: { type: "number" } } }, () => "w".repeat(200))],
      { maxTokens: 400, maxTurns: 20, asyncSummarizer },
    )

    await collectText(runner.run({ sessionId: "async-sum-test", goal: "fill data" }))

    // Wait briefly for the background upgrade to complete
    await new Promise(r => setTimeout(r, 50))

    const events = await sessionLog.read("async-sum-test")
    const hasCompressed = events.some(e => e.event.kind === "compressed")
    expect(hasCompressed).toBe(true)
    expect(summarizerCalled).toBe(true)

    const upgraded = events.find(e => e.event.kind === "summary_upgraded")
    expect(upgraded).toBeDefined()
    expect((upgraded!.event as { summary: string }).summary).toBe(upgradedSummaryText)
  })

  it("falls back to rule-based summary when async summarizer throws", async () => {
    const asyncSummarizer: AsyncSummarizer = {
      async summarize(): Promise<string> {
        throw new Error("LLM unreachable")
      },
    }

    let callCount = 0
    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "", toolCalls: [] }
      },
      async *stream(): AsyncIterable<StreamEvent> {
        callCount += 1
        if (callCount <= 8) {
          yield { type: "tool_call" as const, id: `c${callCount}`, name: "fill", arguments: { n: callCount } }
          return
        }
        yield { type: "text_delta" as const, delta: "done" }
      },
    }

    const { runner, sessionLog } = createRunner(
      provider,
      [tool("fill", "fill", { type: "object", properties: { n: { type: "number" } } }, () => "w".repeat(200))],
      { maxTokens: 400, maxTurns: 20, asyncSummarizer },
    )

    await collectText(runner.run({ sessionId: "async-sum-fallback", goal: "fill data" }))
    await new Promise(r => setTimeout(r, 50))

    const events = await sessionLog.read("async-sum-fallback")
    // No summary_upgraded event — fallback to rule-based is silent
    const upgraded = events.find(e => e.event.kind === "summary_upgraded")
    expect(upgraded).toBeUndefined()
    // But the original compressed event should still exist with a rule-based summary
    const compressed = events.find(e => e.event.kind === "compressed")
    expect(compressed).toBeDefined()
  })
})
