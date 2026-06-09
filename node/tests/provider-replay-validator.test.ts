import { DeepSeekProvider } from "../src/providers/deepseek.js"
import { OpenAIChatAdapter } from "../src/providers/openai-chat.js"
import type { RenderedContext } from "../src/types.js"

describe("OpenAI-compatible replay validation", () => {
  it("rejects orphan tool results before sending provider messages", () => {
    const adapter = new OpenAIChatAdapter()
    const context: RenderedContext = {
      systemText: "",
      turns: [
        { role: "user", content: "use a tool" },
        {
          role: "tool",
          content: "",
          contentParts: [{ type: "tool_result", callId: "call_orphan", output: "done", isError: false }],
        },
      ],
    }

    expect(() => adapter.buildMessages(context)).toThrow(/orphan tool result.*call_orphan/i)
  })

  it("allows tool results paired with the previous assistant tool call sequence", () => {
    const adapter = new OpenAIChatAdapter()
    const context: RenderedContext = {
      systemText: "",
      turns: [
        { role: "user", content: "use a tool" },
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

    expect(adapter.buildMessages(context)).toEqual([
      { role: "user", content: "use a tool" },
      {
        role: "assistant",
        content: "checking",
        tool_calls: [{
          id: "call_1",
          type: "function",
          function: { name: "lookup", arguments: "{}" },
        }],
      },
      { role: "tool", tool_call_id: "call_1", content: "done" },
    ])
  })

  it("fails fast when DeepSeek thinking replay is missing for assistant tool calls", async () => {
    const provider = new DeepSeekProvider("test-key")
    const context: RenderedContext = {
      systemText: "",
      turns: [
        { role: "user", content: "use a tool" },
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

    await expect(provider.complete(context, [])).rejects.toThrow(/deepseek.*replay requires non-empty reasoning_content/i)
  })

  it("allows DeepSeek replay without reasoning when thinking is disabled", async () => {
    const provider = new DeepSeekProvider("test-key")
    const context: RenderedContext = {
      systemText: "",
      turns: [
        { role: "user", content: "use a tool" },
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
    ;(provider as unknown as {
      client: { chat: { completions: { create(req: Record<string, unknown>): Promise<Record<string, unknown>> } } }
    }).client = {
      chat: {
        completions: {
          async create() {
            return {
              choices: [{ message: { content: "ok", tool_calls: [] } }],
              usage: { total_tokens: 1 },
            }
          },
        },
      },
    }

    await expect(provider.complete(context, [], { thinking: false })).resolves.toEqual({
      role: "assistant",
      content: "ok",
      tokenCount: 1,
      toolCalls: [],
    })
  })
})
