import type { LLMProvider, Message, RenderedContext, StreamEvent, ToolSchema } from "../src/types.js"
import { collectText } from "../src/runtime/runner.js"
import { createRunner } from "./runtime/helpers.js"

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

describe("RuntimeRunner session continuity", () => {
  it("replays prior messages when the same session id is reused", async () => {
    const provider = new CapturingProvider()
    const { runner } = createRunner(provider, [], { maxTokens: 2048 })

    await collectText(runner.run({ sessionId: "chat-1", goal: "My name is Ada." }))
    await collectText(runner.run({ sessionId: "chat-1", goal: "What is my name?" }))

    expect(provider.calls[1].turns).toEqual(expect.arrayContaining([
      expect.objectContaining({ role: "user", content: "My name is Ada." }),
      expect.objectContaining({ role: "assistant", content: "answer-1" }),
    ]))
    // goal lands in systemText, systemStable (rebuilt kernel), stateTurn (mid kernel), or turns[0] (legacy)
    const ctx = provider.calls[1]
    const allContent = [ctx.systemText, ctx.systemStable, ctx.systemKnowledge, ctx.stateTurn?.content, ...ctx.turns.map(m => m.content)].filter(Boolean).join("\n")
    expect(allContent).toContain("What is my name?")
  })

  it("keeps different session ids isolated", async () => {
    const provider = new CapturingProvider()
    const { runner } = createRunner(provider, [], { maxTokens: 2048 })

    await collectText(runner.run({ sessionId: "chat-a", goal: "Secret for A" }))
    await collectText(runner.run({ sessionId: "chat-b", goal: "Question for B" }))

    expect(provider.calls[1].turns).not.toEqual(expect.arrayContaining([
      expect.objectContaining({ content: "Secret for A" }),
    ]))
  })
})
