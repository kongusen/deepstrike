import { AnthropicProvider } from "../src/providers/anthropic.js"
import { deepseek } from "../src/providers/factories.js"
import {
  ProviderReplayProtocolMismatchError,
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

  it("fails fast with an endpoint-pinning diagnostic for cross-protocol tool replay", () => {
    const deepseekProvider = deepseek({ apiKey: "k" })
    const message = { content: "calling", toolCalls: [{ id: "c1", name: "ping", arguments: "{}" }] }
    expect(() => seedProviderReplayFromEvents(deepseekProvider, [llmCompleted({
      content: message.content,
      tool_calls: message.toolCalls,
      wire_evidence: { protocol: "anthropic-messages", request_fingerprint: "fp", replay_state: { protocol: "anthropic-messages", native_blocks: [{ type: "thinking", thinking: "secret-reasoning" }] } },
    })])).toThrow(ProviderReplayProtocolMismatchError)
    expect(() => seedProviderReplayFromEvents(deepseekProvider, [llmCompleted({
      content: message.content,
      tool_calls: message.toolCalls,
      wire_evidence: { protocol: "anthropic-messages", request_fingerprint: "fp", replay_state: { protocol: "anthropic-messages", native_blocks: [{ type: "thinking", thinking: "secret-reasoning" }] } },
    })])).toThrow(/pin the previous anthropic-messages endpoint explicitly/)

    try {
      seedProviderReplayFromEvents(deepseekProvider, [llmCompleted({
        content: message.content,
        tool_calls: message.toolCalls,
        wire_evidence: { protocol: "anthropic-messages", request_fingerprint: "fp", replay_state: { protocol: "anthropic-messages", native_blocks: [{ type: "thinking", thinking: "secret-reasoning" }] } },
      })])
    } catch (error) {
      expect(error).toMatchObject({ code: "provider_replay_protocol_mismatch" })
      expect(String(error)).not.toContain("secret-reasoning")
      expect(String(error)).not.toContain("ping")
    }

    // The incompatible envelope is never seeded before the diagnostic is raised.
    const replay = deepseekProvider.peekProviderReplay?.(message)
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
      wire_evidence: { protocol: "openai-chat", request_fingerprint: "fp", replay_state: { provider: "deepseek", protocol: "openai-chat", reasoning_content: "thinking" } },
    })])
    expect(deepseekProvider.peekProviderReplay?.(message)?.reasoning_content).toBe("thinking")
  })
})
