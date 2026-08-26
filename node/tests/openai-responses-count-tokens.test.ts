import { OpenAIResponsesProvider } from "../src/providers/openai-responses.js"
import type { ProviderRunState, RenderedContext, ToolSchema } from "../src/types.js"

describe("SPC-024 OpenAI Responses native input token count", () => {
  it("counts the same stateful request plan used by create", async () => {
    const provider = new OpenAIResponsesProvider("test", "gpt-5.2")
    const captured: Record<string, unknown>[] = []
    ;(provider as any).client = {
      responses: {
        inputTokens: {
          count: async (body: Record<string, unknown>) => {
            captured.push(body)
            return { object: "response.input_tokens", input_tokens: 321 }
          },
        },
      },
    }
    const context: RenderedContext = {
      systemText: "system",
      turns: [
        { role: "user", content: "covered" },
        { role: "assistant", content: "covered reply" },
        { role: "user", content: "new turn" },
      ],
    }
    const tools: ToolSchema[] = [{
      name: "lookup",
      description: "Lookup",
      parameters: '{"type":"object"}',
    }]
    const state: ProviderRunState = {
      previousResponseId: "resp_1",
      coveredMessageCount: 2,
    }

    const measurement = await provider.countTokens!(context, tools, {
      reasoning: { effort: "medium" },
      text: { format: { type: "text" } },
      tool_choice: "auto",
      max_output_tokens: 500,
      store: false,
    }, state)

    expect(captured).toEqual([{
      model: "gpt-5.2",
      input: [{ role: "user", content: "new turn" }],
      instructions: "system",
      previous_response_id: "resp_1",
      tools: [{
        type: "function",
        name: "lookup",
        description: "Lookup",
        parameters: { type: "object" },
      }],
      reasoning: { effort: "medium" },
      text: { format: { type: "text" } },
      tool_choice: "auto",
    }])
    expect(measurement).toEqual({
      inputTokens: 321,
      source: { kind: "native", provider: "openai" },
      confidence: "exact",
    })
  })

  it("rejects native counting on a custom compatible endpoint", async () => {
    const provider = new OpenAIResponsesProvider(
      "test", "gpt-5.2", undefined, "https://proxy.invalid/v1",
    )
    await expect(provider.countTokens!({ turns: [] }, [])).rejects.toThrow("unavailable")
  })
})
