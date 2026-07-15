// Guards the WASM-specific multimodal fix: `messageToKernelMessage` previously sent only the
// `content` string, dropping every content part — so a reconstructed multimodal turn lost its
// image/audio on the reconstruction→preload path (live ingress uses a different converter). This
// pins that image/audio parts now serialize to the kernel Content::Parts shape.
import { messageToKernelMessage } from "../src/runtime/kernel-step.js"
import type { Message } from "../src/types.js"

describe("messageToKernelMessage multimodal serialization", () => {
  it("serializes image content parts to the kernel shape, not just the text string", () => {
    const msg: Message = {
      role: "user",
      content: "describe this",
      contentParts: [
        { type: "text", text: "describe this" },
        { type: "image", data: "QUJD", mediaType: "image/png", detail: "low" },
      ],
      toolCalls: [],
    }
    const out = messageToKernelMessage(msg)
    expect(Array.isArray(out.content)).toBe(true)
    const parts = out.content as Array<Record<string, unknown>>
    const img = parts.find(p => p.type === "image")
    expect(img).toBeDefined()
    expect(img!.data).toBe("QUJD")
    expect(img!.media_type).toBe("image/png")
    expect(img!.detail).toBe("low")
    expect(parts.some(p => p.type === "text" && p.text === "describe this")).toBe(true)
  })

  it("serializes audio content parts", () => {
    const msg: Message = {
      role: "user",
      content: "",
      contentParts: [{ type: "audio", data: "AAAA", mediaType: "audio/wav" }],
      toolCalls: [],
    }
    const parts = messageToKernelMessage(msg).content as Array<Record<string, unknown>>
    const audio = parts.find(p => p.type === "audio")
    expect(audio).toEqual({ type: "audio", data: "AAAA", media_type: "audio/wav" })
  })
})
