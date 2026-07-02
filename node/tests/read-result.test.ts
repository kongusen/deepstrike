/**
 * O7 — the `read_result` meta-tool: once the kernel evicts (spools) a large tool result from
 * context, it exposes `read_result` in the toolset so the model can re-fetch the full output by
 * `call_id`. The kernel only advertises the capability; the HOST resolves the content (in-memory
 * pending map → on-disk result spool → session-log scan). This mirrors the Layer-1 spool
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
    let callCount = 0
    let readResultOutput = ""

    const provider: LLMProvider = {
      async complete(): Promise<Message> {
        return { role: "assistant", content: "", toolCalls: [] }
      },
      async *stream(_context: RenderedContext, tools: ToolSchema[]): AsyncIterable<StreamEvent> {
        callCount += 1
        seenTools.push(tools)
        if (callCount === 1) {
          // Turn 1: produce the oversized result that the kernel will spool out of context.
          yield { type: "tool_call", id: "big-1", name: "big_out", arguments: {} }
          return
        }
        if (tools.some(t => t.name === "read_result") && !readResultOutput) {
          // Turn 2+: the kernel now advertises `read_result` (a handle left residency) — fetch it.
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
      if (evt.type === "tool_result" && evt.callId === "read-1") {
        readResultOutput = evt.content
      }
    }

    // Sanity: the kernel did actually spool the oversized result out of context.
    const logged = await sessionLog.read("read-result-run")
    expect(logged.find(e => e.event.kind === "large_result_spooled")).toBeDefined()

    // The toolset advertised `read_result` only once eviction happened (progressive disclosure).
    expect(seenTools[0].some(t => t.name === "read_result")).toBe(false)
    expect(seenTools.some(ts => ts.some(t => t.name === "read_result"))).toBe(true)

    // The host resolved the call_id back to the ORIGINAL full content.
    expect(readResultOutput).toContain(`of ${huge.length}`)
    expect(readResultOutput).toContain(huge.slice(0, 4000))
  })
})
