import { AnthropicMessagesAdapter } from "../src/providers/anthropic-adapter.js"
import { resolveProviderRuntime } from "../src/providers/catalog.js"
import type { CanonicalAdapterInput } from "../src/providers/content-normalization.js"
import type { LLMProvider, RenderedContext } from "../src/types.js"

function input(extensions: Record<string, unknown> = {}): CanonicalAdapterInput {
  return {
    context: {
      systemText: "stable\n\nknowledge",
      systemStable: "stable",
      systemKnowledge: "knowledge",
      turns: [
        { role: "user", blocks: [{ type: "text", text: "hi" }] },
        {
          role: "assistant",
          blocks: [{ type: "text", text: "checking" }],
          toolCalls: [{ id: "call_1", name: "lookup", arguments: '{"q":"x"}' }],
          providerReplay: {
            native_blocks: [
              { type: "thinking", thinking: "plan", signature: "sig" },
              { type: "text", text: "checking" },
              { type: "tool_use", id: "call_1", name: "lookup", input: { q: "x" } },
            ],
          },
        },
        {
          role: "tool",
          blocks: [{
            type: "tool_result",
            callId: "call_1",
            blocks: [{ type: "text", text: "ok" }],
            isError: false,
          }],
        },
      ],
      frozenPrefixLen: 1,
    },
    tools: [{
      name: "lookup",
      description: "Lookup",
      parameters: '{"type":"object","properties":{"q":{"type":"string"}}}',
    }],
    resolved: {
      identity: {
        providerId: "anthropic",
        modelId: "claude-sonnet-4-6",
        endpointId: "anthropic.messages",
        protocol: "anthropic-messages",
      },
    },
    extensions,
  } as unknown as CanonicalAdapterInput
}

describe("SPC-013 A-05 Anthropic Messages ProtocolAdapter", () => {
  it("builds stable and beta transport plans with identical canonical messages", () => {
    const adapter = new AnthropicMessagesAdapter()
    const stable = adapter.buildRequest(input())
    const beta = adapter.buildRequest(input({ betas: ["code-execution-2025-08-25"] }))

    expect(stable.transport).toBe("stable")
    expect(beta.transport).toBe("beta")
    expect(beta.params.betas).toEqual(["code-execution-2025-08-25"])
    expect(beta.params.messages).toEqual(stable.params.messages)
    expect(stable.params.messages).toEqual([
      {
        role: "user",
        content: [{ type: "text", text: "hi", cache_control: { type: "ephemeral" } }],
      },
      {
        role: "assistant",
        content: [
          { type: "thinking", thinking: "plan", signature: "sig" },
          { type: "text", text: "checking" },
          { type: "tool_use", id: "call_1", name: "lookup", input: { q: "x" } },
        ],
      },
      {
        role: "user",
        content: [{
          type: "tool_result",
          tool_use_id: "call_1",
          content: "ok",
          is_error: false,
          cache_control: { type: "ephemeral" },
        }],
      },
    ])
    expect(stable.params.system).toEqual([
      { type: "text", text: "stable", cache_control: { type: "ephemeral" } },
      { type: "text", text: "knowledge", cache_control: { type: "ephemeral" } },
    ])
  })

  it("preserves explicit one-block content shape without restoring dual content fields", () => {
    const adapter = new AnthropicMessagesAdapter()
    const canonical = input()
    const legacy = adapter.buildRequest({
      ...canonical,
      context: {
        ...canonical.context,
        turns: [{
          role: "tool",
          blocks: [{
            type: "tool_result",
            callId: "call_1",
            blocks: [{ type: "text", text: "ok" }],
            isError: false,
            contentForm: "legacy_text",
          }],
        }],
      },
    })
    const structured = adapter.buildRequest({
      ...canonical,
      context: {
        ...canonical.context,
        turns: [{
          role: "tool",
          blocks: [{
            type: "tool_result",
            callId: "call_1",
            blocks: [{ type: "text", text: "ok" }],
            isError: false,
            contentForm: "blocks",
          }],
        }],
      },
    })
    expect((legacy.params.messages as any[])[0].content[0].content).toBe("ok")
    expect((structured.params.messages as any[])[0].content[0].content).toEqual([
      { type: "text", text: "ok" },
    ])
  })

  it("preserves legacy replay and empty tool-turn eligibility rules", () => {
    const adapter = new AnthropicMessagesAdapter()
    const canonical = input()
    const plan = adapter.buildRequest({
      ...canonical,
      context: {
        ...canonical.context,
        turns: [
          {
            role: "assistant",
            blocks: [{ type: "text", text: "visible" }],
            providerReplay: {
              native_blocks: [{ type: "thinking", thinking: "hidden", signature: "sig" }],
            },
          },
          { role: "tool", blocks: [{ type: "text", text: "legacy orphan" }] },
        ],
      },
    })
    expect(plan.params.messages).toEqual([
      {
        role: "assistant",
        content: [{ type: "text", text: "visible", cache_control: { type: "ephemeral" } }],
      },
    ])
  })

  it.each([
    ["deepseek/deepseek-chat", "deepseek", "deepseek-chat"],
    ["kimi/kimi-k2.6", "kimi", "kimi-k2.6"],
    ["qwen/qwen3.6-plus", "qwen", "qwen3.6-plus"],
    ["glm/glm-5.2", "glm", "glm-5.2"],
    ["minimax/MiniMax-M3", "minimax", "MiniMax-M3"],
  ])("binds %s to its CN endpoint without inheriting the official native meter", async (
    model,
    providerId,
    wireModel,
  ) => {
    const resolved = resolveProviderRuntime({ model, apiKey: "k" })
    expect(resolved.identity).toMatchObject({
      providerId,
      modelId: wireModel,
      protocol: "anthropic-messages",
    })
    expect(resolved.effectiveCapabilities.nativeTokenCounting.state).toBe("unknown")

    const provider = resolved.adapter as LLMProvider
    const captured: { request?: Record<string, unknown> } = {}
    const client = (provider as unknown as {
      client: {
        messages: {
          create: (request: Record<string, unknown>) => Promise<Record<string, unknown>>
          countTokens: () => Promise<unknown>
        }
      }
    }).client
    client.messages.create = async request => {
      captured.request = request
      return {
        content: [{ type: "text", text: "ok" }],
        usage: { input_tokens: 1, output_tokens: 1 },
      }
    }
    client.messages.countTokens = async () => {
      throw new Error("compatible endpoint native meter must not be called")
    }
    const context: RenderedContext = {
      systemText: "system",
      turns: [{ role: "user", content: "hello" }],
    }
    await expect(provider.complete(context, [])).resolves.toMatchObject({ content: "ok" })
    expect(captured.request?.model).toBe(wireModel)
    await expect(provider.countTokens?.(context, [])).rejects.toThrow(/unavailable/)
  })

  it("decodes complete output and returns native replay", () => {
    const adapter = new AnthropicMessagesAdapter()
    expect(adapter.decodeComplete({
      content: [
        { type: "thinking", thinking: "plan", signature: "sig" },
        { type: "text", text: "done" },
        { type: "tool_use", id: "call_2", name: "lookup", input: { q: "y" } },
      ],
      usage: { input_tokens: 10, output_tokens: 4 },
    }, { input: input() })).toEqual({
      message: {
        role: "assistant",
        content: "done",
        tokenCount: 4,
        toolCalls: [{
          id: "call_2",
          name: "lookup",
          arguments: '{"q":"y"}',
        }],
      },
      replay: {
        native_blocks: [
          { type: "thinking", thinking: "plan", signature: "sig" },
          { type: "text", text: "done" },
          { type: "tool_use", id: "call_2", name: "lookup", input: { q: "y" } },
        ],
      },
    })
  })

  it("assembles thinking, tool JSON, usage and replay across stream lifecycle", () => {
    const adapter = new AnthropicMessagesAdapter()
    const state = adapter.createStreamState({ input: input() })
    const events = [
      ...adapter.pushStreamChunk({
        type: "message_start",
        message: {
          usage: {
            input_tokens: 10,
            output_tokens: 1,
            cache_read_input_tokens: 5,
            cache_creation_input_tokens: 2,
          },
        },
      }, state).events,
      ...adapter.pushStreamChunk({
        type: "content_block_start",
        index: 0,
        content_block: { type: "thinking", thinking: "", signature: "" },
      }, state).events,
      ...adapter.pushStreamChunk({
        type: "content_block_delta",
        index: 0,
        delta: { type: "thinking_delta", thinking: "plan" },
      }, state).events,
      ...adapter.pushStreamChunk({
        type: "content_block_start",
        index: 1,
        content_block: { type: "tool_use", id: "call_2", name: "lookup", input: {} },
      }, state).events,
      ...adapter.pushStreamChunk({
        type: "content_block_delta",
        index: 1,
        delta: { type: "input_json_delta", partial_json: '{"q":"y"}' },
      }, state).events,
      ...adapter.pushStreamChunk({
        type: "content_block_stop",
        index: 1,
      }, state).events,
      ...adapter.pushStreamChunk({
        type: "message_delta",
        delta: { stop_reason: "tool_use" },
        usage: { output_tokens: 4 },
      }, state).events,
    ]
    expect(events).toContainEqual({ type: "thinking_delta", delta: "plan" })
    expect(events).toContainEqual({
      type: "tool_call",
      id: "call_2",
      name: "lookup",
      arguments: { q: "y" },
    })
    expect(events.at(-1)).toMatchObject({
      type: "usage",
      totalTokens: 21,
      inputTokens: 17,
      outputTokens: 4,
      stopReason: "tool_use",
      providerUsage: {
        inputTokens: 17,
        outputTokens: 4,
        cacheReadInputTokens: 5,
        cacheCreationInputTokens: 2,
      },
    })
    expect(adapter.finishStream(state, undefined)).toEqual({
      events: [],
      replay: {
        native_blocks: [
          { type: "thinking", thinking: "plan", signature: "" },
          { type: "tool_use", id: "call_2", name: "lookup", input: { q: "y" } },
        ],
      },
    })
  })
})
