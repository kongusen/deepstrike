/**
 * Plan task 1.1c · provider replay travels with the kernel item: a rendered turn carries it back,
 * and providers prefer it to their own content-keyed store (which misses once the kernel rewrites
 * the turn's text).
 */
import { kernelMessageToSdk } from "../src/runtime/kernel-step.js"
import { normalizeCanonicalContext } from "../src/providers/content-normalization.js"

describe("kernel-held provider replay", () => {
  it("reaches the canonical message from the rendered turn, not the content key", () => {
    const turn = kernelMessageToSdk({
      role: "assistant",
      content: "[narration collapsed]",
      tool_calls: [{ call_id: "c1", name: "read", arguments: { page: 1 } }],
      item_id: 7,
      replay: { protocol: "openai-chat", reasoning_content: "first read page one" },
    })
    expect(turn.itemId).toBe(7)
    const canonical = normalizeCanonicalContext({ systemText: "", turns: [turn] }, () => undefined)
    expect(canonical.turns[0].providerReplay).toEqual({ protocol: "openai-chat", reasoning_content: "first read page one" })
  })
})
