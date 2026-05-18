import { OpenAIResponsesAdapter } from "../src/providers/openai-responses.js"
import type { RenderedContext } from "../src/types.js"

const context: RenderedContext = {
  systemText: "system rules",
  turns: [
    { role: "user", content: "Find weather" },
    {
      role: "assistant",
      content: "",
      toolCalls: [{ id: "call_1", name: "lookup", arguments: '{"city":"Shanghai"}' }],
    },
    {
      role: "tool",
      content: "",
      contentParts: [{ type: "tool_result", callId: "call_1", output: "sunny", isError: false }],
    },
  ],
}

describe("OpenAIResponsesAdapter", () => {
  it("builds Responses tools with flat function metadata", () => {
    const adapter = new OpenAIResponsesAdapter()
    expect(adapter.buildTools([{
      name: "lookup",
      description: "Lookup",
      parameters: '{"type":"object","properties":{}}',
    }])).toEqual([{
      type: "function",
      name: "lookup",
      description: "Lookup",
      parameters: { type: "object", properties: {} },
    }])
  })

  it("extracts system instructions separately from response input items", () => {
    const adapter = new OpenAIResponsesAdapter()
    expect(adapter.buildInstructions(context)).toBe("system rules")
    expect(adapter.buildInput(context)).toEqual([
      { role: "user", content: "Find weather" },
      { type: "function_call", call_id: "call_1", name: "lookup", arguments: '{"city":"Shanghai"}' },
      { type: "function_call_output", call_id: "call_1", output: "sunny" },
    ])
  })

  it("serializes only the uncovered tail when continuing from a previous response", () => {
    const adapter = new OpenAIResponsesAdapter()
    // context.turns = [user(0), assistant+toolcall(1), tool_result(2)]
    // covering 2 messages (user + assistant) leaves tool_result as the uncovered tail
    expect(adapter.buildInput(context, {
      previousResponseId: "resp_1",
      coveredMessageCount: 2,
    })).toEqual([
      { type: "function_call_output", call_id: "call_1", output: "sunny" },
    ])
  })

  it("keeps assistant text when a historical turn also contains tool calls", () => {
    const adapter = new OpenAIResponsesAdapter()
    const ctx: RenderedContext = {
      systemText: "",
      turns: [
        {
          role: "assistant",
          content: "I will check.",
          toolCalls: [{ id: "call_1", name: "lookup", arguments: '{"city":"Shanghai"}' }],
        },
      ],
    }
    expect(adapter.buildInput(ctx)).toEqual([
      { role: "assistant", content: "I will check." },
      { type: "function_call", call_id: "call_1", name: "lookup", arguments: '{"city":"Shanghai"}' },
    ])
  })

  it("normalizes Responses output items", () => {
    const adapter = new OpenAIResponsesAdapter()
    expect(adapter.decodeOutput([
      {
        type: "message",
        role: "assistant",
        content: [{ type: "output_text", text: "done" }],
      },
      {
        type: "function_call",
        call_id: "call_1",
        name: "lookup",
        arguments: '{"city":"Shanghai"}',
      },
    ])).toEqual({
      content: "done",
      toolCalls: [{ id: "call_1", name: "lookup", arguments: '{"city":"Shanghai"}' }],
    })
  })
})
