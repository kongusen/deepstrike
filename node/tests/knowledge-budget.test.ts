/**
 * K2 — knowledge budget. The knowledge partition counts into ρ but the compression pyramid can
 * only squeeze history, so unbounded knowledge growth would silently steal history's working
 * room. The budget (`knowledgeBudgetRatio` × maxTokens) marks the OLDEST unpinned, non-skill
 * entries for eviction at the next compaction boundary and emits a warn-once
 * `knowledge_budget_exceeded` observation. Pinned entries survive even over budget.
 */
import { createRunner, tool } from "./runtime/helpers.js"
import { collectText } from "../src/runtime/runner.js"
import type { LLMProvider, Message, RenderedContext, StreamEvent } from "../src/types.js"

const EVICTABLE = "OLD_UNPINNED_REFERENCE_"
const PINNED = "PINNED_CRITICAL_REFERENCE"

describe("knowledge budget (K2)", () => {
  it("evicts oldest unpinned entries at the boundary, pinned survive", async () => {
    let call = 0
    let finalKnowledge = ""

    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "", toolCalls: [] }
      },
      async *stream(context: RenderedContext): AsyncIterable<StreamEvent> {
        call += 1
        if (call === 1) {
          // Budget = 480 × 0.25 = 120 tokens. Two 60-token unpinned entries + one pinned
          // (~60 tokens) ⇒ ~180 used, over budget by ~60 ⇒ the OLDEST unpinned entry gets
          // marked; the pinned one is exempt regardless of age.
          await runner.pushKnowledge({ role: "system", content: PINNED.padEnd(240, "p"), toolCalls: [] }, 60, { key: "keep", pinned: true })
          await runner.pushKnowledge({ role: "system", content: `${EVICTABLE}1`.padEnd(240, "x"), toolCalls: [] }, 60, { key: "old1" })
          await runner.pushKnowledge({ role: "system", content: `${EVICTABLE}2`.padEnd(240, "y"), toolCalls: [] }, 60, { key: "old2" })
          yield { type: "tool_call", id: `b${call}`, name: "bulk", arguments: {} }
          return
        }
        if (call <= 10) {
          yield { type: "tool_call", id: `b${call}`, name: "bulk", arguments: {} }
          return
        }
        finalKnowledge = context.systemKnowledge ?? ""
        yield { type: "text_delta", delta: "done" }
      },
    }

    const { runner, sessionLog } = createRunner(
      provider,
      [tool("bulk", "bulk", { type: "object", properties: {} }, () => "z".repeat(240))],
      { maxTokens: 480, maxTurns: 30, repeatFuse: false },
    )

    const text = await collectText(runner.run({ sessionId: "knowledge-budget", goal: "exercise the budget" }))
    expect(text).toBe("done")

    const events = await sessionLog.read("knowledge-budget")
    expect(events.some(e => e.event.kind === "compressed")).toBe(true)

    // The pinned entry survives; enough old unpinned entries were evicted to fit the budget
    // (oldest-first ⇒ old1 goes before old2).
    expect(finalKnowledge).toContain(PINNED)
    expect(finalKnowledge).not.toContain(`${EVICTABLE}1`)
  })

  it("knowledgeBudgetRatio: 0 disables the cap", async () => {
    let call = 0
    let finalKnowledge = ""

    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "", toolCalls: [] }
      },
      async *stream(context: RenderedContext): AsyncIterable<StreamEvent> {
        call += 1
        if (call === 1) {
          // 180 tokens — clearly over the default budget (120) so this test would FAIL if the
          // ratio-0 knob didn't reach the kernel.
          await runner.pushKnowledge({ role: "system", content: `${EVICTABLE}1`.padEnd(240, "x"), toolCalls: [] }, 60, { key: "old1" })
          await runner.pushKnowledge({ role: "system", content: `${EVICTABLE}2`.padEnd(240, "y"), toolCalls: [] }, 60, { key: "old2" })
          await runner.pushKnowledge({ role: "system", content: `${EVICTABLE}3`.padEnd(240, "w"), toolCalls: [] }, 60, { key: "old3" })
          yield { type: "tool_call", id: `b${call}`, name: "bulk", arguments: {} }
          return
        }
        if (call <= 10) {
          yield { type: "tool_call", id: `b${call}`, name: "bulk", arguments: {} }
          return
        }
        finalKnowledge = context.systemKnowledge ?? ""
        yield { type: "text_delta", delta: "done" }
      },
    }

    const { runner } = createRunner(
      provider,
      [tool("bulk", "bulk", { type: "object", properties: {} }, () => "z".repeat(240))],
      { maxTokens: 480, maxTurns: 30, repeatFuse: false, knowledgeBudgetRatio: 0 },
    )

    await collectText(runner.run({ sessionId: "knowledge-budget-off", goal: "no cap" }))

    // Same over-budget shape, cap disabled ⇒ everything survives the boundaries.
    expect(finalKnowledge).toContain(`${EVICTABLE}1`)
    expect(finalKnowledge).toContain(`${EVICTABLE}2`)
  })
})
