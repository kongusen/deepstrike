import { Agent, type AgentOptions } from "../src/agent.js"
import type { LLMProvider, ModelMessage, RenderedContext, StreamEvent } from "../src/types.js"

class FakeProvider implements LLMProvider {
  async complete(_context: RenderedContext, _tools: never[]): Promise<ModelMessage> {
    return { role: "assistant", content: "ok" }
  }
  async *stream(): AsyncIterable<StreamEvent> {
    yield { type: "text_delta", delta: "ok" }
    yield { type: "done", iterations: 1, totalTokens: 3, status: "completed" }
  }
}

test("Agent facade keeps a stable session and returns structured run results", async () => {
  const options: AgentOptions = { name: "wasm-agent", runtimeBinding: { provider: new FakeProvider() } }
  const agent = new Agent(options)
  expect(agent.session("s")).toBe(agent.session("s"))
  const result = await agent.run("hello", { sessionId: "s" })
  expect(result.output).toBe("ok")
  expect(result.status).toBe("completed")
  expect(result.sessionId).toBe("s")
  expect(result.runId).toBeTruthy()
})

test("Agent facade validates structured output at the public boundary", async () => {
  const agent = new Agent({
    name: "structured",
    outputSchema: { type: "object", required: ["answer"] },
    runtimeBinding: { provider: new FakeProvider() },
  })
  const result = await agent.run("hello", { sessionId: "structured" })
  expect(result.outputValidation?.ok).toBe(false)
  expect(result.outputValidation?.errors[0]).toContain("expected object")
})
