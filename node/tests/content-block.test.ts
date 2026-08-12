/**
 * spc_011-B-05: Node `ContentBlock` — additive alongside the existing `ContentPart` union (same
 * precedent as the Rust side: new type, old type untouched, no "V2" in the name per explicit
 * instruction). `ToolResultPart.output: string` stays as-is — 011-B-04 (Rust) found the analogous
 * rename disruptive (54 call sites) and the user chose docs-only over a forced merge; the Node
 * side has ~24 call sites across core runtime (runner.ts/kernel-step.ts/execution-plane.ts, not
 * just providers), the same shape of problem, so `ContentBlockToolResult.content: ContentBlock[]`
 * is a new, separately-named variant that coexists rather than a rename of `ToolResultPart`.
 */
import type { ContentBlock, MediaSource } from "../src/types.js"

describe("ContentBlock (spc_011-B-05)", () => {
  it("MediaSource constructs all four variants", () => {
    const url: MediaSource = { kind: "url", url: "https://example.com/a.png" }
    const base64: MediaSource = { kind: "base64", data: "AAAA" }
    const fileId: MediaSource = { kind: "fileId", id: "file_123" }
    const object: MediaSource = { kind: "object", handle: "handle_abc" }
    expect([url, base64, fileId, object].map(s => s.kind)).toEqual(["url", "base64", "fileId", "object"])
  })

  it("ContentBlock constructs all six variants, including the new Video/File kinds", () => {
    const blocks: ContentBlock[] = [
      { type: "text", text: "hi" },
      { type: "image", source: { kind: "url", url: "u" }, mediaType: "image/png" },
      { type: "audio", source: { kind: "base64", data: "AAAA" }, mediaType: "audio/wav" },
      { type: "video", source: { kind: "url", url: "u" }, mediaType: "video/mp4" },
      { type: "file", source: { kind: "fileId", id: "file_1" }, filename: "a.pdf", mediaType: "application/pdf" },
      { type: "tool_result", callId: "call_1", content: [{ type: "text", text: "ok" }], isError: false },
    ]
    expect(blocks.map(b => b.type)).toEqual(["text", "image", "audio", "video", "file", "tool_result"])
  })

  it("Image provider_options carries the OpenAI detail key without a canonical `detail` field", () => {
    const image: ContentBlock = {
      type: "image",
      source: { kind: "base64", data: "AAAA" },
      mediaType: "image/png",
      providerOptions: { openai_detail: "high" },
    }
    expect(image).not.toHaveProperty("detail")
    if (image.type === "image") expect(image.providerOptions?.openai_detail).toBe("high")
  })
})
