/**
 * Fixture helpers for `ReplayProvider`.
 *
 * The canonical persistence shape for a recorded run is a `SessionLog` of `llm_completed` events
 * (already written by the runner on every live run). `extractRecordedMessages` walks such a log and
 * pulls the assistant turns in order, so the fixture is just "a prior session log + the messages
 * the LLM produced". No new on-disk format.
 */

import type { Message, ToolCall } from "../types.js"
import type { SessionEvent } from "./session-log.js"

/**
 * Extract the ordered list of assistant Messages from a recorded session log.
 *
 * Walks `llm_completed` events (which is what the runner appends for every LLM call) and produces
 * one Message per event. Pass the result directly to `new ReplayProvider(messages)`.
 *
 * Accepts both wire shapes the SDK uses interchangeably:
 *   - in-memory: `{ toolCalls, tokenCount, providerReplay }` (camelCase)
 *   - serialised session-log: `{ tool_calls, token_count, provider_replay }` (snake_case)
 *
 * @param events Session events, in original order. Accepts both `{ event, seq }` (the shape
 *               `SessionLog.read()` returns) and a bare `SessionEvent[]`.
 */
export function extractRecordedMessages(
  events: Array<{ event: SessionEvent } | SessionEvent>,
): Message[] {
  const out: Message[] = []
  for (const entry of events) {
    const event: SessionEvent = isWrapped(entry) ? entry.event : entry
    if (event.kind !== "llm_completed") continue
    const e = event as unknown as Record<string, unknown>
    const tcRaw = (e.toolCalls ?? e.tool_calls) as unknown[] | undefined
    const tokenCount = (e.tokenCount ?? e.token_count) as number | undefined
    out.push({
      role: "assistant",
      content: typeof e.content === "string" ? e.content : "",
      ...(Array.isArray(tcRaw) && tcRaw.length > 0
        ? { toolCalls: normalizeToolCalls(tcRaw as ToolCall[]) }
        : {}),
      ...(tokenCount !== undefined ? { tokenCount } : {}),
    })
  }
  return out
}

function isWrapped(x: unknown): x is { event: SessionEvent } {
  return !!x && typeof x === "object" && "event" in (x as Record<string, unknown>)
}

function normalizeToolCalls(tcs: ToolCall[]): ToolCall[] {
  return tcs.map(tc => ({
    id: tc.id,
    name: tc.name,
    arguments: typeof tc.arguments === "string" ? tc.arguments : JSON.stringify(tc.arguments),
  }))
}
