import { jest } from "@jest/globals"
import { AnthropicProvider } from "../src/providers/anthropic.js"
import { GeminiProvider } from "../src/providers/gemini.js"
import { OllamaProvider } from "../src/providers/ollama.js"
import { OpenAIChatProvider } from "../src/providers/openai.js"
import { OpenAIResponsesProvider } from "../src/providers/openai-responses.js"
import { QwenProvider } from "../src/providers/qwen.js"
import { DeepSeekProvider } from "../src/providers/deepseek.js"
import type { RenderedContext, ToolSchema } from "../src/types.js"

const context: RenderedContext = {
  systemText: "system",
  turns: [{ role: "user", content: "hi" }],
}
const tools: ToolSchema[] = [{ name: "lookup", description: "lookup", parameters: '{"type":"object"}' }]

describe("provider control-plane forwarding", () => {
  it("forwards Anthropic extensions in stream and complete without allowing structural overrides", async () => {
    const provider = new AnthropicProvider("test-key")
    const requests: Array<Record<string, unknown>> = []
    ;(provider as unknown as { client: { messages: { create(req: Record<string, unknown>): Promise<Record<string, unknown>>; stream(req: Record<string, unknown>): AsyncIterable<Record<string, unknown>> } } }).client = {
      messages: {
        async create(req) {
          requests.push(req)
          return { content: [{ type: "text", text: "done" }], usage: { input_tokens: 1, output_tokens: 1 } }
        },
        stream(req) {
          requests.push(req)
          return { async *[Symbol.asyncIterator]() {} }
        },
      },
    }

    await provider.complete(context, tools, {
      thinking: { type: "enabled", budget_tokens: 10000 },
      max_tokens: 32000,
      temperature: 0.2,
      model: "wrong",
      messages: [],
    })
    for await (const _event of provider.stream(context, tools, {
      thinking: { type: "enabled", budget_tokens: 10000 },
      max_tokens: 32000,
      temperature: 0.2,
      model: "wrong",
      messages: [],
    })) {}

    for (const req of requests) {
      expect(req).toMatchObject({
        model: "claude-sonnet-4-6",
        max_tokens: 32000,
        thinking: { type: "enabled", budget_tokens: 10000 },
        temperature: 0.2,
        system: "system",
      })
      expect(req.messages).toEqual([{ role: "user", content: "hi" }])
      expect(req.tools).toBeDefined()
    }
  })

  it("uses Anthropic beta messages when betas are requested", async () => {
    const provider = new AnthropicProvider("test-key")
    const requests: Array<Record<string, unknown>> = []
    ;(provider as unknown as {
      client: {
        messages: { create(req: Record<string, unknown>): Promise<Record<string, unknown>>; stream(req: Record<string, unknown>): AsyncIterable<Record<string, unknown>> }
        beta: { messages: { create(req: Record<string, unknown>): Promise<Record<string, unknown>>; stream(req: Record<string, unknown>): AsyncIterable<Record<string, unknown>> } }
      }
    }).client = {
      messages: {
        async create() { throw new Error("stable path should not be used") },
        stream() { throw new Error("stable path should not be used") },
      },
      beta: {
        messages: {
          async create(req) {
            requests.push(req)
            return { content: [{ type: "text", text: "done" }], usage: { input_tokens: 1, output_tokens: 1 } }
          },
          stream(req) {
            requests.push(req)
            return { async *[Symbol.asyncIterator]() {} }
          },
        },
      },
    }

    const extensions = { betas: ["interleaved-thinking-2025-05-14"] }
    await provider.complete(context, tools, extensions)
    for await (const _event of provider.stream(context, tools, extensions)) {}

    expect(requests).toHaveLength(2)
    expect(requests[0].betas).toEqual(["interleaved-thinking-2025-05-14"])
    expect(requests[1].betas).toEqual(["interleaved-thinking-2025-05-14"])
  })

  it("forwards OpenAI Chat extensions in stream and complete while preserving SDK-owned fields", async () => {
    const provider = new OpenAIChatProvider("test-key")
    const requests: Array<Record<string, unknown>> = []
    ;(provider as unknown as { client: { chat: { completions: { create(req: Record<string, unknown>): Promise<unknown> } } } }).client = {
      chat: {
        completions: {
          async create(req) {
            requests.push(req)
            if (req.stream) return { async *[Symbol.asyncIterator]() {} }
            return { choices: [{ message: { content: "done" } }], usage: { total_tokens: 2 } }
          },
        },
      },
    }

    await provider.complete(context, tools, { temperature: 0.1, reasoning_effort: "high", model: "wrong" })
    for await (const _event of provider.stream(context, tools, { temperature: 0.1, reasoning_effort: "high", stream: false })) {}

    expect(requests[0]).toMatchObject({ model: "gpt-4o", temperature: 0.1, reasoning_effort: "high" })
    expect(requests[1]).toMatchObject({ model: "gpt-4o", temperature: 0.1, reasoning_effort: "high", stream: true })
    expect(requests[1].stream_options).toEqual({ include_usage: true })
  })

  it("forwards Responses API extensions in stream and complete", async () => {
    const provider = new OpenAIResponsesProvider("test-key")
    const requests: Array<Record<string, unknown>> = []
    ;(provider as unknown as { client: { responses: { create(req: Record<string, unknown>): Promise<unknown> } } }).client = {
      responses: {
        async create(req) {
          requests.push(req)
          if (req.stream) return { async *[Symbol.asyncIterator]() {} }
          return { output: [], usage: { total_tokens: 1 } }
        },
      },
    }

    await provider.complete(context, tools, { temperature: 0.1, reasoning: { effort: "high" }, model: "wrong" })
    for await (const _event of provider.stream(context, tools, { temperature: 0.1, reasoning: { effort: "high" }, stream: false })) {}

    expect(requests[0]).toMatchObject({ model: "gpt-4.1", temperature: 0.1, reasoning: { effort: "high" } })
    expect(requests[1]).toMatchObject({ model: "gpt-4.1", temperature: 0.1, reasoning: { effort: "high" }, stream: true })
  })

  it("maps Gemini extensions into request config for stream and complete", async () => {
    const provider = new GeminiProvider("test-key")
    const modelConfigs: Array<Record<string, unknown>> = []
    const requests: Array<Record<string, unknown>> = []
    ;(provider as unknown as { genAI: { getGenerativeModel(config: Record<string, unknown>): Record<string, unknown> } }).genAI = {
      getGenerativeModel(config) {
        modelConfigs.push(config)
        return {
          async generateContent(req: Record<string, unknown>) {
            requests.push(req)
            return { response: { candidates: [{ content: { parts: [{ text: "done" }] } }], usageMetadata: { totalTokenCount: 1 } } }
          },
          async generateContentStream(req: Record<string, unknown>) {
            requests.push(req)
            return {
              stream: { async *[Symbol.asyncIterator]() {} },
              response: Promise.resolve({ usageMetadata: { totalTokenCount: 1 } }),
            }
          },
        }
      },
    }

    const extensions = {
      generationConfig: { temperature: 0.2, thinkingConfig: { thinkingBudget: 1024 } },
      model: "wrong",
    }
    await provider.complete(context, tools, extensions)
    for await (const _event of provider.stream(context, tools, extensions)) {}

    expect(modelConfigs[0]).toMatchObject({ model: "gemini-2.0-flash", generationConfig: extensions.generationConfig })
    expect(modelConfigs[1]).toMatchObject({ model: "gemini-2.0-flash", generationConfig: extensions.generationConfig })
    expect(requests).toEqual([{ contents: [{ role: "user", parts: [{ text: "hi" }] }] }, { contents: [{ role: "user", parts: [{ text: "hi" }] }] }])
  })

  it("forwards Ollama extensions and tools in stream and complete", async () => {
    const provider = new OllamaProvider()
    const requests: Array<Record<string, unknown>> = []
    const originalFetch = global.fetch
    global.fetch = jest.fn(async (_url: string | URL | Request, init?: RequestInit) => {
      requests.push(JSON.parse(String(init?.body)))
      if (requests.length === 1) {
        return new Response(JSON.stringify({ message: { content: "done" } }), { status: 200 })
      }
      return new Response(new ReadableStream({ start(controller) { controller.close() } }), { status: 200 })
    }) as typeof fetch

    try {
      await provider.complete(context, tools, { think: true, options: { temperature: 0.2 }, model: "wrong" })
      for await (const _event of provider.stream(context, tools, { think: true, options: { temperature: 0.2 }, stream: false })) {}
    } finally {
      global.fetch = originalFetch
    }

    expect(requests[0]).toMatchObject({ model: "llama3", think: true, options: { temperature: 0.2 }, stream: false })
    expect(requests[1]).toMatchObject({ model: "llama3", think: true, options: { temperature: 0.2 }, stream: true })
    expect(requests[0].tools).toBeDefined()
    expect(requests[1].tools).toBeDefined()
  })

  it("applies DeepSeek native thinking controls in complete as well as stream", async () => {
    const provider = new DeepSeekProvider("test-key")
    let request: Record<string, unknown> | undefined
    ;(provider as unknown as { client: { chat: { completions: { create(req: Record<string, unknown>): Promise<unknown> } } } }).client = {
      chat: {
        completions: {
          async create(req) {
            request = req
            return { choices: [{ message: { content: "done" } }], usage: { total_tokens: 1 } }
          },
        },
      },
    }

    await provider.complete(context, tools, { thinking: false, reasoningEffort: "max", temperature: 0.3 })

    expect(request).toMatchObject({
      temperature: 0.3,
      reasoning_effort: "max",
      extra_body: { thinking: { type: "disabled" } },
    })
  })

  it("keeps Qwen native thinking controls while forwarding unrelated extensions", async () => {
    const provider = new QwenProvider("test-key")
    let request: Record<string, unknown> | undefined
    ;(provider as unknown as { client: { chat: { completions: { create(req: Record<string, unknown>): Promise<unknown> } } } }).client = {
      chat: {
        completions: {
          async create(req) {
            request = req
            return { async *[Symbol.asyncIterator]() {} }
          },
        },
      },
    }

    for await (const _event of provider.stream(context, tools, {
      enableThinking: true,
      thinkingBudget: 2048,
      temperature: 0.3,
    })) {}

    expect(request).toMatchObject({
      temperature: 0.3,
      extra_body: { enable_thinking: true, thinking_budget: 2048 },
    })
  })

  it("applies Qwen native thinking controls in complete as well as stream", async () => {
    const provider = new QwenProvider("test-key")
    let request: Record<string, unknown> | undefined
    ;(provider as unknown as { client: { chat: { completions: { create(req: Record<string, unknown>): Promise<unknown> } } } }).client = {
      chat: {
        completions: {
          async create(req) {
            request = req
            return { choices: [{ message: { content: "done" } }], usage: { total_tokens: 1 } }
          },
        },
      },
    }

    await provider.complete(context, tools, { enableThinking: true, thinkingBudget: 1024, temperature: 0.2 })

    expect(request).toMatchObject({
      temperature: 0.2,
      extra_body: { enable_thinking: true, thinking_budget: 1024 },
    })
  })
})
