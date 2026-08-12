import { RuntimeRunner } from "../src/runtime/runner.js"
import { InMemorySessionLog } from "../src/runtime/session-log.js"
import type { ExecutionPlane } from "../src/runtime/execution-plane.js"
import { toAnthropicMessages } from "../src/providers/base.js"
import {
  attachToolOutputOverlay,
  ContentValidationError,
  normalizeCanonicalAdapterInput,
} from "../src/providers/content-normalization.js"
import { resolveEffectiveModelCapabilities } from "../src/providers/model-registry.js"
import { resolveProviderRuntime } from "../src/providers/catalog.js"
import type {
  LLMProvider, Message, RenderedContext, StreamEvent, ToolCall, ToolOutputBlock, ToolResultEvent, ToolSchema,
} from "../src/types.js"

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
        content: this.executions === 1 ? "first\n[image]" : "second text only",
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
          ? ([{ type: "tool_result", callId: "nested", content: [{ type: "text", text: "secret" }], isError: false }] as unknown as ToolOutputBlock[])
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

function imageToolContext(): RenderedContext {
  return {
    systemText: "",
    turns: [{
      role: "tool",
      content: "[image]",
      contentParts: [{
        type: "tool_result",
        callId: "call_1",
        output: "[image]",
        isError: false,
        contentParts: [{
          type: "image",
          source: { kind: "base64", data: "aW1hZ2U=" },
          mediaType: "image/png",
        }],
      }],
    }],
  }
}

describe("spc_013-A-02: canonical content and operation overlay", () => {
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
    const staleBlocks: ToolOutputBlock[] = [
      { type: "text" as const, text: "old process-local fact" },
      { type: "image" as const, source: { kind: "base64" as const, data: "b2xk" }, mediaType: "image/png" },
    ]
    const restored = restoredTextProjection()
    expect(reused).not.toHaveProperty("structuredToolOutputs")
    expect(fresh).not.toHaveProperty("structuredToolOutputs")
    expect(attachToolOutputOverlay(restored, new Map()))
      .toEqual(attachToolOutputOverlay(restored, new Map()))
    expect(attachToolOutputOverlay(restored, new Map([["call_1", staleBlocks]])))
      .not.toEqual(restored)
  })

  it("rejects conflicting output and structured content instead of choosing one silently", () => {
    expect(() => toAnthropicMessages(structuredToolContext().turns)).toThrow(/projection|conflict/i)
  })

  it("rejects nested ToolResult blocks", () => {
    expect(() => toAnthropicMessages(structuredToolContext(true).turns)).toThrow(/nested|tool.result/i)
  })

  it("applies modality preflight recursively to images inside tool output", () => {
    const effectiveCapabilities = resolveEffectiveModelCapabilities({
      model: {
        id: "test/text-only",
        providerId: "test",
        kind: "generation",
        intrinsic: { inputModalities: ["text"] },
      },
      protocol: "openai-chat",
    })
    const base = resolveProviderRuntime({ model: "openai/gpt-4o", apiKey: "k" })
    const resolved = { ...base, effectiveCapabilities }
    expect(() => normalizeCanonicalAdapterInput({
      context: imageToolContext(),
      tools: [],
      resolved,
    })).toThrow(ContentValidationError)
  })

  it("rejects invalid media sources before an adapter sees them", () => {
    const context = imageToolContext()
    const tool = context.turns[0].contentParts?.[0]
    if (tool?.type !== "tool_result" || tool.contentParts?.[0]?.type !== "image") {
      throw new Error("invalid test fixture")
    }
    tool.contentParts[0].source = { kind: "base64", data: "***not-base64***" }
    const resolved = resolveProviderRuntime({ model: "openai/gpt-4o", apiKey: "k" })
    expect(() => normalizeCanonicalAdapterInput({ context, tools: [], resolved }))
      .toThrow(/valid base64/i)
  })

  it("rejects provider files when endpoint affinity does not match", () => {
    const context: RenderedContext = {
      systemText: "",
      turns: [{
        role: "tool",
        content: "[file]",
        contentParts: [{
          type: "tool_result",
          callId: "call_file",
          output: "[file]",
          isError: false,
          contentParts: [{
            type: "file",
            source: {
              kind: "fileId",
              id: "file_1",
              affinity: { providerId: "openai", endpointId: "openai.responses" },
            },
          }],
        }],
      }],
    }
    const resolved = resolveProviderRuntime({
      model: "openai/gpt-5.5",
      endpoint: "openai.responses",
      apiKey: "k",
    })
    context.turns[0].contentParts![0] = {
      ...(context.turns[0].contentParts![0] as Extract<
        NonNullable<Message["contentParts"]>[number],
        { type: "tool_result" }
      >),
      contentParts: [{
        type: "file",
        source: {
          kind: "fileId",
          id: "file_1",
          affinity: { providerId: "openai", endpointId: "openai.chat" },
        },
      }],
    }
    expect(() => normalizeCanonicalAdapterInput({ context, tools: [], resolved }))
      .toThrow(/belongs to openai\/openai.chat/i)
  })
})
