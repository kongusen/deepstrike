import * as path from "path"
import * as fs from "node:fs/promises"
import { PayloadStore } from "../../src/runtime/payload-store.js"
import { collectText } from "../../src/runtime/runner.js"
import { createRunner, tool } from "./helpers.js"
import type { LLMProvider, Message, StreamEvent } from "../../src/types.js"

describe("runner external payload integration", () => {
  const storageDir = path.join(process.cwd(), ".payload-runner-test")

  afterAll(async () => {
    await fs.rm(storageDir, { recursive: true, force: true })
  })

  it("persists an oversized tool result without a legacy session event", async () => {
    const huge = "x".repeat(60 * 1024)
    const payloadStore = new PayloadStore({ storageDir })

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
      { maxTokens: 128_000, maxTurns: 4, payloadStore },
    )

    await collectText(runner.run({ sessionId: "payload-run", goal: "fetch big output" }))

    const events = await sessionLog.read("payload-run")
    expect(events.some(entry => entry.event.kind === "tool_completed")).toBe(true)
    expect((await fs.readdir(storageDir)).length).toBeGreaterThan(0)
  })

  it("bounds a multibyte external preview by UTF-8 bytes", async () => {
    const huge = "界".repeat(20 * 1024)
    const payloadStore = new PayloadStore({ storageDir })
    let callCount = 0
    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "", toolCalls: [] }
      },
      async *stream(): AsyncIterable<StreamEvent> {
        callCount += 1
        if (callCount === 1) {
          yield { type: "tool_call", id: "big-cjk", name: "big_out", arguments: {} }
          return
        }
        yield { type: "text_delta", delta: "done" }
      },
    }
    const { runner } = createRunner(
      provider,
      [tool("big_out", "big", { type: "object", properties: {} }, () => huge)],
      { maxTokens: 128_000, maxTurns: 4, payloadStore },
    )

    await expect(collectText(runner.run({
      sessionId: "payload-run-cjk",
      goal: "fetch a multibyte output",
    }))).resolves.toBe("done")
  })
})
