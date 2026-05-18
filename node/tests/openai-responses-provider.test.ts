import { OpenAIResponsesProvider } from "../src/providers/openai-responses.js"
import type { RenderedContext } from "../src/types.js"

describe("OpenAIResponsesProvider", () => {
  it("continues with previous_response_id and only sends the uncovered tail", async () => {
    const provider = new OpenAIResponsesProvider("test-key")
    const state = provider.createRunState()
    const requests: Array<Record<string, unknown>> = []
    let callCount = 0

    ;(provider as unknown as {
      client: {
        responses: {
          create(req: Record<string, unknown>): Promise<AsyncIterable<Record<string, unknown>>>
        }
      }
    }).client = {
      responses: {
        async create(req) {
          requests.push(req)
          callCount += 1
          const currentCall = callCount
          return {
            async *[Symbol.asyncIterator]() {
              if (currentCall === 1) {
                yield {
                  type: "response.output_item.added",
                  output_index: 0,
                  item: {
                    type: "function_call",
                    call_id: "call_1",
                    name: "lookup",
                    arguments: "",
                  },
                }
                yield {
                  type: "response.function_call_arguments.done",
                  output_index: 0,
                  arguments: '{"city":"Shanghai"}',
                }
                yield {
                  type: "response.output_item.done",
                  output_index: 0,
                  item: {
                    type: "function_call",
                    call_id: "call_1",
                    name: "lookup",
                    arguments: '{"city":"Shanghai"}',
                  },
                }
                yield {
                  type: "response.completed",
                  response: { id: "resp_1", usage: { total_tokens: 12 } },
                }
                return
              }

              yield { type: "response.output_text.delta", delta: "done" }
              yield {
                type: "response.completed",
                response: { id: "resp_2", usage: { total_tokens: 20 } },
              }
            },
          }
        },
      },
    }

    const firstContext: RenderedContext = {
      systemText: "system rules",
      turns: [{ role: "user", content: "Find weather" }],
    }
    const firstEvents = []
    for await (const event of provider.stream(firstContext, [], undefined, state)) firstEvents.push(event)

    const secondContext: RenderedContext = {
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
    const secondEvents = []
    for await (const event of provider.stream(secondContext, [], undefined, state)) secondEvents.push(event)

    expect(firstEvents).toEqual([
      { type: "tool_call", id: "call_1", name: "lookup", arguments: { city: "Shanghai" } },
      { type: "usage", totalTokens: 12 },
    ])
    expect(requests[0]).toMatchObject({
      model: "gpt-4.1",
      instructions: "system rules",
      input: [{ role: "user", content: "Find weather" }],
    })
    expect(requests[1]).toMatchObject({
      model: "gpt-4.1",
      instructions: "system rules",
      previous_response_id: "resp_1",
      input: [{ type: "function_call_output", call_id: "call_1", output: "sunny" }],
    })
    expect(secondEvents).toEqual([
      { type: "text_delta", delta: "done" },
      { type: "usage", totalTokens: 20 },
    ])
  })
})
