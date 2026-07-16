/**
 * T5: every memory query route shares ONE kernel recall lifecycle.
 *
 * The prefetch path used to call `dreamStore.search` directly and hand-assemble a history
 * message, so `memory_recalled` never fired for prefetched hits — recall counts froze and
 * promotions could never trigger. Prefetch now routes each query through the kernel's
 * `query_memory → memory_query_result` effect: the kernel injects each routed hit into
 * history itself (`[MEMORY …]`, one message per hit — same shape as an in-run query) and
 * derives the recall lifecycle statelessly from the routed hits. The store stays a pure
 * query; the runner mirrors `memory_recalled → recordRecall` and surfaces the kernel's
 * edge-triggered `promotion_suggested`.
 */
import { createRunner } from "./runtime/helpers.js"
import { collectText } from "../src/runtime/runner.js"
import type { DreamStore, MemoryRecall, MemoryRecallLifecycle } from "../src/memory/protocols.js"
import type { LLMProvider, Message, RenderedContext, StreamEvent } from "../src/types.js"

const scope = { tenant_id: "agent-lifecycle", namespace: "t5" }

function record(recordId: string, content: string, recallCount: number) {
  return {
    record_id: recordId, scope, name: recordId, kind: "reference", content,
    description: "t5 fixture",
    provenance: { author: "host", trust: "host_verified", evidence_refs: [] },
    created_at: 1, updated_at: 1, recall_count: recallCount, confidence: 0.9,
    links: [], pinned: false,
  }
}

/** Store whose recordRecall really persists — so a later query sees the updated count. */
function trackingStore(initialCount = 0, opts: { withRecordRecall?: boolean; failSearch?: boolean } = {}) {
  const state = { recallCount: initialCount }
  const recallCalls: MemoryRecallLifecycle[][] = []
  const store: DreamStore = {
    upsert: async () => {},
    saveSession: async () => {},
    search: async (): Promise<MemoryRecall[]> => {
      if (opts.failSearch) throw new Error("store offline")
      return [{
        record: record("record-t5", "LONGTERM_FACT_T5", state.recallCount),
        score: 0.9,
        why: "fixture",
      }]
    },
    ...(opts.withRecordRecall !== false ? {
      recordRecall: async (_agentId: string, recalls: MemoryRecallLifecycle[]) => {
        recallCalls.push(recalls)
        for (const r of recalls) state.recallCount = Number(r.recall_count)
      },
    } : {}),
  }
  return { store, recallCalls, state }
}

function textProvider(onContext?: (context: RenderedContext) => void): LLMProvider {
  return {
    async complete(): Promise<Message> {
      return { role: "assistant", content: "unused", toolCalls: [] }
    },
    async *stream(context: RenderedContext): AsyncIterable<StreamEvent> {
      onContext?.(context)
      yield { type: "text_delta", delta: "done" }
    },
  }
}

function runnerOpts(store: DreamStore, extra: Record<string, unknown> = {}) {
  return {
    agentId: "agent-lifecycle",
    memoryScope: scope,
    dreamStore: store,
    preQueryMemory: () => [{ scope, query: "past facts", top_k: 5, kinds: [] }],
    ...extra,
  }
}

describe("T5 memory recall lifecycle is shared by every query route", () => {
  it("initial prefetch routes through the kernel: recordRecall(1) + kernel-shaped injection", async () => {
    const { store, recallCalls } = trackingStore(0)
    let turnsJson = ""
    const provider = textProvider(context => { turnsJson ||= JSON.stringify(context.turns) })
    const { runner } = createRunner(provider, [], runnerOpts(store))

    await collectText(runner.run({ sessionId: "t5-initial", goal: "use the fact" }))

    expect(recallCalls).toHaveLength(1)
    expect(recallCalls[0]).toEqual([
      expect.objectContaining({ record_id: "record-t5", recall_count: 1 }),
    ])
    // Kernel injection shape: one `[MEMORY …]` message per hit — the old hand-assembled
    // combined `[memory …]` message is gone.
    expect(turnsJson).toContain("[MEMORY record_id=record-t5")
    expect(turnsJson).toContain("LONGTERM_FACT_T5")
    expect(turnsJson).not.toContain("[memory record_id=")
  })

  it("two prefetch queries hitting the same record recall and inject it once", async () => {
    const { store, recallCalls } = trackingStore(0)
    let turnsJson = ""
    const provider = textProvider(context => { turnsJson ||= JSON.stringify(context.turns) })
    const { runner } = createRunner(provider, [], runnerOpts(store, {
      preQueryMemory: () => [
        { scope, query: "first angle", top_k: 5, kinds: [] },
        { scope, query: "second angle", top_k: 5, kinds: [] },
      ],
    }))

    await collectText(runner.run({ sessionId: "t5-dedupe", goal: "use the fact" }))

    expect(recallCalls).toHaveLength(1)
    expect(recallCalls[0]).toHaveLength(1)
    expect(turnsJson.split("[MEMORY record_id=record-t5").length - 1).toBe(1)
  })

  it("promotion fires exactly on the threshold crossing, and never re-fires past it", async () => {
    const promotions: Array<{ recordId: string; recallCount: number }> = []
    // before=1 → after=2 crosses threshold 2: exactly one suggestion.
    const crossing = trackingStore(1)
    const { runner } = createRunner(textProvider(), [], runnerOpts(crossing.store, {
      memoryPolicy: { promotionRecallThreshold: 2 },
      onPromotionSuggested: (s: { recordId: string; recallCount: number }) => { promotions.push(s) },
    }))
    await collectText(runner.run({ sessionId: "t5-promote", goal: "use the fact" }))
    expect(promotions).toEqual([{ recordId: "record-t5", recallCount: 2 }])

    // before=2 (already at threshold) → after=3: no repeat suggestion.
    const past = trackingStore(2)
    const { runner: runner2 } = createRunner(textProvider(), [], runnerOpts(past.store, {
      memoryPolicy: { promotionRecallThreshold: 2 },
      onPromotionSuggested: (s: { recordId: string; recallCount: number }) => { promotions.push(s) },
    }))
    await collectText(runner2.run({ sessionId: "t5-past", goal: "use the fact" }))
    expect(promotions).toHaveLength(1)
    expect(past.recallCalls[0]).toEqual([
      expect.objectContaining({ record_id: "record-t5", recall_count: 3 }),
    ])
  })

  it("a failing store search stays errs-open: the run completes without a recall", async () => {
    const { store, recallCalls } = trackingStore(0, { failSearch: true })
    const { runner } = createRunner(textProvider(), [], runnerOpts(store))

    const text = await collectText(runner.run({ sessionId: "t5-fail", goal: "use the fact" }))

    expect(text).toContain("done")
    expect(recallCalls).toHaveLength(0)
  })

  it("a store without recordRecall still runs, and promotion still follows the kernel", async () => {
    const promotions: Array<{ recordId: string; recallCount: number }> = []
    const { store, recallCalls } = trackingStore(1, { withRecordRecall: false })
    const { runner } = createRunner(textProvider(), [], runnerOpts(store, {
      memoryPolicy: { promotionRecallThreshold: 2 },
      onPromotionSuggested: (s: { recordId: string; recallCount: number }) => { promotions.push(s) },
    }))

    const text = await collectText(runner.run({ sessionId: "t5-norecord", goal: "use the fact" }))

    expect(text).toContain("done")
    expect(recallCalls).toHaveLength(0)
    expect(promotions).toEqual([{ recordId: "record-t5", recallCount: 2 }])
  })

  it("host queryMemory() shares the recall lifecycle: prefetch 1 → host query 2", async () => {
    const { store, recallCalls } = trackingStore(0)
    const { runner } = createRunner(textProvider(), [], runnerOpts(store))
    await collectText(runner.run({ sessionId: "t5-host", goal: "use the fact" }))
    expect(recallCalls).toHaveLength(1)
    expect(recallCalls[0][0]).toEqual(expect.objectContaining({ recall_count: 1 }))

    // The prefetch's recordRecall persisted before this query, so the kernel's stateless
    // derivation continues the count instead of restarting it.
    const hits = await runner.queryMemory(
      { scope, query: "past facts", top_k: 5, kinds: [] },
      { sessionId: "t5-host" },
    )
    expect(hits).toHaveLength(1)
    expect(recallCalls).toHaveLength(2)
    expect(recallCalls[1][0]).toEqual(expect.objectContaining({ recall_count: 2 }))
  })
})
