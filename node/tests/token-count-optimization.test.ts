import { AnthropicProvider } from "../src/providers/anthropic.js"
import { OpenAIChatProvider } from "../src/providers/openai.js"
import { GeminiProvider } from "../src/providers/gemini.js"
import { OpenAIResponsesProvider } from "../src/providers/openai-responses.js"
import type { RenderedContext } from "../src/types.js"

const mockContext: RenderedContext = {
  systemText: "system-rules",
  systemStable: "system-rules",
  systemVolatile: "",
  turns: [{ role: "user", content: "test query" }],
}

describe("Token Count Optimization", () => {
  describe("AnthropicProvider", () => {
    it("keeps usage evidence off the ProviderMessage returned by complete()", async () => {
      const provider = new AnthropicProvider({ apiKey: "test-key" })
      ;(provider as any).client = {
        messages: {
          create: async () => ({
            content: [{ type: "text", text: "hello" }],
            usage: { input_tokens: 100, output_tokens: 20 },
          }),
        },
      }
      const message = await provider.complete(mockContext, [])
      expect((message as { tokenCount?: number }).tokenCount).toBeUndefined()
    })

    it("yields detailed usage events in stream()", async () => {
      const provider = new AnthropicProvider({ apiKey: "test-key" })
      ;(provider as any).client = {
        messages: {
          stream: () => ({
            async *[Symbol.asyncIterator]() {
              yield {
                type: "message_start",
                message: { usage: { input_tokens: 100, output_tokens: 20 } },
              }
            },
          }),
        },
      }
      const events = []
      for await (const event of provider.stream(mockContext, [])) {
        events.push(event)
      }
      expect(events).toContainEqual({
        type: "usage",
        totalTokens: 120,
        inputTokens: 100,
        outputTokens: 20,
        cacheReadInputTokens: 0,
        cacheCreationInputTokens: 0,
        cacheTelemetryStatus: "unavailable",
        providerUsage: {
          inputTokens: 100,
          outputTokens: 20,
          cacheTelemetryStatus: "unavailable",
        },
      })
    })

    it("reports full prompt size (input + cache) and surfaces the cache breakdown", async () => {
      const provider = new AnthropicProvider({ apiKey: "test-key" })
      ;(provider as any).client = {
        messages: {
          stream: () => ({
            async *[Symbol.asyncIterator]() {
              // message_start pins input + cache counts; message_delta carries
              // the final output count and (per the API) omits input/cache.
              yield {
                type: "message_start",
                message: {
                  usage: {
                    input_tokens: 200,
                    cache_read_input_tokens: 5000,
                    cache_creation_input_tokens: 300,
                    output_tokens: 1,
                  },
                },
              }
              yield { type: "message_delta", usage: { output_tokens: 40 } }
            },
          }),
        },
      }
      const events: any[] = []
      for await (const event of provider.stream(mockContext, [])) events.push(event)
      const usage = events.filter(e => e.type === "usage").at(-1)
      // inputTokens is the FULL prompt: 200 uncached + 5000 read + 300 write.
      // The provider reports aggregate cache usage only; no slot attribution is fabricated.
      expect(usage).toMatchObject({
        type: "usage",
        totalTokens: 5540, // 5500 prompt + 40 output
        inputTokens: 5500,
        outputTokens: 40, // max() keeps 40 despite the message_delta omitting input/cache
        cacheReadInputTokens: 5000,
        cacheCreationInputTokens: 300,
      })
    })
  })

  describe("OpenAIProvider", () => {
    it("keeps usage evidence off the ProviderMessage returned by complete()", async () => {
      const provider = new OpenAIChatProvider({ apiKey: "test-key" })
      ;(provider as any).client = {
        chat: {
          completions: {
            create: async () => ({
              choices: [{ message: { content: "hello" } }],
              usage: { prompt_tokens: 50, completion_tokens: 15, total_tokens: 65 },
            }),
          },
        },
      }
      const message = await provider.complete(mockContext, [])
      expect((message as { tokenCount?: number }).tokenCount).toBeUndefined()
    })

    it("yields detailed usage events in stream()", async () => {
      const provider = new OpenAIChatProvider({ apiKey: "test-key" })
      ;(provider as any).client = {
        chat: {
          completions: {
            create: async () => ({
              async *[Symbol.asyncIterator]() {
                yield {
                  usage: { prompt_tokens: 50, completion_tokens: 15, total_tokens: 65 },
                }
              },
            }),
          },
        },
      }
      const events = []
      for await (const event of provider.stream(mockContext, [])) {
        events.push(event)
      }
      expect(events).toContainEqual({
        type: "usage",
        totalTokens: 65,
        inputTokens: 50,
        outputTokens: 15,
        cacheTelemetryStatus: "unavailable",
        providerUsage: {
          inputTokens: 50,
          outputTokens: 15,
          cacheTelemetryStatus: "unavailable",
        },
      })
    })
  })

  describe("GeminiProvider", () => {
    it("keeps usage evidence off the ProviderMessage returned by complete()", async () => {
      const provider = new GeminiProvider("test-key")
      ;(provider as any).genAI = {
        getGenerativeModel: () => ({
          generateContent: async () => ({
            response: {
              candidates: [{ content: { parts: [{ text: "hello" }] } }],
              usageMetadata: { promptTokenCount: 80, candidatesTokenCount: 25, totalTokenCount: 105 },
            },
          }),
        }),
      }
      const message = await provider.complete(mockContext, [])
      expect((message as { tokenCount?: number }).tokenCount).toBeUndefined()
    })

    it("yields detailed usage events in stream()", async () => {
      const provider = new GeminiProvider("test-key")
      ;(provider as any).genAI = {
        getGenerativeModel: () => ({
          generateContentStream: async () => ({
            stream: {
              async *[Symbol.asyncIterator]() {
                yield { candidates: [{ content: { parts: [{ text: "hello" }] } }] }
              },
            },
            response: Promise.resolve({
              usageMetadata: { promptTokenCount: 80, candidatesTokenCount: 25, totalTokenCount: 105 },
            }),
          }),
        }),
      }
      const events = []
      for await (const event of provider.stream(mockContext, [])) {
        events.push(event)
      }
      expect(events).toContainEqual({
        type: "usage",
        totalTokens: 105,
        inputTokens: 80,
        outputTokens: 25,
        cacheTelemetryStatus: "unavailable",
        providerUsage: { inputTokens: 80, outputTokens: 25, cacheTelemetryStatus: "unavailable" },
      })
    })
  })

  describe("OpenAIResponsesProvider", () => {
    it("keeps usage evidence off the ProviderMessage returned by complete()", async () => {
      const provider = new OpenAIResponsesProvider("test-key")
      ;(provider as any).client = {
        responses: {
          create: async () => ({
            output: [{ type: "message", content: [{ type: "output_text", text: "hello" }] }],
            usage: { input_tokens: 90, output_tokens: 30, total_tokens: 120 },
          }),
        },
      }
      const message = await provider.complete(mockContext, [])
      expect((message as { tokenCount?: number }).tokenCount).toBeUndefined()
    })

    it("yields detailed usage events in stream()", async () => {
      const provider = new OpenAIResponsesProvider("test-key")
      ;(provider as any).client = {
        responses: {
          create: async () => ({
            async *[Symbol.asyncIterator]() {
              yield {
                type: "response.completed",
                response: {
                  id: "resp_123",
                  usage: { input_tokens: 90, output_tokens: 30, total_tokens: 120 },
                },
              }
            },
          }),
        },
      }
      const events = []
      for await (const event of provider.stream(mockContext, [])) {
        events.push(event)
      }
      expect(events).toContainEqual({
        type: "usage",
        totalTokens: 120,
        inputTokens: 90,
        outputTokens: 30,
        cacheTelemetryStatus: "unavailable",
        providerUsage: { inputTokens: 90, outputTokens: 30, cacheTelemetryStatus: "unavailable" },
      })
    })
  })
})
