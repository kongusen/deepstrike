import type { Message, RenderedContext } from "../types.js"
import { assistantReplayKey } from "../runtime/provider-replay.js"

function parseToolArguments(args: string): Record<string, unknown> {
  try { return JSON.parse(args || "{}") as Record<string, unknown> } catch { return {} }
}

/** Build OpenAI-compatible chat messages from a RenderedContext. */
export function toOpenAIMessages(context: RenderedContext): Array<Record<string, unknown>> {
  const messages: Array<Record<string, unknown>> = []
  if (context.systemText) {
    messages.push({ role: "system", content: context.systemText })
  }
  for (const msg of context.turns) {
    if (msg.role === "tool") {
      messages.push({ role: "tool", content: msg.content })
      continue
    }
    const next: Record<string, unknown> = { role: msg.role, content: msg.content }
    if (msg.role === "assistant" && msg.toolCalls?.length) {
      next.tool_calls = msg.toolCalls.map(tc => ({
        id: tc.id,
        type: "function",
        function: { name: tc.name, arguments: tc.arguments },
      }))
    }
    messages.push(next)
  }
  return messages
}

export function toAnthropicMessages(
  context: RenderedContext,
  nativeReplay?: (message: Message) => Array<Record<string, unknown>> | undefined,
): Array<Record<string, unknown>> {
  const result: Array<Record<string, unknown>> = []

  for (const msg of context.turns) {
    if (msg.role === "tool") {
      result.push({ role: "user", content: [{ type: "tool_result", tool_use_id: msg.content ? undefined : "", content: msg.content, is_error: false }] })
      continue
    }

    if (msg.role === "assistant" && msg.toolCalls?.length) {
      const replay = nativeReplay?.(msg)
      if (replay) {
        result.push({ role: "assistant", content: replay })
        continue
      }
      const blocks: Array<Record<string, unknown>> = []
      if (msg.content) blocks.push({ type: "text", text: msg.content })
      blocks.push(...msg.toolCalls.map(tc => ({
        type: "tool_use",
        id: tc.id,
        name: tc.name,
        input: parseToolArguments(tc.arguments),
      })))
      result.push({ role: "assistant", content: blocks })
      continue
    }

    result.push({ role: msg.role, content: msg.content })
  }

  return result
}

/** Collect a non-streaming assistant Message from stream events. */
export async function collectStreamMessage(
  stream: AsyncIterable<{ type: string; delta?: string; id?: string; name?: string; arguments?: Record<string, unknown> }>,
): Promise<Message> {
  let content = ""
  const toolCalls: Message["toolCalls"] = []
  for await (const evt of stream) {
    if (evt.type === "text_delta" && evt.delta) content += evt.delta
    else if (evt.type === "tool_call" && evt.id && evt.name) {
      toolCalls.push({ id: evt.id, name: evt.name, arguments: JSON.stringify(evt.arguments ?? {}) })
    }
  }
  return { role: "assistant", content, ...(toolCalls.length ? { toolCalls } : {}) }
}

export { assistantReplayKey }
