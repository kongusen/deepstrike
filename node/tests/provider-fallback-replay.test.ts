import { AnthropicProvider } from "../src/providers/anthropic.js"
import { deepseek } from "../src/providers/factories.js"
import {
  isReplayCompatibleWithProvider,
  seedProviderReplayFromEvents,
} from "../src/runtime/provider-replay.js"
import type { SessionEvent } from "../src/runtime/session-log.js"

function llmCompleted(event: Partial<Extract<SessionEvent, { kind: "llm_completed" }>>): { event: SessionEvent } {
  return {
    event: {
      kind: "llm_completed",
      turn: 0,
      content: "",
      tool_calls: [],
      ...event,
    } as Extract<SessionEvent, { kind: "llm_completed" }>,
  }
}

describe("provider fallback replay", () => {
  describe("isReplayCompatibleWithProvider", () => {
    it("matches explicit protocol against the provider descriptor", () => {
      const anthropic = new AnthropicProvider({ apiKey: "k", model: "claude-sonnet-4-6" })
      expect(isReplayCompatibleWithProvider({ protocol: "anthropic-messages" }, anthropic.descriptor?.())).toBe(true)
      expect(isReplayCompatibleWithProvider({ protocol: "openai-chat" }, anthropic.descriptor?.())).toBe(false)
    })

    it("fails closed on replay without an explicit protocol", () => {
      const anthropic = new AnthropicProvider({ apiKey: "k", model: "claude-sonnet-4-6" })
      const deepseekProvider = deepseek({ apiKey: "k" })
      expect(() => isReplayCompatibleWithProvider({ native_blocks: [{ type: "text", text: "x" }] }, anthropic.descriptor?.())).toThrow(/protocol is required/)
      expect(() => isReplayCompatibleWithProvider({ native_blocks: [{ type: "text", text: "x" }] }, deepseekProvider.descriptor?.())).toThrow(/protocol is required/)
      expect(() => isReplayCompatibleWithProvider({ reasoning_content: "t" }, deepseekProvider.descriptor?.())).toThrow(/protocol is required/)
      expect(() => isReplayCompatibleWithProvider({ reasoning_content: "t" }, anthropic.descriptor?.())).toThrow(/protocol is required/)
      expect(() => isReplayCompatibleWithProvider({ protocol: "openai-chat", schema_version: 1 } as never, deepseekProvider.descriptor?.())).toThrow(/unknown field schema_version/)
    })

    it("accepts an explicit protocol when no descriptor is available", () => {
      const anthropic = new AnthropicProvider({ apiKey: "k", model: "claude-sonnet-4-6" })
      expect(isReplayCompatibleWithProvider({ protocol: "openai-chat", reasoning_content: "t" }, undefined)).toBe(true)
      expect(() => isReplayCompatibleWithProvider({}, anthropic.descriptor?.())).toThrow(/protocol is required/)
    })
  })

  it("does not seed a DeepSeek (openai-chat) replay into an Anthropic provider", () => {
    const anthropic = new AnthropicProvider({ apiKey: "k", model: "claude-sonnet-4-6" })
    const message = { content: "calling", toolCalls: [{ id: "c1", name: "ping", arguments: "{}" }] }
    seedProviderReplayFromEvents(anthropic, [llmCompleted({
      content: message.content,
      tool_calls: message.toolCalls,
      provider_replay: { provider: "deepseek", protocol: "openai-chat", reasoning_content: "thinking" },
    })])
    // The incompatible envelope is dropped without reconstruction.
    const replay = anthropic.peekProviderReplay?.(message)
    expect(replay?.native_blocks).toBeUndefined()
    expect((replay as { reasoning_content?: unknown })?.reasoning_content).toBeUndefined()
  })

  it("does not reconstruct replay when no canonical envelope was persisted", () => {
    const anthropic = new AnthropicProvider({ apiKey: "k", model: "claude-sonnet-4-6" })
    const message = { content: "calling", toolCalls: [{ id: "c1", name: "ping", arguments: '{"a":1}' }] }
    seedProviderReplayFromEvents(anthropic, [llmCompleted({
      content: message.content,
      tool_calls: message.toolCalls,
    })])
    const replay = anthropic.peekProviderReplay?.(message)
    expect(replay).toBeUndefined()
  })

  it("seeds a matching-protocol DeepSeek replay into a DeepSeek provider", () => {
    const deepseekProvider = deepseek({ apiKey: "k" })
    const message = { content: "calling", toolCalls: [{ id: "c1", name: "ping", arguments: "{}" }] }
    seedProviderReplayFromEvents(deepseekProvider, [llmCompleted({
      content: message.content,
      tool_calls: message.toolCalls,
      provider_replay: { provider: "deepseek", protocol: "openai-chat", reasoning_content: "thinking" },
    })])
    expect(deepseekProvider.peekProviderReplay?.(message)?.reasoning_content).toBe("thinking")
  })
})
