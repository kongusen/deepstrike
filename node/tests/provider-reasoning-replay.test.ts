import { assistantReplayKey } from "../src/runtime/provider-replay.js"
import { OpenAIProvider } from "../src/providers/openai.js"
import { ThinkingTagStreamExtractor } from "../src/providers/base.js"
import { buildContents } from "../src/providers/gemini.js"

describe("assistantReplayKey normalization", () => {
  it("normalizes tool call arguments with different key order and spacing", () => {
    const key1 = assistantReplayKey({
      content: "test",
      toolCalls: [
        { id: "c1", name: "tool", arguments: '{\n  "b": 2,\n  "a": 1\n}' }
      ]
    })
    const key2 = assistantReplayKey({
      content: "test",
      toolCalls: [
        { id: "c1", name: "tool", arguments: '{"a":1,"b":2}' }
      ]
    })
    expect(key1).toBe(key2)
  })

  it("handles nested structures and lists in tool call arguments", () => {
    const key1 = assistantReplayKey({
      content: "test",
      toolCalls: [
        { id: "c1", name: "tool", arguments: '{"y":[1,{"nested":true}],"x":1}' }
      ]
    })
    const key2 = assistantReplayKey({
      content: "test",
      toolCalls: [
        { id: "c1", name: "tool", arguments: '{"x":1,"y":[1,{"nested":true}]}' }
      ]
    })
    expect(key1).toBe(key2)
  })

  it("handles non-JSON strings gracefully", () => {
    const key1 = assistantReplayKey({
      content: "test",
      toolCalls: [
        { id: "c1", name: "tool", arguments: 'not valid json' }
      ]
    })
    const key2 = assistantReplayKey({
      content: "test",
      toolCalls: [
        { id: "c1", name: "tool", arguments: 'not valid json' }
      ]
    })
    expect(key1).toBe(key2)
  })
})

describe("OpenAIProvider reasoning replay empty string handling", () => {
  it("does not discard empty string reasoning_content during peek and seed", () => {
    const provider = new OpenAIProvider("test-key")
    const message = {
      content: "output text",
      toolCalls: [{ id: "c1", name: "tool", arguments: '{"a":1}' }]
    }

    // Seed empty reasoning content
    provider.seedProviderReplay(message, { reasoning_content: "" })

    // Peek and verify it is not discarded
    const replay = provider.peekProviderReplay(message)
    expect(replay).toEqual({ reasoning_content: "" })
  })

  it("returns undefined if reasoning_content is not set at all", () => {
    const provider = new OpenAIProvider("test-key")
    const message = {
      content: "output text",
      toolCalls: [{ id: "c1", name: "tool", arguments: '{"a":1}' }]
    }

    // Peek without seeding
    const replay = provider.peekProviderReplay(message)
    expect(replay).toBeUndefined()
  })
})

describe("ThinkingTagStreamExtractor", () => {
  it("extracts clean content and thinking blocks correctly", () => {
    const extractor = new ThinkingTagStreamExtractor()
    const events: Array<{ type: string; content: string }> = []

    for (const chunk of ["hello ", "<thi", "nk> internal thought </th", "ink> final answer"]) {
      for (const event of extractor.feed(chunk)) {
        events.push(event)
      }
    }
    for (const event of extractor.flush()) {
      events.push(event)
    }

    expect(events).toEqual([
      { type: "text", content: "hello " },
      { type: "thinking", content: " internal thought " },
      { type: "text", content: " final answer" }
    ])
  })

  it("handles streams without thinking tags", () => {
    const extractor = new ThinkingTagStreamExtractor()
    const events: Array<{ type: string; content: string }> = []

    for (const chunk of ["just ", "regular ", "text"]) {
      for (const event of extractor.feed(chunk)) {
        events.push(event)
      }
    }
    for (const event of extractor.flush()) {
      events.push(event)
    }

    expect(events).toEqual([
      { type: "text", content: "just " },
      { type: "text", content: "regular " },
      { type: "text", content: "text" }
    ])
  })
})

describe("Gemini tool response name resolution", () => {
  it("resolves the original function name instead of using callId", () => {
    const turns = [
      {
        role: "user" as const,
        content: "call the function"
      },
      {
        role: "assistant" as const,
        content: "",
        toolCalls: [
          { id: "call_123", name: "my_actual_tool", arguments: "{}" }
        ]
      },
      {
        role: "tool" as const,
        content: "",
        contentParts: [
          { type: "tool_result" as const, callId: "call_123", output: "success", isError: false }
        ]
      }
    ]

    const contents = buildContents(turns)
    const toolTurn = contents[2]
    expect(toolTurn.role).toBe("user")
    expect(toolTurn.parts).toEqual([
      {
        functionResponse: {
          name: "my_actual_tool",
          response: { output: "success" }
        }
      }
    ])
  })
})
