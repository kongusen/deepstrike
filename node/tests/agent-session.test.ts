import { Agent } from "../src/agent.js"
import type { LLMProvider, Message, RenderedContext, StreamEvent, ToolSchema } from "../src/types.js"

class CapturingProvider implements LLMProvider {
  readonly calls: RenderedContext[] = []

  async complete(_context: RenderedContext, _tools: ToolSchema[]): Promise<Message> {
    return { role: "assistant", content: "unused", toolCalls: [] }
  }

  async *stream(context: RenderedContext, _tools: ToolSchema[]): AsyncIterable<StreamEvent> {
    this.calls.push(context)
    yield { type: "text_delta", delta: `answer-${this.calls.length}` }
  }
}

describe("Agent session continuity", () => {
  it("replays prior messages when the same session id is reused", async () => {
    const provider = new CapturingProvider()
    const agent = new Agent(provider, { maxTokens: 2048 })

    await agent.run("My name is Ada.", undefined, undefined, "chat-1")
    await agent.run("What is my name?", undefined, undefined, "chat-1")

    expect(provider.calls[1].turns).toEqual(expect.arrayContaining([
      expect.objectContaining({ role: "user", content: "My name is Ada." }),
      expect.objectContaining({ role: "assistant", content: "answer-1" }),
      expect.objectContaining({ role: "user", content: "What is my name?" }),
    ]))
  })

  it("keeps different session ids isolated", async () => {
    const provider = new CapturingProvider()
    const agent = new Agent(provider, { maxTokens: 2048 })

    await agent.run("Secret for A", undefined, undefined, "chat-a")
    await agent.run("Question for B", undefined, undefined, "chat-b")

    expect(provider.calls[1].turns).not.toEqual(expect.arrayContaining([
      expect.objectContaining({ content: "Secret for A" }),
    ]))
  })
})
