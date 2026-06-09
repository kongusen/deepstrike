import type { LLMProvider, Message, ProviderDescriptor, ProviderProtocol, ProviderReplay, ToolCall } from "../types.js"
import type { SessionEvent } from "./session-log.js"

function sortObjectKeys(val: any): any {
  if (val === null || typeof val !== "object") {
    return val
  }
  if (Array.isArray(val)) {
    return val.map(sortObjectKeys)
  }
  const sortedKeys = Object.keys(val).sort()
  const result: Record<string, any> = {}
  for (const key of sortedKeys) {
    result[key] = sortObjectKeys(val[key])
  }
  return result
}

export function assistantReplayKey(message: Pick<Message, "content" | "toolCalls">): string {
  const toolCalls = (message.toolCalls ?? []).map(tc => {
    let normalizedArgs = tc.arguments
    try {
      const parsed = typeof tc.arguments === "string" ? JSON.parse(tc.arguments) : tc.arguments
      normalizedArgs = JSON.stringify(sortObjectKeys(parsed))
    } catch {
      // fallback
    }
    return {
      id: tc.id,
      name: tc.name,
      arguments: normalizedArgs,
    }
  })
  return JSON.stringify({
    content: message.content,
    toolCalls,
  })
}

/**
 * Infer the wire protocol a stored replay envelope belongs to.
 *
 * New envelopes carry an explicit `protocol`. Legacy envelopes are inferred
 * from their shape: Anthropic persisted `native_blocks`, OpenAI-compatible
 * persisted `reasoning_content` / `reasoning_details`.
 */
function replayProtocol(replay: ProviderReplay): ProviderProtocol | undefined {
  if (replay.protocol) return replay.protocol
  if (replay.native_blocks?.length) return "anthropic-messages"
  if (replay.reasoning_content != null || replay.reasoning_details !== undefined) return "openai-chat"
  return undefined
}

/**
 * A stored replay may only be seeded into a provider speaking the same wire
 * protocol. On a cross-protocol fallback (provider A -> provider B) the
 * incompatible envelope is skipped so B re-serializes neutral context instead
 * of replaying A's protocol-specific shape.
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
    // Pass the message even when no replay was persisted: a provider may
    // reconstruct a legacy replay (e.g. Anthropic native_blocks) from the
    // neutral transcript. Providers that cannot reconstruct simply no-op.
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
