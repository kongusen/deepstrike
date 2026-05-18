import { Agent } from "../src/agent.js"
import { tool } from "../src/tools/index.js"
import type { LLMProvider, Message, ProviderRunState, RenderedContext, StreamEvent, ToolSchema } from "../src/types.js"

class StatefulTestProvider implements LLMProvider {
  readonly states: ProviderRunState[] = []
  private callCount = 0

  createRunState(): ProviderRunState {
    return { marker: crypto.randomUUID() }
  }

  async complete(_context: RenderedContext, _tools: ToolSchema[]): Promise<Message> {
    return { role: "assistant", content: "unused", toolCalls: [] }
  }

  async *stream(
    _context: RenderedContext,
    _tools: ToolSchema[],
    _extensions?: Record<string, unknown> | null,
    state?: ProviderRunState,
  ): AsyncIterable<StreamEvent> {
    this.states.push(state ?? {})
    this.callCount += 1

    if (this.callCount === 1) {
      yield { type: "tool_call", id: "call_1", name: "ping", arguments: {} }
      return
    }

    yield { type: "text_delta", delta: "done" }
  }
}

describe("Agent provider run state", () => {
  it("threads the same provider-owned state through every turn in one run", async () => {
    const provider = new StatefulTestProvider()
    const agent = new Agent(provider, { maxTokens: 2048, maxTurns: 4 })
      .register(tool("ping", "Ping", { type: "object", properties: {} }, () => "pong"))

    for await (const _event of agent.runStreaming("Use ping once, then finish.")) {}

    expect(provider.states).toHaveLength(2)
    expect(provider.states[0]).toBe(provider.states[1])
  })
})
