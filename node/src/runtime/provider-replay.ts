import type { LLMProvider, Message, ProviderDescriptor, ProviderReplay, RenderedContext, ReplayabilityAssessment, ToolCall } from "../types.js"
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
 * A stored replay may only be seeded into a provider speaking the same wire
 * protocol. On a cross-protocol fallback (provider A -> provider B) the
 * incompatible envelope is skipped so B re-serializes neutral context instead
 * of replaying A's protocol-specific shape.
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

/**
 * Pre-flight query for fallback routing: would `context` validate against
 * `provider` (with `extensions`) before the request is sent? Seed any persisted
 * replay (via `seedProviderReplayFromEvents`) first so the assessment reflects
 * what the provider can actually replay. Providers that do not implement
 * `assessReplayability` (no reasoning-replay requirement) are reported as ok.
 */
export function assessProviderReplayability(
  provider: LLMProvider,
  context: RenderedContext,
  extensions?: Record<string, unknown>,
): ReplayabilityAssessment {
  return provider.assessReplayability?.(context, extensions) ?? { ok: true, offendingCallIds: [] }
}
