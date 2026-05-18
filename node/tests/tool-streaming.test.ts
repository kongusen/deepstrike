import { Agent } from "../src/agent.js"
import { streamingTool, tool } from "../src/tools/index.js"
import type { LLMProvider, Message, RenderedContext, StreamEvent, ToolSchema } from "../src/types.js"

class ToolStreamingProvider implements LLMProvider {
  private callCount = 0

  async complete(_context: RenderedContext, _tools: ToolSchema[]): Promise<Message> {
    return { role: "assistant", content: "unused" }
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
  it("passes tool output chunks through the agent stream and still returns the aggregated result", async () => {
    const provider = new ToolStreamingProvider()
    const agent = new Agent(provider, { maxTokens: 2048, maxTurns: 4 })
      .register(streamingTool(
        "compose",
        "Compose output",
        { type: "object", properties: {} },
        async function* () {
          yield "hello"
          yield " "
          yield "world"
        },
      ))

    const events = []
    for await (const event of agent.runStreaming("compose once")) events.push(event)

    expect(events).toEqual(expect.arrayContaining([
      { type: "tool_call", id: "call_1", name: "compose", arguments: {} },
      { type: "tool_delta", callId: "call_1", name: "compose", delta: "hello" },
      { type: "tool_delta", callId: "call_1", name: "compose", delta: " " },
      { type: "tool_delta", callId: "call_1", name: "compose", delta: "world" },
      { type: "tool_result", callId: "call_1", name: "compose", content: "hello world", isError: false },
      { type: "text_delta", delta: "done" },
    ]))
  })

  it("keeps regular tools compatible", async () => {
    const provider = new ToolStreamingProvider()
    const agent = new Agent(provider, { maxTokens: 2048, maxTurns: 4 })
      .register(tool("compose", "Compose output", { type: "object", properties: {} }, () => "hello world"))

    const events = []
    for await (const event of agent.runStreaming("compose once")) events.push(event)

    expect(events).toContainEqual({ type: "tool_result", callId: "call_1", name: "compose", content: "hello world", isError: false })
    expect(events.find(event => event.type === "tool_delta")).toBeUndefined()
  })
})
