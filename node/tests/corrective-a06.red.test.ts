import { OpenAIResponsesAdapter } from "../src/providers/openai-responses.js"
import type { CanonicalAdapterInput } from "../src/providers/content-normalization.js"
import type { OpenAIResponsesRunState } from "../src/providers/openai-responses.js"

function input(extensions: Record<string, unknown> = {}): CanonicalAdapterInput {
  return {
    context: {
      systemText: "system rules",
      turns: [
        { role: "user", blocks: [{ type: "text", text: "Find weather" }], contentForm: "text" },
        {
          role: "assistant",
          blocks: [{ type: "text", text: "" }],
          contentForm: "text",
          toolCalls: [{ id: "call_1", name: "lookup", arguments: '{"city":"Shanghai"}' }],
        },
        {
          role: "tool",
          blocks: [{
            type: "tool_result",
            callId: "call_1",
            blocks: [{ type: "text", text: "sunny" }],
            isError: false,
            contentForm: "text",
          }],
          contentForm: "blocks",
        },
      ],
    },
    tools: [{
      name: "lookup",
      description: "Lookup",
      parameters: '{"type":"object","properties":{"city":{"type":"string"}}}',
    }],
    resolved: {
      identity: {
        providerId: "openai",
        modelId: "gpt-4.1",
        endpointId: "openai.responses",
        protocol: "openai-responses",
      },
    },
    extensions,
  } as unknown as CanonicalAdapterInput
}

describe("SPC-013 A-06 OpenAI Responses ProtocolAdapter", () => {
  it("builds a continuation request from only the uncovered tail and merges built-in tools", () => {
    const adapter = new OpenAIResponsesAdapter()
    const runState: OpenAIResponsesRunState = {
      previousResponseId: "resp_1",
      coveredMessageCount: 2,
    }
    const plan = adapter.buildRequest(input({
      web_search: { search_context_size: "low" },
      builtin_tools: [{ type: "file_search", vector_store_ids: ["vs_1"] }],
      temperature: 0.2,
      previous_response_id: "caller_must_not_override",
    }), runState)

    expect(plan.params).toEqual({
      temperature: 0.2,
      model: "gpt-4.1",
      input: [{ type: "function_call_output", call_id: "call_1", output: "sunny" }],
      instructions: "system rules",
      previous_response_id: "resp_1",
      tools: [
        {
          type: "function",
          name: "lookup",
          description: "Lookup",
          parameters: { type: "object", properties: { city: { type: "string" } } },
        },
        { type: "web_search", search_context_size: "low" },
        { type: "file_search", vector_store_ids: ["vs_1"] },
      ],
    })
    expect(runState).toEqual({ previousResponseId: "resp_1", coveredMessageCount: 2 })
  })

  it("decodes complete output and validates usage through the shared adapter contract", () => {
    const adapter = new OpenAIResponsesAdapter()
    expect(adapter.decodeComplete({
      id: "resp_complete",
      output: [
        { type: "message", content: [{ type: "output_text", text: "done" }] },
        { type: "function_call", call_id: "call_2", name: "lookup", arguments: '{"city":"Paris"}' },
      ],
      usage: { input_tokens: 10, output_tokens: 4, total_tokens: 14 },
    }, { input: input() })).toEqual({
      message: {
        role: "assistant",
        content: "done",
        toolCalls: [{ id: "call_2", name: "lookup", arguments: '{"city":"Paris"}' }],
        tokenCount: 4,
      },
    })
  })

  it("returns continuation as an operation patch without mutating caller or adapter singleton state", () => {
    const adapter = new OpenAIResponsesAdapter()
    const original: OpenAIResponsesRunState = { coveredMessageCount: 0 }
    const state = adapter.createStreamState({ input: input() }, original)
    const outputs = [
      adapter.pushStreamChunk({ type: "response.output_text.delta", delta: "Checking." }, state),
      adapter.pushStreamChunk({
        type: "response.output_item.added",
        output_index: 0,
        item: { type: "function_call", call_id: "call_2", name: "lookup", arguments: "" },
      }, state),
      adapter.pushStreamChunk({
        type: "response.function_call_arguments.delta",
        output_index: 0,
        delta: '{"city":"Paris"}',
      }, state),
      adapter.pushStreamChunk({
        type: "response.output_item.done",
        output_index: 0,
        item: { type: "function_call", call_id: "call_2", name: "lookup", arguments: '{"city":"Paris"}' },
      }, state),
      adapter.pushStreamChunk({
        type: "response.completed",
        response: {
          id: "resp_2",
          usage: {
            input_tokens: 10,
            output_tokens: 4,
            total_tokens: 14,
            input_tokens_details: { cached_tokens: 3 },
          },
        },
      }, state),
    ]

    expect(outputs.flatMap(output => output.events)).toEqual([
      { type: "text_delta", delta: "Checking." },
      { type: "tool_call", id: "call_2", name: "lookup", arguments: { city: "Paris" } },
      {
        type: "usage",
        totalTokens: 14,
        inputTokens: 10,
        outputTokens: 4,
        cacheReadInputTokens: 3,
        providerUsage: { inputTokens: 10, outputTokens: 4 },
      },
    ])
    expect(outputs.at(-1)?.runStatePatch).toEqual({
      previousResponseId: "resp_2",
      coveredMessageCount: 4,
    })
    expect(adapter.finishStream(state, undefined)).toEqual({ events: [] })
    expect(original).toEqual({ coveredMessageCount: 0 })

    const fresh = adapter.createStreamState({ input: input() }, { coveredMessageCount: 0 })
    expect(fresh).not.toBe(state)
  })
})
