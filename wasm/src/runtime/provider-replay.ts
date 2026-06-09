import type { LLMProvider, Message, ProviderDescriptor, ProviderProtocol, ProviderReplay, ToolCall } from "../types.js"
import type { SessionEvent } from "./session-log.js"

export function assistantReplayKey(message: Pick<Message, "content" | "toolCalls">): string {
  return JSON.stringify({
    content: message.content,
    toolCalls: message.toolCalls ?? [],
  })
}

/** Infer the wire protocol a stored replay envelope belongs to (explicit, or by shape for legacy logs). */
function replayProtocol(replay: ProviderReplay): ProviderProtocol | undefined {
  if (replay.protocol) return replay.protocol
  if (replay.native_blocks?.length) return "anthropic-messages"
  if (replay.reasoning_content != null || replay.reasoning_details !== undefined) return "openai-chat"
  return undefined
}

/**
 * A stored replay may only be seeded into a provider speaking the same wire
 * protocol; on a cross-protocol fallback the incompatible envelope is skipped so
 * the new provider re-serializes neutral context instead.
 */
export function isReplayCompatibleWithProvider(
  replay: ProviderReplay,
  descriptor: ProviderDescriptor | undefined,
): boolean {
  if (!descriptor) return true
  const protocol = replayProtocol(replay)
  if (!protocol) return true
  return protocol === descriptor.protocol
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
    if (stored && !isReplayCompatibleWithProvider(stored, descriptor)) continue
    // Pass the message even with no persisted replay: a provider may reconstruct
    // a legacy replay (e.g. Anthropic native_blocks) from the neutral transcript.
    provider.seedProviderReplay({ content: event.content, toolCalls }, stored ?? {})
  }
}

export function peekProviderReplay(
  provider: LLMProvider,
  content: string,
  toolCalls: ToolCall[],
): ProviderReplay | undefined {
  return provider.peekProviderReplay?.({ content, toolCalls })
}
