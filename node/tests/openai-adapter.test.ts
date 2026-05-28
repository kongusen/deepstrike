import { OpenAIChatAdapter } from "../src/providers/openai-chat.js"

describe("OpenAIChatAdapter", () => {
  it("builds native chat tools", () => {
    const adapter = new OpenAIChatAdapter()
    expect(adapter.buildTools([{
      name: "lookup",
      description: "Lookup",
      parameters: '{"type":"object","properties":{}}',
    }])).toEqual([{
      type: "function",
      function: {
        name: "lookup",
        description: "Lookup",
        parameters: { type: "object", properties: {} },
      },
    }])
  })

  it("normalizes chat tool calls", () => {
    const adapter = new OpenAIChatAdapter()
    expect(adapter.normalizeToolCalls([{
      type: "function",
      id: "call_1",
      function: { name: "lookup", arguments: '{"q":"x"}' },
    }])).toEqual([{ id: "call_1", name: "lookup", arguments: '{"q":"x"}' }])
  })
})
