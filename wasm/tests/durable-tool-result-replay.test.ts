import { InMemorySessionLog } from "../src/runtime/session-log.js"
import { replayMessages } from "../src/runtime/runner.js"
import { toolOutputBlocksToDurable } from "../src/runtime/durable-content.js"

describe("015-07 durable structured ToolResult replay", () => {
  it("restores additive durable blocks and accepts legacy text", async () => {
    const log = new InMemorySessionLog()
    await log.append("s", { kind: "tool_completed", turn: 1, results: [{
      call_id: "call-1", output: "first\n[image]", is_error: false,
      content: { schema_version: 1, blocks: toolOutputBlocksToDurable([
        { type: "text", text: "first" },
        { type: "image", source: { kind: "base64", data: "aW1hZ2U=" }, mediaType: "image/png" },
      ]) },
    }] })
    const messages = await replayMessages(await log.read("s"))
    expect(messages[0]?.contentParts?.[0]).toMatchObject({ type: "tool_result", callId: "call-1", contentParts: [
      { type: "text", text: "first" },
      { type: "image", source: { kind: "base64", data: "aW1hZ2U=" }, mediaType: "image/png" },
    ] })
    expect((await replayMessages([{ seq: 0, event: { kind: "tool_completed", turn: 2, results: [{ call_id: "old", output: "legacy", is_error: false }] } }]))[0]?.contentParts?.[0]).toMatchObject({ type: "tool_result", output: "legacy" })
  })

  it("fails closed when a recorded durable block is malformed", async () => {
    await expect(replayMessages([{ seq: 0, event: { kind: "tool_completed", turn: 1, results: [{
      call_id: "bad", output: "", is_error: false,
      content: { schema_version: 1, blocks: [{ type: "text", text: "x", unknown: true }] },
    }] } }])).rejects.toThrow(/unknown field/)
  })
})
