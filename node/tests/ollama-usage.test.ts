/**
 * spc_011-C-07: `OllamaProvider.stream()`'s first-ever usage extraction. Ollama's final chunk
 * (`done: true`) carries `prompt_eval_count`/`eval_count` — before this card nothing read them.
 */
import { jest } from "@jest/globals"
import { OllamaProvider } from "../src/providers/ollama.js"
import type { RenderedContext } from "../src/types.js"

const context: RenderedContext = { systemText: "", turns: [{ role: "user", content: "hi" }] }

describe("OllamaProvider usage extraction (spc_011-C-07)", () => {
  it("yields a usage event normalized via UsageNormalizer from the done:true chunk", async () => {
    const provider = new OllamaProvider("llama3")
    const originalFetch = global.fetch
    global.fetch = jest.fn(async () => {
      const encoder = new TextEncoder()
      return new Response(new ReadableStream({
        start(controller) {
          controller.enqueue(encoder.encode(JSON.stringify({ message: { content: "hi there" } }) + "\n"))
          controller.enqueue(encoder.encode(JSON.stringify({
            done: true,
            prompt_eval_count: 42,
            eval_count: 8,
          }) + "\n"))
          controller.close()
        },
      }), { status: 200 })
    }) as typeof fetch

    try {
      const events: unknown[] = []
      for await (const event of provider.stream(context, [])) events.push(event)
      expect(events).toContainEqual({
        type: "usage",
        totalTokens: 50,
        inputTokens: 42,
        outputTokens: 8,
        providerUsage: { inputTokens: 42, outputTokens: 8 },
      })
    } finally {
      global.fetch = originalFetch
    }
  })

  it("emits no usage event when the stream never sends a done:true chunk", async () => {
    const provider = new OllamaProvider("llama3")
    const originalFetch = global.fetch
    global.fetch = jest.fn(async () => {
      const encoder = new TextEncoder()
      return new Response(new ReadableStream({
        start(controller) {
          controller.enqueue(encoder.encode(JSON.stringify({ message: { content: "hi" } }) + "\n"))
          controller.close()
        },
      }), { status: 200 })
    }) as typeof fetch

    try {
      const events: unknown[] = []
      for await (const event of provider.stream(context, [])) events.push(event)
      expect(events.some(e => (e as { type?: string }).type === "usage")).toBe(false)
    } finally {
      global.fetch = originalFetch
    }
  })
})
