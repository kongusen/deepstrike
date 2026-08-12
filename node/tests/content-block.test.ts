/** SPC-013 A-02: legal tool output blocks exclude nested ToolResult values. */
import type { ContentBlock, MediaSource } from "../src/types.js"

describe("ContentBlock (spc_011-B-05)", () => {
  it("MediaSource constructs all four variants", () => {
    const url: MediaSource = { kind: "url", url: "https://example.com/a.png" }
    const base64: MediaSource = { kind: "base64", data: "AAAA" }
    const fileId: MediaSource = { kind: "fileId", id: "file_123" }
    const object: MediaSource = { kind: "object", handle: "handle_abc" }
    expect([url, base64, fileId, object].map(s => s.kind)).toEqual(["url", "base64", "fileId", "object"])
  })

  it("ContentBlock constructs the five legal output variants", () => {
    const blocks: ContentBlock[] = [
      { type: "text", text: "hi" },
      { type: "image", source: { kind: "url", url: "u" }, mediaType: "image/png" },
      { type: "audio", source: { kind: "base64", data: "AAAA" }, mediaType: "audio/wav" },
      { type: "video", source: { kind: "url", url: "u" }, mediaType: "video/mp4" },
      { type: "file", source: { kind: "fileId", id: "file_1" }, filename: "a.pdf", mediaType: "application/pdf" },
    ]
    expect(blocks.map(b => b.type)).toEqual(["text", "image", "audio", "video", "file"])
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
