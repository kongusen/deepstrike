import type { LLMProvider, Message, ProviderReplay, ToolCall } from "../types.js"
import type { SessionEvent } from "./session-log.js"

export function assistantReplayKey(message: Pick<Message, "content" | "toolCalls">): string {
  return JSON.stringify({
    content: message.content,
    toolCalls: message.toolCalls ?? [],
  })
}

export function seedProviderReplayFromEvents(
  provider: LLMProvider,
  events: Array<{ event: SessionEvent }>,
): void {
  if (!provider.seedProviderReplay) return
  for (const { event } of events) {
    if (event.kind !== "llm_completed" || !event.provider_replay) continue
    provider.seedProviderReplay(
      { content: event.content, toolCalls: event.tool_calls ?? [] },
      event.provider_replay,
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
