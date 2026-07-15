/**
 * K1 — keyed knowledge entries. Knowledge renders into the cached system[1] block, so identity
 * mutations are boundary-deferred: a same-key `pushKnowledge` stages an upsert and
 * `removeKnowledge` stages a drop, both applied only when a compaction rewrites the prompt-cache
 * prefix anyway. Mid-generation the ORIGINAL bytes keep rendering (cache stability); after the
 * boundary the staged state lands. Fresh appends are immediate (they only extend the prefix).
 */
import { createRunner, tool } from "./runtime/helpers.js"
import { collectText } from "../src/runtime/runner.js"
import type { LLMProvider, Message, RenderedContext, StreamEvent } from "../src/types.js"

const V1 = "KEYED_REF_CONTENT_V1"
const V2 = "KEYED_REF_CONTENT_V2"
const TMP = "TEMPORARY_NOTE_TO_DROP"

describe("keyed knowledge entries (K1)", () => {
  it("same-key push upserts and removeKnowledge drops — both at the compaction boundary", async () => {
    let call = 0
    let midRunKnowledge = ""
    let finalKnowledge = ""

    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "", toolCalls: [] }
      },
      async *stream(context: RenderedContext): AsyncIterable<StreamEvent> {
        call += 1
        if (call === 1) {
          // Stage the lifecycle: keyed append (immediate), same-key upsert (deferred),
          // a second entry that will be marked for removal.
          await runner.pushKnowledge({ role: "system", content: V1, toolCalls: [] }, undefined, { key: "ref" })
          await runner.pushKnowledge({ role: "system", content: V2, toolCalls: [] }, undefined, { key: "ref" })
          await runner.pushKnowledge({ role: "system", content: TMP, toolCalls: [] }, undefined, { key: "tmp" })
          await runner.removeKnowledge("tmp")
          yield { type: "tool_call", id: `b${call}`, name: "bulk", arguments: {} }
          return
        }
        if (call === 2) {
          // Mid-generation: original bytes still render (no system[1] rewrite before a boundary).
          midRunKnowledge = context.systemKnowledge ?? ""
          yield { type: "tool_call", id: `b${call}`, name: "bulk", arguments: {} }
          return
        }
        if (call <= 11) {
          // Filler turns to force the compression pyramid past a boundary.
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
      {
        maxTokens: 480,
        maxTurns: 30,
        // The script repeats an identical `bulk()` call to build pressure — incidental to the
        // repeat fuse's intent, so disabled (same as mm-paging-integration.test.ts).
        repeatFuse: false,
      },
    )

    const text = await collectText(runner.run({ sessionId: "keyed-entries", goal: "exercise keyed knowledge" }))
    expect(text).toBe("done")

    // Pre-boundary: one entry rendering V1 (upsert staged, not applied); TMP still visible.
    expect(midRunKnowledge).toContain(V1)
    expect(midRunKnowledge).not.toContain(V2)
    expect(midRunKnowledge).toContain(TMP)

    // A compaction boundary definitely happened.
    const events = await sessionLog.read("keyed-entries")
    expect(events.some(e => e.event.kind === "compressed")).toBe(true)

    // Post-boundary: upsert applied (V2, exactly one copy), removal applied (TMP gone).
    expect(finalKnowledge).toContain(V2)
    expect(finalKnowledge).not.toContain(V1)
    expect(finalKnowledge).not.toContain(TMP)
  })
})
