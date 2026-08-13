/** Legacy ToolResult carriers remain accepted, then normalize to canonical blocks at the boundary. */
import type { ContentBlock, Message, ToolResult, ToolResultPart } from "../src/types.js"
import { mcpResultToToolOutput } from "../src/runtime/mcp-proxy-plane.js"
import { toAnthropicMessages } from "../src/providers/base.js"

describe("structured tool result field (spc_012-N-01)", () => {
  it("ToolResultPart accepts an optional contentParts alongside the still-required output", () => {
    const blocks: ContentBlock[] = [
      { type: "text", text: "sunny" },
      { type: "image", source: { kind: "url", url: "https://example.com/screenshot.png" }, mediaType: "image/png" },
    ]
    const part: ToolResultPart = {
      type: "tool_result",
      callId: "call_1",
      output: "sunny\n[image]",
      isError: false,
      contentParts: blocks,
    }
    expect(part.output).toBe("sunny\n[image]")
    expect(part.contentParts).toHaveLength(2)
  })

  it("ToolResultPart without contentParts still constructs (backward compatible)", () => {
    const part: ToolResultPart = {
      type: "tool_result",
      callId: "call_2",
      output: "plain text",
      isError: false,
    }
    expect(part.contentParts).toBeUndefined()
  })

  it("ToolResult (the standalone tool-execution result carrier) accepts the same optional field", () => {
    const result: ToolResult = {
      callId: "call_3",
      output: "sunny",
      isError: false,
      contentParts: [{ type: "text", text: "sunny" }],
    }
    expect(result.contentParts).toEqual([{ type: "text", text: "sunny" }])
  })
})

/**
 * spc_012-N-02: regression lock for the silent image-drop bug that lived at
 * `mcp-proxy-plane.ts` — `execute()` used to `.filter(c => c.type === "text")` the MCP
 * `tools/call` response, so an MCP tool returning a screenshot lost the image block with no
 * trace (INV-012-01 violation). These tests pin the fixed behavior: non-text blocks land in
 * `contentParts`, `output` stays the text-only projection.
 */
describe("mcpResultToToolOutput (spc_012-N-02)", () => {
  it("preserves an image block in contentParts instead of silently dropping it", () => {
    const out = mcpResultToToolOutput({
      content: [
        { type: "text", text: "here is the screenshot" },
        { type: "image", data: "aGVsbG8=", mimeType: "image/png" },
      ],
      isError: false,
    })
    expect(out.output).toBe("here is the screenshot\n[image]")
    expect(out.isError).toBe(false)
    expect(out.contentParts).toEqual([
      { type: "text", text: "here is the screenshot" },
      { type: "image", source: { kind: "base64", data: "aGVsbG8=" }, mediaType: "image/png" },
    ])
  })

  it("maps an audio block into contentParts with a defaulted mediaType", () => {
    const out = mcpResultToToolOutput({
      content: [{ type: "audio", data: "AAAA" }],
    })
    expect(out.contentParts).toEqual([
      { type: "audio", source: { kind: "base64", data: "AAAA" }, mediaType: "audio/wav" },
    ])
  })

  it("serializes an unrecognized block type as text instead of dropping it", () => {
    const weird = { type: "resource", uri: "file:///tmp/x" }
    const out = mcpResultToToolOutput({ content: [weird] })
    expect(out.contentParts).toEqual([{ type: "text", text: JSON.stringify(weird) }])
  })

  it("pure-text responses get no contentParts (zero behavior change for the common path)", () => {
    const out = mcpResultToToolOutput({
      content: [{ type: "text", text: "a" }, { type: "text", text: "b" }],
      isError: true,
    })
    expect(out.output).toBe("a\nb")
    expect(out.isError).toBe(true)
    expect(out.contentParts).toBeUndefined()
  })
})

/**
 * spc_012-N-03: the Anthropic provider's tool_result serialization reads `contentParts` when
 * present — the request body's `tool_result.content` must be a structured block array (the
 * protocol natively supports image blocks inside tool_result), not the flattened text projection.
 */
describe("toAnthropicMessages structured tool_result (spc_012-N-03)", () => {
  const toolMessage = (contentParts?: ContentBlock[]): Message => ({
    role: "tool",
    content: "weather: sunny\n[image]",
    toolCalls: [],
    contentParts: [{
      type: "tool_result",
      callId: "call_1",
      output: "weather: sunny\n[image]",
      isError: false,
      ...(contentParts ? { contentParts } : {}),
    }],
  })

  it("serializes contentParts as structured tool_result content (image preserved)", () => {
    const msgs = toAnthropicMessages([toolMessage([
      { type: "text", text: "weather: sunny" },
      { type: "image", source: { kind: "base64", data: "aGVsbG8=" }, mediaType: "image/png" },
    ])])
    expect(msgs).toEqual([{
      role: "user",
      content: [{
        type: "tool_result",
        tool_use_id: "call_1",
        is_error: false,
        content: [
          { type: "text", text: "weather: sunny" },
          { type: "image", source: { type: "base64", media_type: "image/png", data: "aGVsbG8=" } },
        ],
      }],
    }])
  })

  it("falls back to the output text projection when contentParts is absent (unchanged legacy path)", () => {
    const msgs = toAnthropicMessages([toolMessage()])
    expect(msgs).toEqual([{
      role: "user",
      content: [{ type: "tool_result", tool_use_id: "call_1", content: "weather: sunny\n[image]", is_error: false }],
    }])
  })
})

/**
 * spc_012 end-to-end (N-02 → runner side channel → N-03): a tool_result event carrying
 * structured `contentParts` (an MCP-style screenshot) must survive the kernel round trip — which
 * is text-only by design — via the runner's callId side channel, and arrive at the provider's
 * request serializer still structured.
 */
import { RuntimeRunner } from "../src/runtime/runner.js"
import { InMemorySessionLog } from "../src/runtime/session-log.js"
import type { ExecutionPlane } from "../src/runtime/execution-plane.js"
import type {
  LLMProvider, Message, RenderedContext, StreamEvent, ToolCall, ToolResultEvent, ToolSchema,
} from "../src/types.js"

class MultimodalToolPlane implements ExecutionPlane {
  register(): this { return this }
  unregister(): this { return this }
  schemas(): ToolSchema[] {
    return [{
      name: "screenshot",
      description: "Returns a screenshot image",
      parameters: JSON.stringify({ type: "object", properties: {} }),
    }]
  }

  async *executeAll(calls: ToolCall[]): AsyncIterable<StreamEvent> {
    for (const call of calls) {
      yield {
        type: "tool_result",
        callId: call.id,
        name: call.name,
        content: "screenshot taken\n[image]",
        isError: false,
        contentParts: [
          { type: "text", text: "screenshot taken" },
          { type: "image", source: { kind: "base64", data: "aGVsbG8=" }, mediaType: "image/png" },
        ],
      } as ToolResultEvent
    }
  }
}

class CapturingProvider implements LLMProvider {
  readonly contexts: RenderedContext[] = []
  private callCount = 0

  async complete(): Promise<Message> {
    return { role: "assistant", content: "done", toolCalls: [] }
  }

  async *stream(context: RenderedContext): AsyncIterable<StreamEvent> {
    this.contexts.push(context)
    this.callCount += 1
    if (this.callCount === 1) {
      yield { type: "tool_call", id: "call_1", name: "screenshot", arguments: {} }
      return
    }
    yield { type: "text_delta", delta: "done" }
  }
}

describe("structured tool result end-to-end (spc_012)", () => {
  it("an image block from the execution plane reaches the next provider request structured", async () => {
    const provider = new CapturingProvider()
    const runner = new RuntimeRunner({
      provider,
      sessionLog: new InMemorySessionLog(),
      executionPlane: new MultimodalToolPlane(),
      maxTokens: 4000,
      maxTurns: 4,
      baselineToolIds: ["screenshot"],
    } as never)

    for await (const _evt of runner.run({ sessionId: "spc012-e2e", goal: "Take a screenshot."})) {}

    expect(provider.contexts.length).toBeGreaterThanOrEqual(2)
    const followUp = provider.contexts[1]
    const turns = followUp.stateTurn ? [...followUp.turns, followUp.stateTurn] : followUp.turns
    const toolMsg = turns.find(m => m.role === "tool")
    expect(toolMsg).toBeDefined()
    const part = toolMsg!.contentParts?.find(p => p.type === "tool_result" && p.callId === "call_1")
    expect(part).toBeDefined()
    // Canonical durable blocks survive the kernel round trip.
    expect(part!.contentParts).toEqual([
      { type: "text", text: "screenshot taken" },
      { type: "image", source: { kind: "base64", data: "aGVsbG8=" }, mediaType: "image/png" },
    ])

    // And the Anthropic wire serializer emits the image as a native structured block.
    const wire = toAnthropicMessages([toolMsg!])
    expect(wire).toEqual([{
      role: "user",
      content: [{
        type: "tool_result",
        tool_use_id: "call_1",
        is_error: false,
        content: [
          { type: "text", text: "screenshot taken" },
          { type: "image", source: { type: "base64", media_type: "image/png", data: "aGVsbG8=" } },
        ],
      }],
    }])
  })
})

/**
 * spc_012-N-04: OpenAI Responses natively accepts structured `function_call_output.output`
 * content arrays — image blocks must serialize as `input_image` items, not flatten to text.
 */
import { OpenAIResponsesAdapter } from "../src/providers/openai-responses.js"

describe("OpenAI Responses structured tool_result (spc_012-N-04 native)", () => {
  const adapter = new OpenAIResponsesAdapter()
  const ctx = (contentParts?: ContentBlock[]): RenderedContext => ({
    systemText: "",
    systemStable: "",
    systemKnowledge: "",
    turns: [{
      role: "tool",
      content: "weather: sunny\n[image]",
      toolCalls: [],
      contentParts: [{
        type: "tool_result",
        callId: "call_1",
        output: "weather: sunny\n[image]",
        isError: false,
        ...(contentParts ? { contentParts } : {}),
      }],
    }],
  })

  it("serializes contentParts as a native input_text/input_image array", () => {
    const input = adapter.buildInput(ctx([
      { type: "text", text: "weather: sunny" },
      { type: "image", source: { kind: "base64", data: "aGVsbG8=" }, mediaType: "image/png" },
    ]))
    expect(input).toEqual([{
      type: "function_call_output",
      call_id: "call_1",
      output: [
        { type: "input_text", text: "weather: sunny" },
        { type: "input_image", image_url: "data:image/png;base64,aGVsbG8=" },
      ],
    }])
  })

  it("falls back to the output text projection when contentParts is absent", () => {
    const input = adapter.buildInput(ctx())
    expect(input).toEqual([{
      type: "function_call_output",
      call_id: "call_1",
      output: "weather: sunny\n[image]",
    }])
  })
})
