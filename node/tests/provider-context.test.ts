import {
  splitAnthropicSystem,
  toAnthropicMessages,
  toOpenAIMessageParams,
} from "../src/providers/base.js"
import type { Message } from "../src/types.js"

const toolConversation: Message[] = [
  { role: "system", content: "system rules" },
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
]

describe("provider-native context construction", () => {
  it("replays OpenAI tool calls and tool results with native fields", () => {
    expect(toOpenAIMessageParams(toolConversation)).toEqual([
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
    expect(splitAnthropicSystem(toolConversation)).toBe("system rules")
    expect(toAnthropicMessages(toolConversation)).toEqual([
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
    expect(toAnthropicMessages(toolConversation, () => [
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
