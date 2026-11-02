/**
 * I4 (preQueryMemory): the proactive pre-turn-1 memory fetch lands each hit as an unpinned keyed
 * `memory:<record_id>` knowledge entry — the same placement the renewal re-query uses, so one key
 * dedupes a record across both. Unpinned entries are evicted at a boundary when usage needs the
 * room, so a prefetch does not hold its slot forever.
 */
import { createRunner } from "./runtime/helpers.js"
import { collectText } from "../src/runtime/runner.js"
import type { MemoryStore, MemoryRecall } from "../src/memory/protocols.js"
import type { LLMProvider, ModelMessage, RenderedContext, StreamEvent } from "../src/types.js"

const RECALL = "PREFETCHED_LONGTERM_FACT"
const scope = { tenant_id: "agent-prequery", namespace: "prefetch" }

function memoryStore(): MemoryStore {
  return {
    put: async () => {},
    get: async () => null,
    delete: async () => {},
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

describe("preQueryMemory prefetch lands in knowledge, not history", () => {
  it("surfaces the prefetched content in systemKnowledge on turn 1, and never in turns", async () => {
    let sawInTurns = false
    let sawInKnowledge = false
    const provider: LLMProvider = {
      async complete(): Promise<ModelMessage> {
        return { role: "assistant", content: "unused", toolCalls: [] }
      },
      async *stream(context: RenderedContext): AsyncIterable<StreamEvent> {
        if (JSON.stringify(context.turns).includes("prefetch fixture")) sawInTurns = true
        if ((context.systemKnowledge ?? "").includes("prefetch fixture")) sawInKnowledge = true
        yield { type: "text_delta", delta: "done" }
      },
    }

    const { runner } = createRunner(provider, [], {
      agentId: "agent-prequery",
      memoryScope: scope,
      memoryStore: memoryStore(),
    })
    ;(runner as unknown as { opts: { preQueryMemory: unknown } }).opts.preQueryMemory = () => [{
      scope, query: "past facts", top_k: 5, kinds: [],
    }]

    await collectText(runner.run({ sessionId: "prequery", goal: "use the fact" }))

    expect(sawInKnowledge).toBe(true)
    expect(sawInTurns).toBe(false)
  })
})
