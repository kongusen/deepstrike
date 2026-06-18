import { subAgentResultToKernel } from "../src/types/agent.js"
import type { SubAgentResult } from "../src/types/agent.js"

function resultWithToolArgs(rawArguments: string): SubAgentResult {
  return {
    agentId: "child-1",
    result: {
      termination: "completed",
      finalMessage: {
        role: "assistant",
        content: "",
        toolCalls: [{ id: "tc1", name: "do_thing", arguments: rawArguments }],
      },
      turnsUsed: 1,
      totalTokensUsed: 10,
    },
  }
}

describe("subAgentResultToKernel — malformed tool-call arguments must not brick", () => {
  it("degrades malformed JSON arguments to {} instead of throwing", () => {
    // A model wrote a truncated/garbled arguments string on its final turn. The
    // OpenAIChat-family non-streaming path passes it through verbatim, so it
    // reaches serialization raw. This must NOT throw.
    const result = resultWithToolArgs('{"path": "/a/b", "conte')
    let out: Record<string, unknown> | undefined
    expect(() => { out = subAgentResultToKernel(result) }).not.toThrow()
    const finalMessage = (out!.result as any).final_message
    expect(finalMessage.tool_calls[0].arguments).toEqual({})
  })

  it("still parses well-formed arguments into an object", () => {
    const result = resultWithToolArgs('{"path":"/a/b","n":3}')
    const out = subAgentResultToKernel(result)
    const finalMessage = (out.result as any).final_message
    expect(finalMessage.tool_calls[0].arguments).toEqual({ path: "/a/b", n: 3 })
  })

  it("handles an empty/absent arguments string", () => {
    const result = resultWithToolArgs("")
    const out = subAgentResultToKernel(result)
    const finalMessage = (out.result as any).final_message
    expect(finalMessage.tool_calls[0].arguments).toEqual({})
  })
})
