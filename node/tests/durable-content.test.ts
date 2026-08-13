import { readFile } from "node:fs/promises"
import { join } from "node:path"
import {
  DurableContentError,
  decodeDurableContent,
  decodeDurableToolResult,
  encodeDurableContent,
  encodeDurableToolResult,
  migrateLegacyContent,
  toolOutputBlocksToDurable,
} from "../src/runtime/durable-content.js"

describe("016-02 durable content ABI", () => {
  it("round-trips the shared multimodal/tool fixture", async () => {
    const fixture = JSON.parse(await readFile(join(process.cwd(), "..", "tests", "fixtures", "durable-content", "v1-tool-result.json"), "utf8"))
    const result = decodeDurableToolResult(fixture)
    expect(encodeDurableToolResult(result)).toEqual(fixture)
  })

  it("migrates legacy text and rejects unknown or nested blocks", () => {
    expect(migrateLegacyContent("hello")).toEqual({ schema_version: 1, blocks: [{ type: "text", text: "hello" }] })
    expect(() => decodeDurableContent({ schema_version: 1, blocks: [{ type: "text", text: "x", extra: true }] })).toThrow(DurableContentError)
    expect(() => decodeDurableContent({ schema_version: 1, blocks: [{ type: "tool_result", call_id: "nested", blocks: [] }] })).toThrow(/nested tool_result/)
    expect(() => decodeDurableContent({ schema_version: 2, blocks: [] })).toThrow(/unsupported durable content schema_version/)
    expect(() => decodeDurableContent({ schema_version: 1, blocks: [{ type: "file", source: { kind: "file_id", id: "f" } }] })).toThrow(/affinity/)
  })

  it("rejects runtime sources that lack durable ownership or affinity facts", () => {
    expect(() => toolOutputBlocksToDurable([{ type: "video", source: { kind: "object", handle: "h" } }])).toThrow(/requires owner/)
    expect(() => toolOutputBlocksToDurable([{ type: "file", source: { kind: "fileId", id: "f" } }])).toThrow(/requires endpoint affinity/)
  })

  it("keeps endpoint affinity and external payload ownership explicit", () => {
    const content = decodeDurableContent({
      schema_version: 1,
      blocks: [{
        type: "file",
        source: { kind: "file_id", id: "file-1", affinity: { provider_id: "openai", endpoint_id: "responses" } },
        media_type: "application/pdf",
      }],
    })
    expect(content.blocks[0]).toEqual(expect.objectContaining({ type: "file" }))
    expect(() => decodeDurableContent({
      schema_version: 1,
      blocks: [{ type: "file", source: { kind: "object", handle: "h1", owner: "unknown" } }],
    })).toThrow(/payload_ref/)
  })

  it("rejects non-boolean is_error values", () => {
    expect(() => decodeDurableToolResult({ schema_version: 1, call_id: "c1", is_error: "false", blocks: [] })).toThrow(/boolean/)
    expect(() => decodeDurableToolResult({ call_id: "c1", output: "old", is_error: 0 })).toThrow(/boolean/)
  })
})
