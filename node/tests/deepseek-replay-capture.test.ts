import { DeepSeekProvider } from "../src/providers/deepseek.js"
import type { RenderedContext } from "../src/types.js"

describe("DeepSeek provider replay capture", () => {
  it("captures streamed reasoning_content with tool calls as provider-scoped replay", async () => {
    const provider = new DeepSeekProvider("test-key", "deepseek-v4-flash")
    ;(provider as unknown as {
      client: { chat: { completions: { create(req: Record<string, unknown>): Promise<AsyncIterable<Record<string, unknown>>> } } }
    }).client = {
      chat: {
        completions: {
          async create() {
            return {
              async *[Symbol.asyncIterator]() {
                yield { choices: [{ delta: { reasoning_content: "real plan" }, finish_reason: null }] }
                yield { choices: [{ delta: { content: "checking" }, finish_reason: null }] }
                yield {
                  choices: [{
                    delta: { tool_calls: [{ index: 0, id: "call_1", function: { name: "lookup", arguments: "{}" } }] },
                    finish_reason: "tool_calls",
                  }],
                }
              },
            }
          },
        },
      },
    }

    const events = []
    for await (const event of provider.stream({
      systemText: "",
      turns: [{ role: "user", content: "use lookup" }],
    }, [])) {
      events.push(event)
    }

    expect(events).toContainEqual({ type: "text_delta", delta: "checking" })
    expect(events).toContainEqual({ type: "tool_call", id: "call_1", name: "lookup", arguments: {} })
    expect(provider.peekProviderReplay({
      content: "checking",
      toolCalls: [{ id: "call_1", name: "lookup", arguments: "{}" }],
    })).toEqual({
      schema_version: 2,
      provider: "deepseek",
      protocol: "openai-chat",
      model: "deepseek-v4-flash",
      reasoning_content: "real plan",
      tool_calls: [{
        id: "call_1",
        type: "function",
        function: { name: "lookup", arguments: "{}" },
      }],
    })
  })

  it("does not synthesize empty reasoning_content for streamed tool calls", async () => {
    const provider = new DeepSeekProvider("test-key", "deepseek-v4-flash")
    ;(provider as unknown as {
      client: { chat: { completions: { create(req: Record<string, unknown>): Promise<AsyncIterable<Record<string, unknown>>> } } }
    }).client = {
      chat: {
        completions: {
          async create() {
            return {
              async *[Symbol.asyncIterator]() {
                yield { choices: [{ delta: { content: "checking" }, finish_reason: null }] }
                yield {
                  choices: [{
                    delta: { tool_calls: [{ index: 0, id: "call_1", function: { name: "lookup", arguments: "{}" } }] },
                    finish_reason: "tool_calls",
                  }],
                }
              },
            }
          },
        },
      },
    }

    for await (const _event of provider.stream({
      systemText: "",
      turns: [{ role: "user", content: "use lookup" }],
    }, [])) {
      // drain stream
    }

    expect(provider.peekProviderReplay({
      content: "checking",
      toolCalls: [{ id: "call_1", name: "lookup", arguments: "{}" }],
    })).toBeUndefined()
  })

  it("captures non-stream reasoning_content with tool calls as provider-scoped replay", async () => {
    const provider = new DeepSeekProvider("test-key", "deepseek-v4-flash")
    ;(provider as unknown as {
      client: { chat: { completions: { create(req: Record<string, unknown>): Promise<Record<string, unknown>> } } }
    }).client = {
      chat: {
        completions: {
          async create() {
            return {
              choices: [{
                message: {
                  content: "checking",
                  reasoning_content: "real plan",
                  tool_calls: [{
                    id: "call_1",
                    type: "function",
                    function: { name: "lookup", arguments: "{}" },
                  }],
                },
              }],
              usage: { total_tokens: 12, completion_tokens: 7 },
            }
          },
        },
      },
    }

    const message = await provider.complete({
      systemText: "",
      turns: [{ role: "user", content: "use lookup" }],
    }, [])

    expect(message).toEqual({
      role: "assistant",
      content: "checking",
      tokenCount: 7,
      toolCalls: [{ id: "call_1", name: "lookup", arguments: "{}" }],
    })
    expect(provider.peekProviderReplay(message)).toEqual({
      schema_version: 2,
      provider: "deepseek",
      protocol: "openai-chat",
      model: "deepseek-v4-flash",
      reasoning_content: "real plan",
      tool_calls: [{
        id: "call_1",
        type: "function",
        function: { name: "lookup", arguments: "{}" },
      }],
    })
  })

  it("does not merge provider replay envelope fields into OpenAI-compatible request messages", async () => {
    const provider = new DeepSeekProvider("test-key", "deepseek-v4-flash")
    let capturedRequest: Record<string, unknown> | undefined
    ;(provider as unknown as {
      client: { chat: { completions: { create(req: Record<string, unknown>): Promise<Record<string, unknown>> } } }
    }).client = {
      chat: {
        completions: {
          async create(req: Record<string, unknown>) {
            capturedRequest = req
            return {
              choices: [{ message: { content: "ok", tool_calls: [] } }],
              usage: { total_tokens: 1 },
            }
          },
        },
      },
    }

    const context: RenderedContext = {
      systemText: "",
      turns: [
        { role: "user", content: "use lookup" },
        {
          role: "assistant",
          content: "checking",
          toolCalls: [{ id: "call_1", name: "lookup", arguments: "{}" }],
        },
        {
          role: "tool",
          content: "",
          contentParts: [{ type: "tool_result", callId: "call_1", output: "done", isError: false }],
        },
      ],
    }
    provider.seedProviderReplay(context.turns[1], {
      schema_version: 2,
      provider: "deepseek",
      protocol: "openai-chat",
      model: "deepseek-v4-flash",
      reasoning_content: "real plan",
      tool_calls: [{ id: "call_1", type: "function", function: { name: "lookup", arguments: "{}" } }],
      native_message: { content: "checking" },
    })

    await provider.complete(context, [])

    const messages = capturedRequest?.messages as Array<Record<string, unknown>>
    expect(messages[1]).toEqual({
      role: "assistant",
      content: "checking",
      reasoning_content: "real plan",
      tool_calls: [{
        id: "call_1",
        type: "function",
        function: { name: "lookup", arguments: "{}" },
      }],
    })
    expect(messages[1]).not.toHaveProperty("schema_version")
    expect(messages[1]).not.toHaveProperty("provider")
    expect(messages[1]).not.toHaveProperty("protocol")
    expect(messages[1]).not.toHaveProperty("native_message")
  })
})
