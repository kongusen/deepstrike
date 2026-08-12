/**
 * spc_013-A-00: shared deterministic fixtures for the wire/stream characterization.
 *
 * One rich rendered context used across all five generation protocols. It deliberately
 * exercises every shape spc_011/012 put into production: partitioned system prompt
 * (stable/knowledge), plain text turns, assistant tool_calls, a legacy text tool_result,
 * a user image turn (multimodal, 011-B/011-D), and a STRUCTURED tool_result carrying a
 * `contentParts` image block (spc_012). If any of these shapes drifts, exactly one golden
 * section should move.
 *
 * NO RANDOM DATA — every byte here is fixed (DoD: fixture 可复现).
 */
import type { Message, RenderedContext, ToolSchema } from "../../src/types.js"

export const CHARACTERIZATION_TOOLS: ToolSchema[] = [
  {
    name: "get_weather",
    description: "Get the current weather for a city",
    parameters: '{"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}',
  },
]

export const CHARACTERIZATION_CONTEXT: RenderedContext = {
  systemText: "You are a weather assistant.\n\nKnown fact: the Eiffel Tower is in Paris.",
  systemStable: "You are a weather assistant.",
  systemKnowledge: "Known fact: the Eiffel Tower is in Paris.",
  turns: [
    { role: "user", content: "Weather in Paris? Use your tools.", toolCalls: [] },
    {
      role: "assistant",
      content: "Checking the weather for Paris.",
      toolCalls: [{ id: "call_1", name: "get_weather", arguments: '{"city":"Paris"}' }],
    },
    {
      role: "tool",
      content: "sunny, 24C",
      toolCalls: [],
      contentParts: [{ type: "tool_result", callId: "call_1", output: "sunny, 24C", isError: false }],
    },
    {
      role: "user",
      content: "Here is a photo I took.\n[image]",
      toolCalls: [],
      contentParts: [
        { type: "text", text: "Here is a photo I took." },
        { type: "image", data: "aGVsbG8=", mediaType: "image/png" },
      ],
    },
    {
      role: "assistant",
      content: "Now checking Lyon with a screenshot tool.",
      toolCalls: [{ id: "call_2", name: "get_weather", arguments: '{"city":"Lyon"}' }],
    },
    {
      role: "tool",
      content: "cloudy, 18C\n[image]",
      toolCalls: [],
      contentParts: [{
        type: "tool_result",
        callId: "call_2",
        output: "cloudy, 18C\n[image]",
        isError: false,
        contentParts: [
          { type: "text", text: "cloudy, 18C" },
          { type: "image", source: { kind: "base64", data: "d29ybGQ=" }, mediaType: "image/png" },
        ],
      }],
    },
  ] as Message[],
}

/** Fixed usage figures so usage normalization (spc_011-C-07) is locked too. */
export const USAGE = {
  input: 42,
  output: 9,
  cacheRead: 8,
  cacheCreation: 4,
}
