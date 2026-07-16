import { buildContents } from "../src/providers/gemini.js"
import { toOpenAIMessageParams, toAnthropicMessages, UnsupportedModalityError } from "../src/providers/base.js"
import { getKernel } from "../src/kernel.js"
import type { ContentPart, LLMProvider, Message, RenderedContext, StreamEvent, ToolSchema } from "../src/types.js"
import { stepKernelV2 } from "./helpers/kernel-v2.js"
import { createRunner } from "./runtime/helpers.js"

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
    const step = (event: Record<string, unknown>) => stepKernelV2(k, event)
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

describe("attachment seeding is idempotent per session (runner)", () => {
  class CapturingProvider implements LLMProvider {
    readonly calls: RenderedContext[] = []
    async complete(_context: RenderedContext, _tools: ToolSchema[]): Promise<Message> {
      return { role: "assistant", content: "unused", toolCalls: [] }
    }
    async *stream(context: RenderedContext): AsyncIterable<StreamEvent> {
      this.calls.push(context)
      yield { type: "text_delta", delta: "done" }
    }
  }

  const image = (data: string): ContentPart => ({ type: "image", data, mediaType: "image/png" })

  function countImageParts(ctx: RenderedContext): number {
    return ctx.turns.reduce(
      (count, message) => count + (message.contentParts ?? []).filter(p => p.type === "image").length,
      0,
    )
  }

  it("a same-session retry does not double the image (attempt-loop continueSession shape)", async () => {
    const attachments = [image("iVBORw0KGgo=")]
    const provider = new CapturingProvider()
    const { runner, sessionLog } = createRunner(provider)

    for await (const _ of runner.run({ sessionId: "retry", goal: "attempt 1", attachments })) { /* drain */ }
    for await (const _ of runner.run({ sessionId: "retry", goal: "attempt 2", attachments })) { /* drain */ }

    // Only the first run_started records the attachments — replay reconstructs from it, so a
    // second record (or live seed) would double the image in history.
    const starts = (await sessionLog.read("retry")).filter(e => e.event.kind === "run_started")
    expect(starts).toHaveLength(2)
    expect((starts[0]!.event as { attachments?: ContentPart[] }).attachments).toEqual(attachments)
    expect((starts[1]!.event as { attachments?: ContentPart[] }).attachments).toBeUndefined()

    expect(countImageParts(provider.calls.at(-1)!)).toBe(1)
  })

  it("different attachments in a later same-session run are still seeded", async () => {
    const provider = new CapturingProvider()
    const { runner, sessionLog } = createRunner(provider)

    for await (const _ of runner.run({ sessionId: "two-images", goal: "first", attachments: [image("AAAA")] })) { /* drain */ }
    for await (const _ of runner.run({ sessionId: "two-images", goal: "second", attachments: [image("BBBB")] })) { /* drain */ }

    const starts = (await sessionLog.read("two-images")).filter(e => e.event.kind === "run_started")
    expect((starts[1]!.event as { attachments?: ContentPart[] }).attachments).toEqual([image("BBBB")])

    // Run 2 renders BOTH: image A replayed from run 1's history plus the newly seeded image B.
    expect(countImageParts(provider.calls.at(-1)!)).toBe(2)
  })
})

describe("multimodal audio map-or-reject", () => {
  const audioMsg: Message = {
    role: "user",
    content: "",
    contentParts: [
      { type: "text", text: "transcribe" },
      { type: "audio", data: "AAAA", mediaType: "audio/wav" },
    ],
  }

  it("OpenAI maps audio to input_audio", () => {
    const ctx: RenderedContext = { systemText: "", turns: [audioMsg] }
    const openai = toOpenAIMessageParams(ctx)
    const oaContent = openai[0].content as any[]
    expect(oaContent.find(p => p.type === "input_audio").input_audio).toEqual({
      data: "AAAA",
      format: "wav",
    })
  })

  it("Anthropic rejects audio (no silent placeholder)", () => {
    expect(() => toAnthropicMessages([audioMsg])).toThrow(UnsupportedModalityError)
    expect(() => toAnthropicMessages([audioMsg])).toThrow(/UnsupportedModality: audio/)
  })

  it("Gemini maps audio to inlineData (was silently dropping)", () => {
    const contents = buildContents([audioMsg])
    const parts = contents[0].parts as any[]
    expect(parts.some(p => p.text === "transcribe")).toBe(true)
    const audio = parts.find(p => p.inlineData?.mimeType?.startsWith("audio/"))
    expect(audio.inlineData).toEqual({ mimeType: "audio/wav", data: "AAAA" })
  })
})
