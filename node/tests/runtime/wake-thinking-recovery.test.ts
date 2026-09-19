import { mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { FileSessionLog } from "../../src/runtime/session-log.js"

describe("RuntimeRunner thinking wake recovery", () => {
  it("rejects the removed top-level provider_replay field", async () => {
    const dir = await mkdtemp(join(tmpdir(), "ds-thinking-wake-"))
    try {
      const sessionId = "thinking-wake"
      const sessionLog = new FileSessionLog(dir)

      await sessionLog.append(sessionId, {
        kind: "run_started",
        run_id: "r1",
        goal: "use ping",
        criteria: [],
      })
      await sessionLog.append(sessionId, {
        kind: "llm_completed",
        turn: 0,
        content: "checking",
        tool_calls: [{ id: "call_ping", name: "ping", arguments: "{}" }],
        provider_replay: {
          native_blocks: [
            { type: "thinking", thinking: "plan", signature: "sig" },
            { type: "text", text: "checking" },
            { type: "tool_use", id: "call_ping", name: "ping", input: {} },
          ],
        },
      } as never)
      await expect(sessionLog.read(sessionId)).rejects.toThrow("removed provider_replay field")
    } finally {
      await rm(dir, { recursive: true, force: true })
    }
  })
})
