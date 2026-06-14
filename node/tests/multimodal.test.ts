import { buildContents } from "../src/providers/gemini.js"
import { toOpenAIMessageParams, toAnthropicMessages } from "../src/providers/base.js"
import { getKernel } from "../src/kernel.js"
import type { Message, RenderedContext } from "../src/types.js"

describe("multimodal image input", () => {
  const imageMsg: Message = {
    role: "user",
    content: "",
    contentParts: [
      { type: "text", text: "What is in this image?" },
      { type: "image", data: "iVBORw0KGgo=", mediaType: "image/png" },
    ],
  }

  it("Gemini renders image parts as inlineData (was dropping them)", () => {
    const contents = buildContents([imageMsg])
    const parts = contents[0].parts as any[]
    expect(parts.some(p => p.text === "What is in this image?")).toBe(true)
    const img = parts.find(p => p.inlineData)
    expect(img.inlineData).toEqual({ mimeType: "image/png", data: "iVBORw0KGgo=" })
  })

  it("Gemini renders a URL image as fileData", () => {
    const contents = buildContents([
      { role: "user", content: "", contentParts: [{ type: "image", url: "https://x/y.png", mediaType: "image/png" }] },
    ])
    const parts = contents[0].parts as any[]
    expect(parts.find(p => p.fileData).fileData).toEqual({ mimeType: "image/png", fileUri: "https://x/y.png" })
  })

  it("OpenAI renders image_url; Anthropic renders an image source block", () => {
    const ctx: RenderedContext = { systemText: "", turns: [imageMsg] }
    const openai = toOpenAIMessageParams(ctx)
    const oaContent = openai[0].content as any[]
    expect(oaContent.find(p => p.type === "image_url").image_url.url).toBe("data:image/png;base64,iVBORw0KGgo=")

    const anthropic = toAnthropicMessages([imageMsg]) as any[]
    const aContent = anthropic[0].content as any[]
    expect(aContent.find(p => p.type === "image").source).toEqual({ type: "base64", media_type: "image/png", data: "iVBORw0KGgo=" })
  })

  it("upload: add_history_message lands the image in the rendered context (real kernel)", () => {
    const k = new (getKernel().KernelRuntime)({ maxTokens: 4096 })
    const step = (event: unknown) => k.step(JSON.stringify({ version: 1, event }))
    step({ kind: "add_history_message", message: { role: "user", content: [
      { type: "text", text: "describe" },
      { type: "image", data: "iVBORw0KGgo=", media_type: "image/png" },
    ] } })
    step({ kind: "start_run", task: { goal: "describe the image", criteria: [] } })
    const ctx = k.render() as any
    const hasImage = ctx.turns.some((m: any) => (m.contentParts ?? []).some((p: any) => p.type === "image"))
    expect(hasImage).toBe(true)
  })
})
