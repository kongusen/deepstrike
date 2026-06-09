import { AnthropicProvider } from "../src/providers/anthropic.js"
import {
  isReplayCompatibleWithProvider,
  seedProviderReplayFromEvents,
} from "../src/runtime/provider-replay.js"
import { normalizeLlmCompleted } from "../src/runtime/session-repair.js"
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

describe("wasm provider fallback replay (M3 parity)", () => {
  it("normalizeLlmCompleted does not synthesize provider_replay", () => {
    const out = normalizeLlmCompleted({
      kind: "llm_completed",
      turn: 0,
      content: "checking",
      tool_calls: [{ id: "c1", name: "ping", arguments: "{}" }],
    } as Extract<SessionEvent, { kind: "llm_completed" }>)
    expect((out as { provider_replay?: unknown }).provider_replay).toBeUndefined()
  })

  it("isReplayCompatibleWithProvider gates by protocol against the descriptor", () => {
    const anthropic = new AnthropicProvider("k", "claude-sonnet-4-6")
    expect(isReplayCompatibleWithProvider({ protocol: "anthropic-messages" }, anthropic.descriptor?.())).toBe(true)
    expect(isReplayCompatibleWithProvider({ protocol: "openai-chat" }, anthropic.descriptor?.())).toBe(false)
    expect(isReplayCompatibleWithProvider({ reasoning_content: "t" }, anthropic.descriptor?.())).toBe(false)
    expect(isReplayCompatibleWithProvider({ native_blocks: [{ type: "text", text: "x" }] }, anthropic.descriptor?.())).toBe(true)
  })

  it("drops a cross-protocol replay and reconstructs Anthropic blocks for legacy logs", () => {
    const anthropic = new AnthropicProvider("k", "claude-sonnet-4-6")
    const message = { content: "calling", toolCalls: [{ id: "c1", name: "ping", arguments: '{"a":1}' }] }
    // openai-chat envelope is incompatible -> skipped entirely.
    seedProviderReplayFromEvents(anthropic, [llmCompleted({
      content: message.content,
      tool_calls: message.toolCalls,
      provider_replay: { protocol: "openai-chat", reasoning_content: "x" },
    })])
    expect(anthropic.peekProviderReplay?.(message)).toBeUndefined()

    // legacy log with no persisted replay -> Anthropic reconstructs neutral blocks.
    seedProviderReplayFromEvents(anthropic, [llmCompleted({
      content: message.content,
      tool_calls: message.toolCalls,
    })])
    expect(anthropic.peekProviderReplay?.(message)?.native_blocks).toEqual([
      { type: "text", text: "calling" },
      { type: "tool_use", id: "c1", name: "ping", input: { a: 1 } },
    ])
  })
})
