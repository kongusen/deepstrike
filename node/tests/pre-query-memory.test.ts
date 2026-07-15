/**
 * I4 (preQueryMemory) rerouted: strict dynamic context control means a proactive pre-turn-1 memory
 * fetch is still single-use retrieval content, not a stable skill — so it lands in `history` (an
 * ordinary turn the model sees on turn 1) rather than pinning itself into the durable `knowledge`
 * slot forever. It decays with the compression pyramid exactly like a live `memory` tool result.
 */
import { createRunner } from "./runtime/helpers.js"
import { collectText } from "../src/runtime/runner.js"
import type { DreamStore, MemoryRecall } from "../src/memory/protocols.js"
import type { LLMProvider, Message, RenderedContext, StreamEvent } from "../src/types.js"

const RECALL = "PREFETCHED_LONGTERM_FACT"
const scope = { tenant_id: "agent-prequery", namespace: "prefetch" }

function dreamStore(): DreamStore {
  return {
    upsert: async () => {},
    saveSession: async () => {},
    search: async () => [{
      record: {
        record_id: "record-prefetch", scope, name: "prefetched", kind: "reference", content: RECALL,
        description: "prefetch fixture",
        provenance: { author: "host", trust: "host_verified", evidence_refs: [] },
        created_at: 1, updated_at: 1, recall_count: 0, confidence: 0.9, links: [], pinned: false,
      },
      score: 0.9,
      why: "fixture",
    } satisfies MemoryRecall],
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
      memoryScope: scope,
      dreamStore: dreamStore(),
    })
    ;(runner as unknown as { opts: { preQueryMemory: unknown } }).opts.preQueryMemory = () => [{
      scope, query: "past facts", top_k: 5, kinds: [],
    }]

    await collectText(runner.run({ sessionId: "prequery", goal: "use the fact" }))

    expect(sawInTurns).toBe(true)
    expect(sawInKnowledge).toBe(false)
  })
})
