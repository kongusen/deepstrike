/**
 * I4 (preQueryMemory) rerouted: strict dynamic context control means a proactive pre-turn-1 memory
 * fetch is still single-use retrieval content, not a stable skill — so it lands in `history` (an
 * ordinary turn the model sees on turn 1) rather than pinning itself into the durable `knowledge`
 * slot forever. It decays with the compression pyramid exactly like a live `memory` tool result.
 */
import { createRunner } from "./runtime/helpers.js"
import { collectText } from "../src/runtime/runner.js"
import type { DreamStore, MemoryEntry } from "../src/memory/protocols.js"
import type { LLMProvider, Message, RenderedContext, StreamEvent } from "../src/types.js"

const RECALL = "PREFETCHED_LONGTERM_FACT"

function dreamStore(): DreamStore {
  return {
    loadSessions: async () => [],
    loadMemories: async () => [],
    commit: async () => {},
    saveSession: async () => {},
    search: async () => [{ text: RECALL, score: 0.9, metadata: null } satisfies MemoryEntry],
  }
}

describe("preQueryMemory prefetch lands in history, not knowledge", () => {
  it("surfaces the prefetched content in turns on turn 1, and never in systemKnowledge", async () => {
    let sawInTurns = false
    let sawInKnowledge = false
    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "unused", toolCalls: [] }
      },
      async *stream(context: RenderedContext): AsyncIterable<StreamEvent> {
        if (JSON.stringify(context.turns).includes(RECALL)) sawInTurns = true
        if ((context.systemKnowledge ?? "").includes(RECALL)) sawInKnowledge = true
        yield { type: "text_delta", delta: "done" }
      },
    }

    const { runner } = createRunner(provider, [], {
      agentId: "agent-prequery",
      dreamStore: dreamStore(),
    })
    ;(runner as unknown as { opts: { preQueryMemory: unknown } }).opts.preQueryMemory = () => ["past facts"]

    await collectText(runner.run({ sessionId: "prequery", goal: "use the fact" }))

    expect(sawInTurns).toBe(true)
    expect(sawInKnowledge).toBe(false)
  })
})
