import { collectText } from "../../src/runtime/runner.js"
import { createRunner, tool } from "./helpers.js"
import type { DreamSummarizer, LLMProvider, Message, StreamEvent } from "../../src/types.js"
import type { CurationResult, DreamStore } from "../../src/memory/protocols.js"

describe("semantic page_out → DreamStore (Layer 5 contract)", () => {
  it("archives an LLM summary to DreamStore on semantic page_out", async () => {
    let commitCalls = 0
    let lastSummary = ""

    const dreamStore: DreamStore = {
      loadSessions: async () => [],
      loadMemories: async () => [],
      commit: async (_agentId, result: CurationResult) => {
        commitCalls += 1
        lastSummary = result.toAdd[0]?.text ?? ""
      },
      saveSession: async () => {},
      search: async () => [],
    }

    const dreamSummarizer: DreamSummarizer = {
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
        dreamStore,
        dreamSummarizer,
      },
    )

    await collectText(runner.run({ sessionId: "semantic-page-out", goal: "fill until compact" }))
    await new Promise(r => setTimeout(r, 50))

    expect(commitCalls).toBeGreaterThan(0)
    expect(lastSummary).toContain("long-term summary")
  })
})
