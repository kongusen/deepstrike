import { readFileSync } from "node:fs"
import { join } from "node:path"
import { DurableContentError, decodeDurableContent, decodeDurableToolResult, encodeDurableToolResult, toolOutputBlocksToDurable } from "../src/runtime/durable-content.js"

describe("canonical durable content ABI", () => {
  it("round-trips the shared fixture", () => {
    const fixture = JSON.parse(readFileSync(join(process.cwd(), "../tests/fixtures/durable-content/canonical-tool-result.json"), "utf8"))
    expect(encodeDurableToolResult(decodeDurableToolResult(fixture))).toEqual(fixture)
  })
  it("rejects versioned, legacy, unknown, or nested shapes", () => {
    expect(() => decodeDurableContent({ schema_version: 1, blocks: [] })).toThrow(/unknown field schema_version/)
    expect(() => decodeDurableToolResult({ call_id: "c1", output: "old" })).toThrow(/unknown field output/)
    expect(() => decodeDurableContent({ blocks: [{ type: "text", text: "x", extra: true }] })).toThrow(DurableContentError)
    expect(() => decodeDurableContent({ blocks: [{ type: "tool_result" }] })).toThrow(/nested/)
    expect(() => decodeDurableContent({ blocks: [{ type: "file", source: { kind: "file_id", id: "f" } }] })).toThrow(/affinity/)
  })

  it("rejects runtime sources that lack durable ownership or affinity facts", () => {
    expect(() => toolOutputBlocksToDurable([{ type: "video", source: { kind: "object", handle: "h" } }])).toThrow(/requires owner/)
    expect(() => toolOutputBlocksToDurable([{ type: "file", source: { kind: "fileId", id: "f" } }])).toThrow(/requires endpoint affinity/)
  })

  it("rejects non-boolean is_error values", () => {
    expect(() => decodeDurableToolResult({ call_id: "c1", is_error: "false", blocks: [] })).toThrow(/boolean/)
  })
})
