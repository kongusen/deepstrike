import { MiniMaxAnthropicProvider, MiniMaxOpenAIProvider } from "../src/providers/minimax.js"
import { createProvider } from "../src/providers/catalog.js"

function installComplete(provider: unknown, message: Record<string, unknown>, captured?: (req: Record<string, unknown>) => void) {
  ;(provider as { client: { chat: { completions: { create(req: Record<string, unknown>): Promise<Record<string, unknown>> } } } }).client = {
    chat: {
      completions: {
        async create(req: Record<string, unknown>) {
          captured?.(req)
          return { choices: [{ message }], usage: { total_tokens: 10, completion_tokens: 6 } }
        },
      },
    },
  }
}

describe("MiniMax provider split", () => {
  it("MiniMaxAnthropicProvider advertises the anthropic-messages protocol under the minimax identity", () => {
    const provider = new MiniMaxAnthropicProvider("test-key")
    expect(provider.descriptor?.()).toMatchObject({ provider: "minimax", protocol: "anthropic-messages" })
    expect((provider as unknown as { client: { baseURL: string } }).client.baseURL).toBe("https://api.minimaxi.com/anthropic")
  })

  it("MiniMaxOpenAIProvider advertises the openai-chat protocol on the OpenAI-compatible endpoint", () => {
    const provider = new MiniMaxOpenAIProvider("test-key")
    expect(provider.descriptor?.()).toMatchObject({ provider: "minimax", protocol: "openai-chat" })
    expect((provider as unknown as { client: { baseURL: string } }).client.baseURL).toBe("https://api.minimaxi.com/v1")
  })

  it("createProvider routes the minimax.openai endpoint to MiniMaxOpenAIProvider", () => {
    const provider = createProvider({ model: "minimax/MiniMax-M2.7", apiKey: "k", endpoint: "minimax.openai" })
    expect(provider).toBeInstanceOf(MiniMaxOpenAIProvider)
    const anthropic = createProvider({ model: "minimax/MiniMax-M2.7", apiKey: "k" })
    expect(anthropic).toBeInstanceOf(MiniMaxAnthropicProvider)
  })

  it("defaults requests to reasoning_split: true and captures reasoning_content + reasoning_details + tool_calls", async () => {
    const provider = new MiniMaxOpenAIProvider("test-key", "MiniMax-M2.7")
    let req: Record<string, unknown> | undefined
    installComplete(provider, {
      content: "checking",
      reasoning_content: "real plan",
      reasoning_details: [{ type: "reasoning.text", text: "real plan" }],
      tool_calls: [{ id: "call_1", type: "function", function: { name: "lookup", arguments: "{}" } }],
    }, r => { req = r })

    const message = await provider.complete({ systemText: "", turns: [{ role: "user", content: "use lookup" }] }, [])

    expect(req?.reasoning_split).toBe(true)
    expect(message.toolCalls).toEqual([{ id: "call_1", name: "lookup", arguments: "{}" }])
    expect(provider.peekProviderReplay?.(message)).toEqual({
      schema_version: 2,
      provider: "minimax",
      protocol: "openai-chat",
      model: "MiniMax-M2.7",
      reasoning_content: "real plan",
      reasoning_details: [{ type: "reasoning.text", text: "real plan" }],
      tool_calls: [{ id: "call_1", type: "function", function: { name: "lookup", arguments: "{}" } }],
    })
  })

  it("preserves reasoning_details across streamed tool turns", async () => {
    const provider = new MiniMaxOpenAIProvider("test-key", "MiniMax-M2.7")
    ;(provider as unknown as { client: { chat: { completions: { create(): Promise<AsyncIterable<Record<string, unknown>>> } } } }).client = {
      chat: {
        completions: {
          async create() {
            return {
              async *[Symbol.asyncIterator]() {
                yield { choices: [{ delta: { reasoning_content: "real plan", reasoning_details: [{ type: "reasoning.text", text: "real plan" }] }, finish_reason: null }] }
                yield { choices: [{ delta: { content: "checking" }, finish_reason: null }] }
                yield { choices: [{ delta: { tool_calls: [{ index: 0, id: "call_1", function: { name: "lookup", arguments: "{}" } }] }, finish_reason: "tool_calls" }] }
              },
            }
          },
        },
      },
    }

    for await (const _ of provider.stream({ systemText: "", turns: [{ role: "user", content: "use lookup" }] }, [])) { /* drain */ }

    expect(provider.peekProviderReplay?.({ content: "checking", toolCalls: [{ id: "call_1", name: "lookup", arguments: "{}" }] })).toEqual({
      schema_version: 2,
      provider: "minimax",
      protocol: "openai-chat",
      model: "MiniMax-M2.7",
      reasoning_content: "real plan",
      reasoning_details: [{ type: "reasoning.text", text: "real plan" }],
      tool_calls: [{ id: "call_1", type: "function", function: { name: "lookup", arguments: "{}" } }],
    })
  })
})
