import { streamingTool, tool } from "../src/tools/index.js"
import type { LLMProvider, Message, RenderedContext, StreamEvent, ToolSchema } from "../src/types.js"
import { createRunner } from "./runtime/helpers.js"

class MultiToolProvider implements LLMProvider {
  private callCount = 0
  async complete(_context: RenderedContext, _tools: ToolSchema[]): Promise<Message> {
    return { role: "assistant", content: "unused", toolCalls: [] }
  }
  async *stream(): AsyncIterable<StreamEvent> {
    this.callCount += 1
    if (this.callCount === 1) {
      yield { type: "tool_call", id: "call_a", name: "alpha", arguments: { value: "A" } }
      yield { type: "tool_call", id: "call_b", name: "beta", arguments: { value: "B" } }
      return
    }
    yield { type: "text_delta", delta: "done" }
  }
}

describe("tool runtime control plane", () => {
  it("rejects invalid tool arguments against the declared JSON schema before execution", async () => {
    let executed = false
    const provider: LLMProvider = {
      async complete(_context, _tools) { return { role: "assistant", content: "unused", toolCalls: [] } },
      async *stream() {
        yield { type: "tool_call", id: "call_invalid", name: "alpha", arguments: {} }
      },
    }
    const { runner } = createRunner(
      provider,
      [tool("alpha", "Alpha", {
        type: "object",
        properties: { value: { type: "string" } },
        required: ["value"],
      }, () => {
        executed = true
        return "ok"
      })],
      { maxTurns: 2 },
    )

    const events = []
    for await (const event of runner.run({ sessionId: "invalid-args", goal: "run tools" })) events.push(event)

    expect(executed).toBe(false)
    expect(events).toContainEqual(expect.objectContaining({
      type: "tool_result",
      callId: "call_invalid",
      name: "alpha",
      isError: true,
    }))
  })

  it("supports generic suspend and resume hooks without baking in UI semantics", async () => {
    let callCount = 0
    const provider: LLMProvider = {
      async complete(_context, _tools) { return { role: "assistant", content: "unused", toolCalls: [] } },
      async *stream() {
        callCount += 1
        if (callCount === 1) yield { type: "tool_call", id: "call_suspend", name: "await_external", arguments: {} }
        else yield { type: "text_delta", delta: "done" }
      },
    }
    const { runner } = createRunner(
      provider,
      [streamingTool("await_external", "Await external input", { type: "object", properties: {} }, async function* () {
        const resumed = yield { type: "suspend", suspensionId: "ticket_1", payload: { source: "host" } }
        yield { type: "text", text: String(resumed) }
      })],
      {
        maxTurns: 2,
        onToolSuspend: async request => `resumed:${request.suspensionId}`,
      },
    )

    const events = []
    for await (const event of runner.run({ sessionId: "suspend", goal: "wait once" })) events.push(event)

    expect(events).toContainEqual({
      type: "tool_suspend",
      callId: "call_suspend",
      name: "await_external",
      suspensionId: "ticket_1",
      payload: { source: "host" },
    })
    expect(events).toContainEqual({
      type: "tool_result",
      callId: "call_suspend",
      name: "await_external",
      content: "resumed:ticket_1",
      isError: false,
    })
  })

  it("multiplexes concurrent tool streams by call id and emits structured chunks", async () => {
    const { runner } = createRunner(
      new MultiToolProvider(),
      [
        streamingTool("alpha", "Alpha", { type: "object", properties: { value: { type: "string" } }, required: ["value"] }, async function* () {
          yield { type: "progress", progress: 0.5, message: "alpha half" }
          await new Promise(r => setTimeout(r, 5))
          yield { type: "text", text: "A" }
        }),
        streamingTool("beta", "Beta", { type: "object", properties: { value: { type: "string" } }, required: ["value"] }, async function* () {
          yield { type: "artifact", artifactId: "artifact_1", mimeType: "text/plain" }
          yield { type: "text", text: "B" }
        }),
      ],
      { maxTurns: 4 },
    )

    const events = []
    for await (const event of runner.run({ sessionId: "multi", goal: "run tools" })) events.push(event)

    expect(events).toContainEqual({ type: "tool_delta", callId: "call_a", name: "alpha", chunk: { type: "progress", progress: 0.5, message: "alpha half" } })
    expect(events).toContainEqual({ type: "tool_delta", callId: "call_b", name: "beta", chunk: { type: "artifact", artifactId: "artifact_1", mimeType: "text/plain" } })
    expect(events).toContainEqual({ type: "tool_result", callId: "call_a", name: "alpha", content: "A", isError: false })
    expect(events).toContainEqual({ type: "tool_result", callId: "call_b", name: "beta", content: "B", isError: false })
  })
})
