import { RuntimeRunner } from "../src/runtime/runner.js"
import { InMemorySessionLog } from "../src/runtime/session-log.js"
import type { ExecutionPlane } from "../src/runtime/execution-plane.js"
import { toAnthropicMessages, UnsupportedModalityError } from "../src/providers/base.js"
import { OpenAIChatAdapter } from "../src/providers/openai-chat.js"
import type {
  LLMProvider, Message, RenderedContext, StreamEvent, ToolCall, ToolResultEvent, ToolSchema,
} from "../src/types.js"

const describeA02Red = process.env.RUN_SPC013_A02_RED === "1" ? describe : describe.skip

class ReusedCallIdPlane implements ExecutionPlane {
  private executions = 0
  register(): this { return this }
  unregister(): this { return this }
  schemas(): ToolSchema[] {
    return [{ name: "capture", description: "capture", parameters: '{"type":"object"}' }]
  }
  async *executeAll(calls: ToolCall[]): AsyncIterable<StreamEvent> {
    for (const call of calls) {
      this.executions += 1
      yield {
        type: "tool_result",
        callId: call.id,
        name: call.name,
        content: this.executions === 1 ? "first [image]" : "second text only",
        isError: false,
        ...(this.executions === 1 ? {
          contentParts: [
            { type: "text", text: "first" },
            { type: "image", source: { kind: "base64", data: "Zmlyc3Q=" }, mediaType: "image/png" },
          ],
        } : {}),
      } as ToolResultEvent
    }
  }
}

class TwoSessionProvider implements LLMProvider {
  readonly contexts: RenderedContext[] = []
  private calls = 0
  async complete(): Promise<Message> { return { role: "assistant", content: "done" } }
  async *stream(context: RenderedContext): AsyncIterable<StreamEvent> {
    this.contexts.push(context)
    this.calls += 1
    if (this.calls % 2 === 1) {
      yield { type: "tool_call", id: "call_1", name: "capture", arguments: {} }
    } else {
      yield { type: "text_delta", delta: "done" }
    }
  }
}

function structuredToolContext(nested = false): RenderedContext {
  return {
    systemText: "",
    turns: [{
      role: "assistant",
      content: "",
      toolCalls: [{ id: "call_1", name: "capture", arguments: "{}" }],
    }, {
      role: "tool",
      content: "public projection",
      contentParts: [{
        type: "tool_result",
        callId: "call_1",
        output: "public projection",
        isError: false,
        contentParts: nested
          ? [{ type: "tool_result", callId: "nested", content: [{ type: "text", text: "secret" }], isError: false }]
          : [{ type: "text", text: "different structured fact" }],
      }],
    }],
  }
}

function restoredTextProjection(): RenderedContext {
  return {
    systemText: "",
    turns: [{
      role: "assistant",
      content: "",
      toolCalls: [{ id: "call_1", name: "capture", arguments: "{}" }],
    }, {
      role: "tool",
      content: "persisted text projection only",
      contentParts: [{
        type: "tool_result",
        callId: "call_1",
        output: "persisted text projection only",
        isError: false,
      }],
    }],
  }
}

describeA02Red("spc_013-A-00: reproducible A-02 corrective reds", () => {
  it("does not reuse structured blocks when two sessions share a Runner and call_1", async () => {
    const provider = new TwoSessionProvider()
    const runner = new RuntimeRunner({
      provider,
      sessionLog: new InMemorySessionLog(),
      executionPlane: new ReusedCallIdPlane(),
      maxTokens: 4000,
      maxTurns: 4,
    } as never)

    for await (const _ of runner.run({ sessionId: "first-session", goal: "first" })) {}
    for await (const _ of runner.run({ sessionId: "second-session", goal: "second" })) {}

    const secondFollowUp = provider.contexts[3]
    const turns = secondFollowUp.stateTurn ? [...secondFollowUp.turns, secondFollowUp.stateTurn] : secondFollowUp.turns
    const part = turns
      .find(turn => turn.role === "tool")
      ?.contentParts?.find(item => item.type === "tool_result" && item.callId === "call_1")
    expect(part?.contentParts).toBeUndefined()
  })

  it("gives restored history identical semantics for same-Runner and new-Runner wake", () => {
    const options = {
      provider: new TwoSessionProvider(),
      sessionLog: new InMemorySessionLog(),
      executionPlane: new ReusedCallIdPlane(),
      maxTokens: 4000,
      maxTurns: 4,
    }
    const reused = new RuntimeRunner(options as never)
    const fresh = new RuntimeRunner(options as never)
    const staleBlocks = [
      { type: "text" as const, text: "old process-local fact" },
      { type: "image" as const, source: { kind: "base64" as const, data: "b2xk" }, mediaType: "image/png" },
    ]
    const reusedInternals = reused as unknown as {
      structuredToolOutputs: Map<string, typeof staleBlocks>
      withStructuredToolOutputs(context: RenderedContext): RenderedContext
    }
    const freshInternals = fresh as unknown as {
      withStructuredToolOutputs(context: RenderedContext): RenderedContext
    }
    reusedInternals.structuredToolOutputs.set("call_1", staleBlocks)

    const restored = restoredTextProjection()
    expect(reusedInternals.withStructuredToolOutputs(restored))
      .toEqual(freshInternals.withStructuredToolOutputs(restored))
  })

  it("rejects conflicting output and structured content instead of choosing one silently", () => {
    expect(() => toAnthropicMessages(structuredToolContext().turns)).toThrow(/projection|conflict/i)
  })

  it("rejects nested ToolResult blocks", () => {
    expect(() => toAnthropicMessages(structuredToolContext(true).turns)).toThrow(/nested|tool.result/i)
  })

  it("applies modality preflight recursively to images inside tool output", () => {
    const adapter = new OpenAIChatAdapter()
    expect(() => adapter.buildMessages(structuredToolContext(), {
      descriptor: { provider: "qwen", protocol: "openai-chat", model: "qwen3.7-max-preview" },
    })).toThrow(UnsupportedModalityError)
  })
})
