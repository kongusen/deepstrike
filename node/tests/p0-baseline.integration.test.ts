import { createRunner } from "./runtime/helpers.js"
import { collectText } from "../src/runtime/runner.js"
import { tool } from "../src/tools/index.js"
import type { LLMProvider, Message, RenderedContext, StreamEvent, ToolSchema } from "../src/types.js"

function usageTrackingProvider(): LLMProvider {
  let call = 0
  return {
    async complete(_ctx: RenderedContext, _tools: ToolSchema[]): Promise<Message> {
      return { role: "assistant", content: "done" }
    },
    async *stream(_context: RenderedContext): AsyncIterable<StreamEvent> {
      call += 1
      const inputTokens = 600 + call * 250
      yield {
        type: "usage",
        totalTokens: inputTokens + 80,
        inputTokens,
        outputTokens: 80,
      }
      if (call < 8) {
        yield { type: "tool_call", id: `c${call}`, name: "ping", arguments: {} }
        return
      }
      yield { type: "text_delta", delta: "done" }
    },
  }
}

describe("P0 baseline integration", () => {
  it("P0-1: observed usage prevents premature compression in long tool loop", async () => {
    const { runner, sessionLog } = createRunner(
      usageTrackingProvider(),
      [tool("ping", "ping", { type: "object", properties: {} }, async () => "pong")],
      { maxTokens: 10_000, maxTurns: 15 },
    )

    await collectText(runner.run({ sessionId: "p0-1-baseline", goal: "run tools" }))

    const events = await sessionLog.read("p0-1-baseline") as Array<{ event: { kind: string } }>
    const compressed = events.filter(e => e.event.kind === "compressed").length
    expect(compressed).toBe(0)
  })
})
