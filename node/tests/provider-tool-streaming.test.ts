import { jest } from "@jest/globals"
import { GeminiProvider } from "../src/providers/gemini.js"
import { OllamaProvider } from "../src/providers/ollama.js"
import { OpenAIChatProvider } from "../src/providers/openai.js"
import { QwenProvider } from "../src/providers/qwen.js"
import { DeepSeekProvider } from "../src/providers/deepseek.js"
import type { RenderedContext } from "../src/types.js"

const context: RenderedContext = { systemText: "", turns: [{ role: "user", content: "hi" }] }

describe("provider streamed tool-call assembly", () => {
  it.each([
    ["OpenAI", () => new OpenAIChatProvider("test-key")],
    ["Qwen", () => new QwenProvider("test-key")],
    ["DeepSeek", () => new DeepSeekProvider("test-key")],
  ])("flushes %s tool calls even when the stream ends without finish_reason=tool_calls", async (_name, makeProvider) => {
    const provider = makeProvider()
    ;(provider as unknown as { client: { chat: { completions: { create(): Promise<AsyncIterable<Record<string, unknown>>> } } } }).client = {
      chat: {
        completions: {
          async create() {
            return {
              async *[Symbol.asyncIterator]() {
                yield { choices: [{ delta: { tool_calls: [{ index: 0, id: "call_1", function: { name: "look", arguments: '{"q":' } }] }, finish_reason: null }] }
                yield { choices: [{ delta: { tool_calls: [{ index: 0, function: { name: "up", arguments: '"x"}' } }] }, finish_reason: "stop" }] }
              },
            }
          },
        },
      },
    }

    const events = []
    for await (const event of provider.stream(context, [])) events.push(event)

    expect(events).toContainEqual({ type: "tool_call", id: "call_1", name: "lookup", arguments: { q: "x" } })
  })

  it("keeps multiple Gemini calls with the same function name distinct", async () => {
    const provider = new GeminiProvider("test-key")
    ;(provider as unknown as { genAI: { getGenerativeModel(): Record<string, unknown> } }).genAI = {
      getGenerativeModel() {
        return {
          async generateContentStream() {
            return {
              stream: {
                async *[Symbol.asyncIterator]() {
                  yield { candidates: [{ content: { parts: [{ functionCall: { name: "lookup", args: { q: "a" } } }] } }] }
                  yield { candidates: [{ content: { parts: [{ functionCall: { name: "lookup", args: { q: "b" } } }] } }] }
                },
              },
              response: Promise.resolve({ usageMetadata: {} }),
            }
          },
        }
      },
    }

    const events = []
    for await (const event of provider.stream(context, [])) events.push(event)

    expect(events).toEqual([
      { type: "tool_call", id: "call_1", name: "lookup", arguments: { q: "a" } },
      { type: "tool_call", id: "call_2", name: "lookup", arguments: { q: "b" } },
    ])
  })

  it("emits each Ollama streamed tool call once with a stable id", async () => {
    const provider = new OllamaProvider()
    const originalFetch = global.fetch
    global.fetch = jest.fn(async () => {
      const encoder = new TextEncoder()
      return new Response(new ReadableStream({
        start(controller) {
          controller.enqueue(encoder.encode(JSON.stringify({ message: { tool_calls: [{ function: { name: "lookup", arguments: { q: "x" } } }] } }) + "\n"))
          controller.enqueue(encoder.encode(JSON.stringify({ message: { tool_calls: [{ function: { name: "lookup", arguments: { q: "x" } } }] } }) + "\n"))
          controller.close()
        },
      }), { status: 200 })
    }) as typeof fetch

    try {
      const events = []
      for await (const event of provider.stream(context, [])) events.push(event)
      expect(events).toEqual([{ type: "tool_call", id: "call_1", name: "lookup", arguments: { q: "x" } }])
    } finally {
      global.fetch = originalFetch
    }
  })
})
