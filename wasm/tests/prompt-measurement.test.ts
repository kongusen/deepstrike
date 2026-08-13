import { collectText, InMemorySessionLog, LocalExecutionPlane, RuntimeRunner } from "../src/runtime/index.js"
import type { LLMProvider, Message, RenderedContext, StreamEvent, ToolSchema } from "../src/types.js"

class ReservedBudgetProvider implements LLMProvider {
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
    return { inputTokens: 55, source: { kind: "native" as const, provider: "test" }, confidence: "exact" as const }
  }

  async complete(): Promise<Message> {
    return { role: "assistant", content: "done", toolCalls: [] }
  }

  async *stream(): AsyncIterable<StreamEvent> {
    this.streamCalls += 1
    yield { type: "text_delta", delta: "should-not-run" }
  }
}

describe("spc_015-08 WASM host prompt measurement", () => {
  it("blocks a native-measured request when prompt reserves push it over maxTokens", async () => {
    const provider = new ReservedBudgetProvider()
    const runner = new RuntimeRunner({
      provider,
      sessionLog: new InMemorySessionLog(),
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 64,
      promptBudget: { promptOverheadTokens: 4, outputReserveTokens: 4, safetyMarginTokens: 2 },
    })

    await expect(collectText(runner.run({ sessionId: "measurement-reserved-overflow", goal: "hello" }))).resolves.toBe("")
    expect(provider.countCalls).toBeGreaterThan(0)
    expect(provider.streamCalls).toBe(0)
  })
})
