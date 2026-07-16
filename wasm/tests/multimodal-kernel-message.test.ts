// Guards the WASM-specific multimodal fix: `messageToKernelMessage` previously sent only the
// `content` string, dropping every content part — so a reconstructed multimodal turn lost its
// image/audio on the reconstruction→preload path (live ingress uses a different converter). This
// pins that image/audio parts now serialize to the kernel Content::Parts shape.
import { messageToKernelMessage } from "../src/runtime/kernel-step.js"
import { RuntimeRunner, InMemorySessionLog, LocalExecutionPlane } from "../src/runtime/index.js"
import type { ContentPart, LLMProvider, Message, StreamEvent } from "../src/types.js"
import { kernelEvents } from "@deepstrike/wasm-kernel"

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

// Mirrors node/tests/multimodal.test.ts "attachment seeding is idempotent per session". The mock
// kernel can't render accumulated history, so this asserts the SDK side of the contract instead:
// the run_started record (which replay reconstructs from) and the add_history_message live seed
// are emitted once per session for identical attachments, not once per run.
describe("attachment seeding is idempotent per session (runner)", () => {
  const textOnlyProvider: LLMProvider = {
    async complete(): Promise<Message> {
      return { role: "assistant", content: "unused", toolCalls: [] }
    },
    async *stream(): AsyncIterable<StreamEvent> {
      yield { type: "text_delta", delta: "done" }
    },
  }

  it("a same-session retry neither re-records nor re-seeds identical attachments", async () => {
    kernelEvents.length = 0
    const attachments: ContentPart[] = [{ type: "image", data: "iVBORw0KGgo=", mediaType: "image/png" }]
    const sessionLog = new InMemorySessionLog()
    const runner = new RuntimeRunner({
      provider: textOnlyProvider,
      sessionLog,
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 2048,
      maxTurns: 6,
    })

    for await (const _e of runner.run({ sessionId: "retry", goal: "attempt 1", attachments })) { /* drain */ }
    for await (const _e of runner.run({ sessionId: "retry", goal: "attempt 2", attachments })) { /* drain */ }

    const starts = (await sessionLog.read("retry")).filter(e => e.event.kind === "run_started")
    expect(starts).toHaveLength(2)
    expect((starts[0]!.event as { attachments?: ContentPart[] }).attachments).toEqual(attachments)
    expect((starts[1]!.event as { attachments?: ContentPart[] }).attachments).toBeUndefined()

    const seeds = kernelEvents.filter((e: { kind?: string }) => e.kind === "add_history_message")
    expect(seeds).toHaveLength(1)
  })

  it("different attachments in a later same-session run are still seeded", async () => {
    kernelEvents.length = 0
    const sessionLog = new InMemorySessionLog()
    const runner = new RuntimeRunner({
      provider: textOnlyProvider,
      sessionLog,
      executionPlane: new LocalExecutionPlane(),
      maxTokens: 2048,
      maxTurns: 6,
    })
    const imageA: ContentPart[] = [{ type: "image", data: "AAAA", mediaType: "image/png" }]
    const imageB: ContentPart[] = [{ type: "image", data: "BBBB", mediaType: "image/png" }]

    for await (const _e of runner.run({ sessionId: "two", goal: "first", attachments: imageA })) { /* drain */ }
    for await (const _e of runner.run({ sessionId: "two", goal: "second", attachments: imageB })) { /* drain */ }

    const starts = (await sessionLog.read("two")).filter(e => e.event.kind === "run_started")
    expect((starts[1]!.event as { attachments?: ContentPart[] }).attachments).toEqual(imageB)
    const seeds = kernelEvents.filter((e: { kind?: string }) => e.kind === "add_history_message")
    expect(seeds).toHaveLength(2)
  })
})
