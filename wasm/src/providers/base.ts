import type { Message, RenderedContext } from "../types.js"

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

/** Anthropic messages array (system is a separate top-level param). */
export function toAnthropicMessages(context: RenderedContext): Array<{ role: string; content: string }> {
  return context.turns
    .filter(m => m.role !== "system")
    .map(m => ({ role: m.role, content: m.content }))
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
