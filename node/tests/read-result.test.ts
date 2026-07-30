/**
 * O7 — the `read_result` meta-tool: once the kernel evicts (spools) a large tool result from
 * context, the canonical kernel exposes `read_result` so the model can re-fetch the
 * full output by `call_id`. The kernel advertises the capability and lowers the call to a
 * `LoadPayload` effect; the HOST resolves the opaque locator from the payload store. This mirrors the Layer-1 spool
 * integration test (`runner-spool-integration.test.ts`) but drives the meta-tool call itself.
 */
import * as fs from "fs/promises"
import * as os from "os"
import * as path from "path"
import { LargeResultSpool } from "../src/runtime/large-result-spool.js"
import { createRunner, tool } from "./runtime/helpers.js"
import type { LLMProvider, Message, RenderedContext, StreamEvent, ToolSchema } from "../src/types.js"

describe("read_result meta-tool", () => {
  let testSpoolDir: string

  beforeEach(async () => {
    testSpoolDir = await fs.mkdtemp(path.join(os.tmpdir(), "ds-read-result-"))
  })

  afterEach(async () => {
    await fs.rm(testSpoolDir, { recursive: true, force: true })
  })

  it("re-fetches the full output of a spooled tool result by call_id", async () => {
    const huge = "y".repeat(100 * 1024)
    const spool = new LargeResultSpool({ spoolDir: testSpoolDir })

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
          // Turn 1: produce the oversized result that the kernel will spool out of context.
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
      { maxTokens: 128_000, maxTurns: 8, resultSpool: spool },
    )

    const events: StreamEvent[] = []
    for await (const evt of runner.run({ sessionId: "read-result-run", goal: "fetch big output" })) {
      events.push(evt)
    }

    // Sanity: the kernel did actually spool the oversized result out of context.
    const logged = await sessionLog.read("read-result-run")
    expect(logged.find(e => e.event.kind === "large_result_spooled")).toBeDefined()

    // The canonical kernel exposes the syscall only after an external handle is reachable.
    expect(seenTools[0].some(t => t.name === "read_result")).toBe(false)
    expect(seenTools.slice(1).some(ts => ts.some(t => t.name === "read_result"))).toBe(true)

    // The host resolved the opaque locator and core restored the verified body for the next turn.
    expect(JSON.stringify(seenContexts[2])).toContain(huge.slice(0, 4000))
    expect(events.some(event => event.type === "error")).toBe(false)
  })
})
