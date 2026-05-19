import { streamingTool, tool } from "../src/tools/index.js"
import type { LLMProvider, Message, RenderedContext, StreamEvent, ToolSchema } from "../src/types.js"
import { createRunner } from "./runtime/helpers.js"

class ToolStreamingProvider implements LLMProvider {
  private callCount = 0

  async complete(_context: RenderedContext, _tools: ToolSchema[]): Promise<Message> {
    return { role: "assistant", content: "unused", toolCalls: [] }
  }

  async *stream(): AsyncIterable<StreamEvent> {
    this.callCount += 1
    if (this.callCount === 1) {
      yield { type: "tool_call", id: "call_1", name: "compose", arguments: {} }
      return
    }
    yield { type: "text_delta", delta: "done" }
  }
}

describe("streaming tools", () => {
  it("passes tool output chunks through the runner stream and still returns the aggregated result", async () => {
    const { runner } = createRunner(
      new ToolStreamingProvider(),
      [streamingTool(
        "compose",
        "Compose output",
        { type: "object", properties: {} },
        async function* () {
          yield "hello"
          yield " "
          yield "world"
        },
      )],
      { maxTurns: 4 },
    )

    const events = []
    for await (const event of runner.run({ sessionId: "stream-1", goal: "compose once" })) events.push(event)

    expect(events).toEqual(expect.arrayContaining([
      { type: "tool_call", id: "call_1", name: "compose", arguments: {} },
      expect.objectContaining({ type: "tool_delta", callId: "call_1", name: "compose", delta: "hello" }),
      expect.objectContaining({ type: "tool_delta", callId: "call_1", name: "compose", delta: " " }),
      expect.objectContaining({ type: "tool_delta", callId: "call_1", name: "compose", delta: "world" }),
      { type: "tool_result", callId: "call_1", name: "compose", content: "hello world", isError: false },
      { type: "text_delta", delta: "done" },
    ]))
  })

  it("keeps regular tools compatible", async () => {
    const { runner } = createRunner(
      new ToolStreamingProvider(),
      [tool("compose", "Compose output", { type: "object", properties: {} }, () => "hello world")],
      { maxTurns: 4 },
    )

    const events = []
    for await (const event of runner.run({ sessionId: "stream-2", goal: "compose once" })) events.push(event)

    expect(events).toContainEqual({ type: "tool_result", callId: "call_1", name: "compose", content: "hello world", isError: false })
    expect(events.find(event => event.type === "tool_delta")).toBeUndefined()
  })
})
