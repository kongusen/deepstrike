/**
 * O7 — the `read_result` meta-tool: once the kernel externalizes a large tool result from
 * context, the canonical kernel exposes `read_result` so the model can re-fetch the
 * full output by `call_id`. The kernel advertises the capability and lowers the call to a
 * `LoadPayload` effect; the host resolves the opaque locator from the payload store.
 */
import * as fs from "fs/promises"
import * as os from "os"
import * as path from "path"
import { PayloadStore } from "../src/runtime/payload-store.js"
import { createRunner, tool } from "./runtime/helpers.js"
import type { LLMProvider, Message, RenderedContext, StreamEvent, ToolSchema } from "../src/types.js"

describe("read_result meta-tool", () => {
  let storageDir: string

  beforeEach(async () => {
    storageDir = await fs.mkdtemp(path.join(os.tmpdir(), "ds-read-result-"))
  })

  afterEach(async () => {
    await fs.rm(storageDir, { recursive: true, force: true })
  })

  it("re-fetches an external payload by call_id", async () => {
    const huge = "y".repeat(100 * 1024)
    const payloadStore = new PayloadStore({ storageDir })

    const seenTools: ToolSchema[][] = []
    const seenContexts: RenderedContext[] = []
    let callCount = 0

    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "", toolCalls: [] }
      },
      async *stream(_context: RenderedContext, tools: ToolSchema[]): AsyncIterable<StreamEvent> {
        callCount += 1
        seenTools.push(tools)
        seenContexts.push(_context)
        if (callCount === 1) {
          // Turn 1: produce the oversized result that the host externalizes before submission.
          yield { type: "tool_call", id: "big-1", name: "big_out", arguments: {} }
          return
        }
        if (callCount === 2 && tools.some(t => t.name === "read_result")) {
          // Turn 2+: a handle has left residency, so fetch it through the canonical meta-tool.
          yield { type: "tool_call", id: "read-1", name: "read_result", arguments: { call_id: "big-1" } }
          return
        }
        yield { type: "text_delta", delta: "done" }
      },
    }

    const { runner, sessionLog } = createRunner(
      provider,
      [tool("big_out", "big", { type: "object", properties: {} }, () => huge)],
      { maxTokens: 128_000, maxTurns: 8, payloadStore },
    )

    const events: StreamEvent[] = []
    for await (const evt of runner.run({ sessionId: "read-result-run", goal: "fetch big output" })) {
      events.push(evt)
    }

    expect(await sessionLog.read("read-result-run")).not.toHaveLength(0)

    // The canonical kernel exposes the syscall only after an external handle is reachable.
    expect(seenTools[0].some(t => t.name === "read_result")).toBe(false)
    expect(seenTools.slice(1).some(ts => ts.some(t => t.name === "read_result"))).toBe(true)

    // The host resolved the opaque locator and core restored the verified body for the next turn.
    expect(JSON.stringify(seenContexts[2])).toContain(huge.slice(0, 4000))
    expect(events.some(event => event.type === "error")).toBe(false)
  })
})
