import { DeepSeekProvider } from "../src/providers/deepseek.js"
import { KimiProvider } from "../src/providers/kimi.js"
import { MiniMaxProvider } from "../src/providers/minimax.js"

describe("current provider runtime behavior", () => {
  it("uses MiniMax's Anthropic endpoint by default", () => {
    const provider = new MiniMaxProvider("test-key")
    expect((provider as unknown as { client: { baseURL: string } }).client.baseURL)
      .toBe("https://api.minimaxi.com/anthropic")
  })

  it("uses Anthropic api-key auth for MiniMax by default", () => {
    const provider = new MiniMaxProvider("test-key")
    const client = (provider as unknown as {
      client: { apiKey: string | null; authToken: string | null }
    }).client

    expect(client.apiKey).toBe("test-key")
    expect(client.authToken).toBeNull()
  })

  it("uses kimi-k2.6 as the current Kimi default", () => {
    const provider = new KimiProvider("test-key")
    expect((provider as unknown as { model: string }).model).toBe("kimi-k2.6")
  })

  it("replays MiniMax native Anthropic blocks after a tool-use turn", async () => {
    const provider = new MiniMaxProvider("test-key")
    let capturedSecondRequest: Record<string, unknown> | undefined
    let callCount = 0
    ;(provider as unknown as {
      client: {
        messages: {
          stream(req: Record<string, unknown>): AsyncIterable<Record<string, unknown>>
        }
      }
    }).client = {
      messages: {
        stream(req) {
          callCount += 1
          if (callCount === 2) capturedSecondRequest = req
          return {
            async *[Symbol.asyncIterator]() {
              if (callCount === 1) {
                yield { type: "content_block_start", index: 0, content_block: { type: "thinking", thinking: "", signature: "" } }
                yield { type: "content_block_delta", index: 0, delta: { type: "thinking_delta", thinking: "plan" } }
                yield { type: "content_block_delta", index: 0, delta: { type: "signature_delta", signature: "sig" } }
                yield { type: "content_block_stop", index: 0 }
                yield { type: "content_block_start", index: 1, content_block: { type: "text", text: "" } }
                yield { type: "content_block_delta", index: 1, delta: { type: "text_delta", text: "checking" } }
                yield { type: "content_block_stop", index: 1 }
                yield { type: "content_block_start", index: 2, content_block: { type: "tool_use", id: "call_1", name: "lookup", input: {} } }
                yield { type: "content_block_delta", index: 2, delta: { type: "input_json_delta", partial_json: '{"q":"x"}' } }
                yield { type: "content_block_stop", index: 2 }
              }
            },
          }
        },
      },
    }

    for await (const _event of provider.stream({ systemText: "", turns: [{ role: "user", content: "hi" }] }, [])) {}
    for await (const _event of provider.stream({
      systemText: "",
      turns: [
        { role: "user", content: "hi" },
        {
          role: "assistant",
          content: "checking",
          toolCalls: [{ id: "call_1", name: "lookup", arguments: '{"q":"x"}' }],
        },
        {
          role: "tool",
          content: "",
          contentParts: [{ type: "tool_result", callId: "call_1", output: "ok", isError: false }],
        },
      ],
    }, [])) {}

    expect(capturedSecondRequest?.messages).toEqual([
      { role: "user", content: "hi" },
      {
        role: "assistant",
        content: [
          { type: "thinking", thinking: "plan", signature: "sig" },
          { type: "text", text: "checking" },
          { type: "tool_use", id: "call_1", name: "lookup", input: { q: "x" } },
        ],
      },
      {
        role: "user",
        content: [{ type: "tool_result", tool_use_id: "call_1", content: "ok", is_error: false }],
      },
    ])
  })

  it("sends current DeepSeek thinking controls and keeps tools enabled", async () => {
    const provider = new DeepSeekProvider("test-key")
    let request: Record<string, unknown> | undefined
    ;(provider as unknown as {
      client: {
        chat: {
          completions: {
            create(req: Record<string, unknown>): Promise<AsyncIterable<Record<string, unknown>>>
          }
        }
      }
    }).client = {
      chat: {
        completions: {
          async create(req) {
            request = req
            return {
              async *[Symbol.asyncIterator]() {
                yield {
                  choices: [{
                    delta: { reasoning_content: "reason", content: "done" },
                    finish_reason: "stop",
                  }],
                }
              },
            }
          },
        },
      },
    }

    const events = []
    for await (const event of provider.stream(
      { systemText: "", turns: [{ role: "user", content: "hi" }] },
      [{ name: "lookup", description: "lookup", parameters: '{"type":"object"}' }],
      { reasoningEffort: "max" },
    )) {
      events.push(event)
    }

    expect(request).toMatchObject({
      model: "deepseek-v4-flash",
      reasoning_effort: "max",
      extra_body: { thinking: { type: "enabled" } },
    })
    expect(request?.tools).toBeDefined()
    expect(events).toEqual([{ type: "text_delta", delta: "done" }])
  })
})
