import type { LLMProvider, Message, ProviderReplay, ToolCall } from "../types.js"
import type { SessionEvent } from "./session-log.js"
import { effectiveProviderReplay } from "./session-repair.js"

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

export function seedProviderReplayFromEvents(
  provider: LLMProvider,
  events: Array<{ event: SessionEvent }>,
): void {
  if (!provider.seedProviderReplay) return
  for (const { event } of events) {
    if (event.kind !== "llm_completed") continue
    const toolCalls = event.tool_calls ?? []
    const replay = effectiveProviderReplay(event.content, toolCalls, event.provider_replay)
    if (!replay) continue
    provider.seedProviderReplay(
      { content: event.content, toolCalls },
      replay,
    )
  }
}

export function peekProviderReplay(
  provider: LLMProvider,
  content: string,
  toolCalls: ToolCall[],
): ProviderReplay | undefined {
  return provider.peekProviderReplay?.({ content, toolCalls })
}
