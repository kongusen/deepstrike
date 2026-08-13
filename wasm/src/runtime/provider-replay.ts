import type { LLMProvider, Message, ProviderDescriptor, ProviderReplay, ToolCall } from "../types.js"
import type { SessionEvent } from "./session-log.js"

export function assistantReplayKey(message: Pick<Message, "content" | "toolCalls">): string {
  return JSON.stringify({
    content: message.content,
    toolCalls: message.toolCalls ?? [],
  })
}

/**
 * A stored replay may only be seeded into a provider speaking the same wire
 * protocol; on a cross-protocol fallback the incompatible envelope is skipped so
 * the new provider re-serializes neutral context instead.
 */
export function isReplayCompatibleWithProvider(
  replay: Partial<ProviderReplay>,
  descriptor: ProviderDescriptor | undefined,
): boolean {
  assertCanonicalReplay(replay)
  return !descriptor || replay.protocol === descriptor.protocol
}

const REPLAY_KEYS = new Set(["protocol", "provider", "model", "native_blocks", "reasoning_content", "reasoning_details", "native_message", "tool_calls"])

function assertCanonicalReplay(replay: Partial<ProviderReplay>): asserts replay is ProviderReplay {
  for (const key of Object.keys(replay)) if (!REPLAY_KEYS.has(key)) throw new Error(`provider replay has unknown field ${key}`)
  if (typeof replay.protocol !== "string" || replay.protocol.length === 0) throw new Error("provider replay protocol is required")
}

export function seedProviderReplayFromEvents(
  provider: LLMProvider,
  events: Array<{ event: SessionEvent }>,
): void {
  if (!provider.seedProviderReplay) return
  const descriptor = provider.descriptor?.()
  for (const { event } of events) {
    if (event.kind !== "llm_completed") continue
    const toolCalls = event.tool_calls ?? []
    const stored = event.provider_replay
    if (!stored || !isReplayCompatibleWithProvider(stored, descriptor)) continue
    provider.seedProviderReplay({ content: event.content, toolCalls }, stored)
  }
}

export function peekProviderReplay(
  provider: LLMProvider,
  content: string,
  toolCalls: ToolCall[],
): ProviderReplay | undefined {
  return provider.peekProviderReplay?.({ content, toolCalls })
}
