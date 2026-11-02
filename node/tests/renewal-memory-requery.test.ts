/**
 * K4 — renewal-boundary memory re-query. A sprint renewal (ρ > 0.98) rebuilds history wholesale,
 * dropping earlier memory hits with it. The runner now re-fires the `preQueryMemory` prefetch on
 * the live `renewed` observation (phase: "renewal") so the new sprint starts with a fresh recall
 * pass — the symmetric counterpart of the turn-1 fetch (phase: "initial"). A running operation
 * accepts history only through effects or external events, so both the turn-1 and the renewal hits
 * enter as keyed `memory:<record_id>` knowledge entries carrying the canonical recall index line (M5).
 */
import { createRunner, tool } from "./runtime/helpers.js"
import { collectText } from "../src/runtime/runner.js"
import type { MemoryStore, MemoryRecall } from "../src/memory/protocols.js"
import type { LLMProvider, ModelMessage, RenderedContext, StreamEvent } from "../src/types.js"

const RECALL = "LONGTERM_FACT_FOR_SPRINT"
const RECALL_TURN = `[MEMORY record_id=record-renewal kind=reference trust=host_verified] ${RECALL}`
const scope = { tenant_id: "agent-k4", namespace: "renewal" }

describe("renewal-boundary memory re-query (K4)", () => {
  it("re-fires preQueryMemory with phase 'renewal' and lands hits in the new sprint's context", async () => {
    const phases: Array<string | undefined> = []
    let sawRecallAfterRenewal = false
    let sawRenewal = false
    let call = 0

    const memoryStore: MemoryStore = {
      put: async () => {},
      get: async () => null,
      delete: async () => {},
      saveSession: async () => {},
      search: async () => [{
        record: {
          record_id: "record-renewal", scope, name: "renewal-fact", kind: "reference", content: "full renewal record",
          description: RECALL,
          provenance: { author: "host", trust: "host_verified", evidence_refs: [] },
          created_at: 1, updated_at: 1, recall_count: 0, confidence: 0.9, links: [], pinned: false,
        },
        score: 0.9,
        why: "fixture",
      } satisfies MemoryRecall],
    }

    const provider: LLMProvider = {
      async complete(): Promise<ModelMessage> {
        return { role: "assistant", content: "", toolCalls: [] }
      },
      async *stream(context: RenderedContext): AsyncIterable<StreamEvent> {
        call += 1
        // Renewal carries knowledge over, so the turn-1 entry may still be there; the re-fetch
        // upserts the same key, which is what the `phase` record proves.
        if (sawRenewal && (context.systemKnowledge ?? "").includes(RECALL_TURN)) {
          sawRecallAfterRenewal = true
        }
        if (call <= 10 && !sawRecallAfterRenewal) {
          yield { type: "tool_call", id: `b${call}`, name: "bulk", arguments: {} }
          return
        }
        yield { type: "text_delta", delta: "done" }
      },
    }

    const { runner, sessionLog } = createRunner(
      provider,
      // 400-char outputs against a 200-token window push ρ past the renewal threshold fast; once
      // the first renewal fired, output shrinks so pressure subsides and the re-fetched recall
      // line survives to the next render instead of being wiped by back-to-back renewals.
      // spc_011-C-01: varied phrase-repeated text, not a single repeated character — "z".repeat(400)
      // compresses far below the ~100 effective tokens/turn this test needs under the real-BPE
      // default engine (011-C-01); repeated identical bytes are exactly what BPE merges hardest.
      [tool("bulk", "bulk", { type: "object", properties: {} }, () => (sawRenewal ? "ok" : "The quick fox jumps over lazy dogs and writes long context reports daily. ".repeat(7)))],
      {
        maxTokens: 200,
        maxTurns: 30,
        agentId: "agent-k4",
        memoryScope: scope,
        memoryStore,
        repeatFuse: false,
        preQueryMemory: (ctx: { goal: string; phase?: string }) => {
          phases.push(ctx.phase)
          if (ctx.phase === "renewal") sawRenewal = true
          return [{ scope, query: "relevant facts", top_k: 5, kinds: [] }]
        },
      },
    )

    await collectText(runner.run({ sessionId: "renewal-requery", goal: "long sprint work" }))

    const events = await sessionLog.read("renewal-requery")
    expect(events.some(e => e.event.kind === "context_renewed")).toBe(true)
    const eventKinds = events.map(e => e.event.kind)
    // A compaction the ladder ran on the way to renewal is journaled before it.
    if (eventKinds.includes("compressed")) {
      expect(eventKinds.indexOf("compressed")).toBeLessThan(eventKinds.indexOf("context_renewed"))
    }

    // Turn-1 fetch first, then at least one renewal-boundary re-fetch.
    expect(phases[0]).toBe("initial")
    expect(phases).toContain("renewal")
    // The re-fetched hit reached the new sprint's rendered knowledge.
    expect(sawRecallAfterRenewal).toBe(true)
    // M5 · every prefetch, initial and renewal, is audited with the hits it injected.
    const retrievals = events.filter(e => e.event.kind === "memory_retrieval_result")
    expect(retrievals.length).toBe(phases.length)
  })
})
