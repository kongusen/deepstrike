import { collectText } from "../../src/runtime/runner.js"
import { createRunner, tool } from "./helpers.js"
import type { MemorySummarizer, LLMProvider, Message, StreamEvent } from "../../src/types.js"
import type { MemoryStore } from "../../src/memory/protocols.js"

describe("semantic page_out → MemoryStore (Layer 5 contract)", () => {
  it("archives an LLM summary to MemoryStore on semantic page_out", async () => {
    let commitCalls = 0
    let lastSummary = ""

    const memoryStore: MemoryStore = {
      put: async (_agentId, record) => {
        commitCalls += 1
        lastSummary = record.content
      },
      get: async () => null,
      delete: async () => {},
      saveSession: async () => {},
      search: async () => [],
    }

    const memorySummarizer: MemorySummarizer = {
      async summarize(_archived, ctx) {
        return `long-term summary for ${ctx.action ?? "compress"}`
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
          yield { type: "tool_call", id: `c${callCount}`, name: "fill", arguments: { n: callCount } }
          return
        }
        yield { type: "text_delta", delta: "done" }
      },
    }

    const { runner } = createRunner(
      provider,
      [tool("fill", "fill", { type: "object", properties: { n: { type: "number" } } }, () => "w".repeat(200))],
      {
        maxTokens: 400,
        maxTurns: 20,
        agentId: "agent-semantic",
        memoryScope: { tenant_id: "agent-semantic", namespace: "integration" },
        memoryStore,
        memorySummarizer,
      },
    )

    await collectText(runner.run({ sessionId: "semantic-page-out", goal: "fill until compact" }))

    expect(commitCalls).toBeGreaterThan(0)
    expect(lastSummary).toContain("long-term summary")
  })
})
