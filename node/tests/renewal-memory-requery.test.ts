/**
 * K4 — renewal-boundary memory re-query. A sprint renewal (ρ > 0.98) rebuilds history wholesale,
 * dropping earlier memory hits with it. The runner now re-fires the `preQueryMemory` prefetch on
 * the live `renewed` observation (phase: "renewal") so the new sprint starts with a fresh recall
 * pass — the symmetric counterpart of the turn-1 fetch (phase: "initial"). Hits land in `history`
 * as ordinary turns (single-use retrieval content), never in `knowledge`.
 */
import { createRunner, tool } from "./runtime/helpers.js"
import { collectText } from "../src/runtime/runner.js"
import type { DreamStore, MemoryRecall } from "../src/memory/protocols.js"
import type { LLMProvider, Message, RenderedContext, StreamEvent } from "../src/types.js"

const RECALL = "LONGTERM_FACT_FOR_SPRINT"
const scope = { tenant_id: "agent-k4", namespace: "renewal" }

describe("renewal-boundary memory re-query (K4)", () => {
  it("re-fires preQueryMemory with phase 'renewal' and lands hits in the new sprint's history", async () => {
    const phases: Array<string | undefined> = []
    let sawRecallAfterRenewal = false
    let sawRenewal = false
    let call = 0

    const dreamStore: DreamStore = {
      upsert: async () => {},
      saveSession: async () => {},
      search: async () => [{
        record: {
          record_id: "record-renewal", scope, name: "renewal-fact", kind: "reference", content: RECALL,
          description: "renewal fixture",
          provenance: { author: "host", trust: "host_verified", evidence_refs: [] },
          created_at: 1, updated_at: 1, recall_count: 0, confidence: 0.9, links: [], pinned: false,
        },
        score: 0.9,
        why: "fixture",
      } satisfies MemoryRecall],
    }

    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "", toolCalls: [] }
      },
      async *stream(context: RenderedContext): AsyncIterable<StreamEvent> {
        call += 1
        if (sawRenewal && JSON.stringify(context.turns).includes(RECALL)) {
          sawRecallAfterRenewal = true
        }
        expect(context.systemKnowledge ?? "").not.toContain(RECALL)
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
      [tool("bulk", "bulk", { type: "object", properties: {} }, () => (sawRenewal ? "ok" : "z".repeat(400)))],
      {
        maxTokens: 200,
        maxTurns: 30,
        agentId: "agent-k4",
        memoryScope: scope,
        dreamStore,
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

    // Turn-1 fetch first, then at least one renewal-boundary re-fetch.
    expect(phases[0]).toBe("initial")
    expect(phases).toContain("renewal")
    // The re-fetched hits reached the new sprint's rendered turns.
    expect(sawRecallAfterRenewal).toBe(true)
  })
})
