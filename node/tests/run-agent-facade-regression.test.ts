import { runAgent } from "../src/runtime/facade.js"
import { ReplayProvider } from "../src/runtime/replay-provider.js"
import type { Message } from "../src/types.js"

/**
 * spc_001-05: `Agent` (spc_001-02) is a new, parallel entry point — it must not change the shape
 * or behavior of the existing `runAgent()` facade. This pins down the minimal call before/after
 * the spc_001 cards land, so any accidental edit to `facade.ts`/`AgentRunSpec` shows up here.
 */
describe("spc_001-05: runAgent() facade regression", () => {
  it("runs a minimal goal against a provider and returns its text, unaffected by the new Agent class", async () => {
    const msg: Message = { role: "assistant", content: "done", tokenCount: 4 }
    const provider = new ReplayProvider([msg])

    const result = await runAgent({ provider, goal: "say done" })

    expect(result).toBe("done")
  })

  it("still accepts RunAgentOptions with only the required fields (provider, goal)", async () => {
    const provider = new ReplayProvider([{ role: "assistant", content: "ok", tokenCount: 2 }])
    await expect(runAgent({ provider, goal: "ok" })).resolves.toBe("ok")
  })
})
