import { InMemorySessionLog } from "../src/runtime/session-log.js"
import { replayMessages } from "../src/runtime/runner.js"
import { toolOutputBlocksToDurable } from "../src/runtime/durable-content.js"

describe("015-07 durable structured ToolResult replay", () => {
  it("restores a structured result from an additive SessionLog blocks field", async () => {
    const log = new InMemorySessionLog()
    await log.append("s", { kind: "tool_completed", turn: 1, results: [{
      call_id: "call-1", output: "first\n[image]", is_error: false,
      content: { schema_version: 1, blocks: toolOutputBlocksToDurable([
        { type: "text", text: "first" },
        { type: "image", source: { kind: "base64", data: "aW1hZ2U=" }, mediaType: "image/png" },
      ]) },
    }] })

    const messages = replayMessages(await log.read("s"))
    const result = messages[0]?.contentParts?.[0]
    expect(result).toMatchObject({ type: "tool_result", callId: "call-1", output: "first\n[image]" })
    expect(result?.type === "tool_result" ? result.contentParts : undefined).toEqual([
      { type: "text", text: "first" },
      { type: "image", source: { kind: "base64", data: "aW1hZ2U=" }, mediaType: "image/png" },
    ])
  })

  it("continues to replay a legacy text-only result", () => {
    const messages = replayMessages([{ seq: 0, event: { kind: "tool_completed", turn: 1, results: [{ call_id: "legacy", output: "old", is_error: false }] } }])
    expect(messages[0]?.contentParts?.[0]).toMatchObject({ type: "tool_result", callId: "legacy", output: "old" })
  })

  it("fails closed when a recorded durable block is malformed", () => {
    expect(() => replayMessages([{ seq: 0, event: { kind: "tool_completed", turn: 1, results: [{
      call_id: "bad", output: "", is_error: false,
      content: { schema_version: 1, blocks: [{ type: "text", text: "x", unknown: true }] },
    }] } }])).toThrow(/unknown field/)
  })
})
