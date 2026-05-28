import { AnthropicProvider } from "../src/providers/anthropic.js"
import { OpenAIChatProvider } from "../src/providers/openai.js"
import { QwenProvider } from "../src/providers/qwen.js"
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
    it("reports assistant message tokenCount using only output tokens in complete()", async () => {
      const provider = new AnthropicProvider("test-key")
      ;(provider as any).client = {
        messages: {
          create: async () => ({
            content: [{ type: "text", text: "hello" }],
            usage: { input_tokens: 100, output_tokens: 20 },
          }),
        },
      }
      const message = await provider.complete(mockContext, [])
      expect(message.tokenCount).toBe(20)
    })

    it("yields detailed usage events in stream()", async () => {
      const provider = new AnthropicProvider("test-key")
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
      })
    })
  })

  describe("OpenAIProvider", () => {
    it("reports assistant message tokenCount using completion_tokens in complete()", async () => {
      const provider = new OpenAIChatProvider("test-key")
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
      expect(message.tokenCount).toBe(15)
    })

    it("yields detailed usage events in stream()", async () => {
      const provider = new OpenAIChatProvider("test-key")
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
      })
    })
  })

  describe("QwenProvider", () => {
    it("reports assistant message tokenCount using completion_tokens in complete()", async () => {
      const provider = new QwenProvider("test-key")
      ;(provider as any).client = {
        chat: {
          completions: {
            create: async () => ({
              choices: [{ message: { content: "hello" } }],
              usage: { prompt_tokens: 60, completion_tokens: 18, total_tokens: 78 },
            }),
          },
        },
      }
      const message = await provider.complete(mockContext, [])
      expect(message.tokenCount).toBe(18)
    })

    it("yields detailed usage events in stream()", async () => {
      const provider = new QwenProvider("test-key")
      ;(provider as any).client = {
        chat: {
          completions: {
            create: async () => ({
              async *[Symbol.asyncIterator]() {
                yield {
                  usage: { prompt_tokens: 60, completion_tokens: 18, total_tokens: 78 },
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
        totalTokens: 78,
        inputTokens: 60,
        outputTokens: 18,
      })
    })
  })

  describe("GeminiProvider", () => {
    it("reports assistant message tokenCount using candidatesTokenCount in complete()", async () => {
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
      expect(message.tokenCount).toBe(25)
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
      })
    })
  })

  describe("OpenAIResponsesProvider", () => {
    it("reports assistant message tokenCount using output_tokens in complete()", async () => {
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
      expect(message.tokenCount).toBe(30)
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
      })
    })
  })
})
