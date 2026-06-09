import { DeepSeekProvider } from "../src/providers/deepseek.js"
import { OpenAIChatAdapter } from "../src/providers/openai-chat.js"
import { DEGRADED_REASONING_PLACEHOLDER } from "../src/providers/replay-validator.js"
import { assessProviderReplayability } from "../src/runtime/provider-replay.js"
import type { RenderedContext } from "../src/types.js"

function toolCallContext(): RenderedContext {
  return {
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
}

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

  it("rejects an assistant tool_calls turn with no following tool result (missing case)", () => {
    const adapter = new OpenAIChatAdapter()
    const context: RenderedContext = {
      systemText: "",
      turns: [
        { role: "user", content: "use a tool" },
        {
          role: "assistant",
          content: "checking",
          toolCalls: [{ id: "call_unanswered", name: "lookup", arguments: "{}" }],
        },
        { role: "user", content: "never mind" },
      ],
    }

    expect(() => adapter.buildMessages(context)).toThrow(/no tool result for call_unanswered/i)
  })

  it("rejects an assistant tool_calls turn left pending at the end of history", () => {
    const adapter = new OpenAIChatAdapter()
    const context: RenderedContext = {
      systemText: "",
      turns: [
        { role: "user", content: "use a tool" },
        {
          role: "assistant",
          content: "checking",
          toolCalls: [{ id: "call_dangling", name: "lookup", arguments: "{}" }],
        },
      ],
    }

    expect(() => adapter.buildMessages(context)).toThrow(/no tool result for call_dangling/i)
  })

  it("assessReplayability reports offending call ids before sending, without throwing", () => {
    const provider = new DeepSeekProvider("test-key")
    const assessment = provider.assessReplayability(toolCallContext())
    expect(assessment).toEqual({ ok: false, offendingCallIds: ["call_1"] })
  })

  it("assessReplayability is ok when the provider does not require reasoning replay", () => {
    const provider = new DeepSeekProvider("test-key")
    // thinking off → no reasoning replay required for this candidate
    const assessment = provider.assessReplayability(toolCallContext(), { thinking: false })
    expect(assessment).toEqual({ ok: true, offendingCallIds: [] })
  })

  it("assessProviderReplayability reports ok for providers without the hook", () => {
    const provider = { complete: async () => ({ role: "assistant" as const, content: "" }), stream: async function* () {} }
    const assessment = assessProviderReplayability(provider, toolCallContext())
    expect(assessment).toEqual({ ok: true, offendingCallIds: [] })
  })

  it("degrades missing reasoning to a placeholder instead of throwing when opted in", async () => {
    const provider = new DeepSeekProvider("test-key")
    let seenRequest: Record<string, unknown> = {}
    ;(provider as unknown as {
      client: { chat: { completions: { create(req: Record<string, unknown>): Promise<Record<string, unknown>> } } }
    }).client = {
      chat: {
        completions: {
          async create(req: Record<string, unknown>) {
            seenRequest = req
            return { choices: [{ message: { content: "ok", tool_calls: [] } }], usage: { total_tokens: 1 } }
          },
        },
      },
    }

    await expect(
      provider.complete(toolCallContext(), [], { degradeMissingReasoningReplay: true }),
    ).resolves.toMatchObject({ role: "assistant", content: "ok" })

    const messages = seenRequest.messages as Array<Record<string, unknown>>
    const assistant = messages.find(m => m.role === "assistant")
    expect(assistant?.reasoning_content).toBe(DEGRADED_REASONING_PLACEHOLDER)
    // control flag must not leak onto the wire request
    expect(seenRequest).not.toHaveProperty("degradeMissingReasoningReplay")
  })
})
