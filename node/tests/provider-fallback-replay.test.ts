import { AnthropicProvider } from "../src/providers/anthropic.js"
import { DeepSeekProvider } from "../src/providers/deepseek.js"
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
      const anthropic = new AnthropicProvider("k", "claude-sonnet-4-6")
      expect(isReplayCompatibleWithProvider({ protocol: "anthropic-messages" }, anthropic.descriptor?.())).toBe(true)
      expect(isReplayCompatibleWithProvider({ protocol: "openai-chat" }, anthropic.descriptor?.())).toBe(false)
    })

    it("infers legacy protocol from envelope shape", () => {
      const anthropic = new AnthropicProvider("k", "claude-sonnet-4-6")
      const deepseek = new DeepSeekProvider("k")
      // Legacy Anthropic shape (native_blocks) is only compatible with Anthropic.
      expect(isReplayCompatibleWithProvider({ native_blocks: [{ type: "text", text: "x" }] }, anthropic.descriptor?.())).toBe(true)
      expect(isReplayCompatibleWithProvider({ native_blocks: [{ type: "text", text: "x" }] }, deepseek.descriptor?.())).toBe(false)
      // Legacy OpenAI-compatible shape (reasoning_content) is only compatible with openai-chat.
      expect(isReplayCompatibleWithProvider({ reasoning_content: "t" }, deepseek.descriptor?.())).toBe(true)
      expect(isReplayCompatibleWithProvider({ reasoning_content: "t" }, anthropic.descriptor?.())).toBe(false)
    })

    it("allows unknown shapes through (no descriptor or unknown protocol)", () => {
      const anthropic = new AnthropicProvider("k", "claude-sonnet-4-6")
      expect(isReplayCompatibleWithProvider({ reasoning_content: "t" }, undefined)).toBe(true)
      expect(isReplayCompatibleWithProvider({}, anthropic.descriptor?.())).toBe(true)
    })
  })

  it("does not seed a DeepSeek (openai-chat) replay into an Anthropic provider", () => {
    const anthropic = new AnthropicProvider("k", "claude-sonnet-4-6")
    const message = { content: "calling", toolCalls: [{ id: "c1", name: "ping", arguments: "{}" }] }
    seedProviderReplayFromEvents(anthropic, [llmCompleted({
      content: message.content,
      tool_calls: message.toolCalls,
      provider_replay: { schema_version: 2, provider: "deepseek", protocol: "openai-chat", reasoning_content: "thinking" },
    })])
    // The DeepSeek envelope is dropped; Anthropic falls back to neutral
    // reconstruction (text + tool_use), never the openai-chat reasoning_content.
    const replay = anthropic.peekProviderReplay?.(message)
    expect(replay?.native_blocks).toBeUndefined()
    expect((replay as { reasoning_content?: unknown })?.reasoning_content).toBeUndefined()
  })

  it("reconstructs Anthropic native_blocks for a legacy tool-use log with no persisted replay", () => {
    const anthropic = new AnthropicProvider("k", "claude-sonnet-4-6")
    const message = { content: "calling", toolCalls: [{ id: "c1", name: "ping", arguments: '{"a":1}' }] }
    seedProviderReplayFromEvents(anthropic, [llmCompleted({
      content: message.content,
      tool_calls: message.toolCalls,
    })])
    const replay = anthropic.peekProviderReplay?.(message)
    expect(replay?.native_blocks).toEqual([
      { type: "text", text: "calling" },
      { type: "tool_use", id: "c1", name: "ping", input: { a: 1 } },
    ])
  })

  it("seeds a matching-protocol DeepSeek replay into a DeepSeek provider", () => {
    const deepseek = new DeepSeekProvider("k")
    const message = { content: "calling", toolCalls: [{ id: "c1", name: "ping", arguments: "{}" }] }
    seedProviderReplayFromEvents(deepseek, [llmCompleted({
      content: message.content,
      tool_calls: message.toolCalls,
      provider_replay: { schema_version: 2, provider: "deepseek", protocol: "openai-chat", reasoning_content: "thinking" },
    })])
    expect(deepseek.peekProviderReplay?.(message)?.reasoning_content).toBe("thinking")
  })
})
