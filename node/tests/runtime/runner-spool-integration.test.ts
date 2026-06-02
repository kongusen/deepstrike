import * as path from "path"
import { LargeResultSpool } from "../../src/runtime/large-result-spool.js"
import { collectText } from "../../src/runtime/runner.js"
import { createRunner, tool } from "./helpers.js"
import type { LLMProvider, Message, StreamEvent } from "../../src/types.js"

describe("runner Layer-1 spool integration", () => {
  const testSpoolDir = path.join(process.cwd(), ".spool-runner-test")

  it("logs large_result_spooled when kernel spools an oversized tool result", async () => {
    const huge = "x".repeat(60 * 1024)
    const spool = new LargeResultSpool({ spoolDir: testSpoolDir })

    let callCount = 0
    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "", toolCalls: [] }
      },
      async *stream(): AsyncIterable<StreamEvent> {
        callCount += 1
        if (callCount === 1) {
          yield { type: "tool_call", id: "big-1", name: "big_out", arguments: {} }
          return
        }
        yield { type: "text_delta", delta: "done" }
      },
    }

    const { runner, sessionLog } = createRunner(
      provider,
      [tool("big_out", "big", { type: "object", properties: {} }, () => huge)],
      { maxTokens: 128_000, maxTurns: 4, resultSpool: spool },
    )

    await collectText(runner.run({ sessionId: "spool-run", goal: "fetch big output" }))

    const events = await sessionLog.read("spool-run")
    const spooled = events.find(e => e.event.kind === "large_result_spooled")
    expect(spooled).toBeDefined()
    expect((spooled!.event as { original_size: number }).original_size).toBeGreaterThan(50 * 1024)
    expect((spooled!.event as { spool_ref?: string }).spool_ref).toContain(testSpoolDir)

    const ref = (spooled!.event as { spool_ref: string }).spool_ref
    await expect(spool.readSpooledResult(ref)).resolves.toBe(huge)
  })
})
