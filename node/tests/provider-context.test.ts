import {
  toAnthropicMessages,
  toOpenAIMessageParams,
} from "../src/providers/base.js"
import type { RenderedContext } from "../src/types.js"

const context: RenderedContext = {
  systemText: "system rules",
  turns: [
    { role: "user", content: "What is the weather?" },
    {
      role: "assistant",
      content: "I'll check.",
      toolCalls: [{ id: "call_1", name: "get_weather", arguments: '{"city":"Shanghai"}' }],
    },
    {
      role: "tool",
      content: "",
      contentParts: [{ type: "tool_result", callId: "call_1", output: "sunny", isError: false }],
    },
  ],
}

describe("provider-native context construction", () => {
  it("replays OpenAI tool calls and tool results with native fields", () => {
    expect(toOpenAIMessageParams(context)).toEqual([
      { role: "system", content: "system rules" },
      { role: "user", content: "What is the weather?" },
      {
        role: "assistant",
        content: "I'll check.",
        tool_calls: [{
          id: "call_1",
          type: "function",
          function: { name: "get_weather", arguments: '{"city":"Shanghai"}' },
        }],
      },
      { role: "tool", tool_call_id: "call_1", content: "sunny" },
    ])
  })

  it("replays Anthropic tool calls and tool results as content blocks", () => {
    expect(context.systemText).toBe("system rules")
    expect(toAnthropicMessages(context.turns)).toEqual([
      { role: "user", content: "What is the weather?" },
      {
        role: "assistant",
        content: [
          { type: "text", text: "I'll check." },
          { type: "tool_use", id: "call_1", name: "get_weather", input: { city: "Shanghai" } },
        ],
      },
      {
        role: "user",
        content: [{ type: "tool_result", tool_use_id: "call_1", content: "sunny", is_error: false }],
      },
    ])
  })

  it("lets Anthropic callers replay preserved native assistant blocks", () => {
    expect(toAnthropicMessages(context.turns, () => [
      { type: "thinking", thinking: "reason first", signature: "sig" },
      { type: "tool_use", id: "call_1", name: "get_weather", input: { city: "Shanghai" } },
    ])).toEqual([
      { role: "user", content: "What is the weather?" },
      {
        role: "assistant",
        content: [
          { type: "thinking", thinking: "reason first", signature: "sig" },
          { type: "tool_use", id: "call_1", name: "get_weather", input: { city: "Shanghai" } },
        ],
      },
      {
        role: "user",
        content: [{ type: "tool_result", tool_use_id: "call_1", content: "sunny", is_error: false }],
      },
    ])
  })
})
