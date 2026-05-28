import type { Message, ContentPart, RenderedContext } from "../types.js"

export class CircuitBreaker {
  private failures = 0
  private openedAt: number | null = null

  constructor(
    private readonly openAfter: number = 5,
    private readonly resetAfter: number = 60_000,
  ) {}

  isOpen(): boolean {
    if (this.openedAt === null) return false
    if (Date.now() - this.openedAt >= this.resetAfter) {
      this.openedAt = null
      return false
    }
    return true
  }

  recordSuccess(): void {
    this.failures = 0
    this.openedAt = null
  }

  recordFailure(): void {
    this.failures++
    if (this.failures >= this.openAfter) this.openedAt = Date.now()
  }
}


export function omitExtensionKeys(
  extensions: Record<string, unknown> | undefined,
  keys: readonly string[],
): Record<string, unknown> {
  if (!extensions) return {}
  const blocked = new Set(keys)
  return Object.fromEntries(Object.entries(extensions).filter(([key]) => !blocked.has(key)))
}

export function normalizeToolCall(id: string, name: string, args: unknown): { id: string; name: string; arguments: string } | null {
  const n = String(name ?? "").trim()
  if (!n) return null
  let parsed: Record<string, unknown> = {}
  if (typeof args === "string") {
    try { parsed = JSON.parse(args || "{}") } catch { parsed = {} }
  } else if (args && typeof args === "object") {
    parsed = args as Record<string, unknown>
  }
  return { id: String(id ?? ""), name: n, arguments: JSON.stringify(parsed) }
}

function parseToolArguments(args: string): Record<string, unknown> {
  try { return JSON.parse(args || "{}") as Record<string, unknown> } catch { return {} }
}

// ─── Anthropic message conversion ────────────────────────────────────────────

export function toAnthropicContent(msg: Message): string | Array<Record<string, unknown>> {
  if (!msg.contentParts?.length) return msg.content
  return msg.contentParts.map(p => {
    if (p.type === "text") return { type: "text", text: p.text }
    if (p.type === "image") {
      if (p.data) {
        return { type: "image", source: { type: "base64", media_type: p.mediaType ?? "image/png", data: p.data } }
      }
      return { type: "image", source: { type: "url", url: p.url } }
    }
    if (p.type === "audio") {
      return { type: "text", text: `[audio: ${p.mediaType}]` }
    }
    if (p.type === "tool_result") {
      return { type: "tool_result", tool_use_id: p.callId, content: p.output, is_error: p.isError }
    }
    return { type: "text", text: "" }
  })
}

/** Convert RenderedContext.turns to Anthropic messages array.
 *  `turns` contains only user / assistant / tool roles — no system filtering needed. */
export function toAnthropicMessages(
  turns: Message[],
  nativeReplay?: (message: Message) => Array<Record<string, unknown>> | undefined,
): Array<Record<string, unknown>> {
  const result: Array<Record<string, unknown>> = []

  for (const msg of turns) {
    if (msg.role === "tool") {
      const parts = (msg.contentParts ?? [])
        .filter((p): p is Extract<ContentPart, { type: "tool_result" }> => p.type === "tool_result")
        .map(p => ({ type: "tool_result", tool_use_id: p.callId, content: p.output, is_error: p.isError }))
      if (parts.length) result.push({ role: "user", content: parts })
      continue
    }

    if (msg.role === "assistant" && msg.toolCalls?.length) {
      const replay = nativeReplay?.(msg)
      if (replay) {
        result.push({ role: "assistant", content: ensureAssistantToolText(replay) })
        continue
      }
      const blocks: Array<Record<string, unknown>> = []
      if (msg.content) blocks.push({ type: "text", text: msg.content })
      else blocks.push({ type: "text", text: "Tool call requested." })
      blocks.push(...msg.toolCalls.map(tc => ({
        type: "tool_use",
        id: tc.id,
        name: tc.name,
        input: parseToolArguments(tc.arguments),
      })))
      result.push({ role: "assistant", content: blocks })
      continue
    }

    result.push({ role: msg.role, content: toAnthropicContent(msg) })
  }

  return result
}

function ensureAssistantToolText(blocks: Array<Record<string, unknown>>): Array<Record<string, unknown>> {
  if (!blocks.some(b => b.type === "tool_use")) return blocks
  if (blocks.some(b => b.type === "text" && String(b.text ?? "").trim())) return blocks
  if (blocks.some(b => b.type === "thinking")) return blocks
  return [{ type: "text", text: "Tool call requested." }, ...blocks]
}

// ─── OpenAI-compatible message conversion ────────────────────────────────────

export function toOpenAIContent(msg: Message): string | Array<Record<string, unknown>> {
  if (!msg.contentParts?.length) return msg.content
  return msg.contentParts.map(p => {
    if (p.type === "text") return { type: "text", text: p.text }
    if (p.type === "image") {
      const url = p.data ? `data:${p.mediaType ?? "image/png"};base64,${p.data}` : p.url!
      return { type: "image_url", image_url: { url, ...(p.detail ? { detail: p.detail } : {}) } }
    }
    if (p.type === "audio") {
      return { type: "input_audio", input_audio: { data: p.data, format: p.mediaType?.split("/")[1] ?? "wav" } }
    }
    if (p.type === "tool_result") {
      return { type: "text", text: p.output }
    }
    return { type: "text", text: "" }
  })
}

/** Build the full OpenAI messages array from a RenderedContext.
 *  Prepends systemText as the first system message, then converts turns. */
export function toOpenAIMessageParams(context: RenderedContext): Array<Record<string, unknown>> {
  const result: Array<Record<string, unknown>> = []

  if (context.systemText) {
    result.push({ role: "system", content: context.systemText })
  }

  for (const msg of context.turns) {
    if (msg.role === "tool") {
      const parts = (msg.contentParts ?? [])
        .filter((p): p is Extract<ContentPart, { type: "tool_result" }> => p.type === "tool_result")
      for (const p of parts) {
        result.push({ role: "tool", tool_call_id: p.callId, content: p.output })
      }
      continue
    }

    const next: Record<string, unknown> = {
      role: msg.role,
      content: toOpenAIContent(msg),
    }
    if (msg.role === "assistant" && msg.toolCalls?.length) {
      next.tool_calls = msg.toolCalls.map(tc => ({
        id: tc.id,
        type: "function",
        function: { name: tc.name, arguments: tc.arguments },
      }))
    }
    result.push(next)
  }

  return result
}

export class ThinkingTagStreamExtractor {
  private buffer = ""
  private inThinking = false

  feed(chunk: string): Array<{ type: "text" | "thinking"; content: string }> {
    this.buffer += chunk
    const events: Array<{ type: "text" | "thinking"; content: string }> = []

    while (true) {
      if (!this.inThinking) {
        const thinkIndex = this.buffer.indexOf("<think>")
        if (thinkIndex !== -1) {
          const textBefore = this.buffer.substring(0, thinkIndex)
          if (textBefore) {
            events.push({ type: "text", content: textBefore })
          }
          this.inThinking = true
          this.buffer = this.buffer.substring(thinkIndex + 7)
          continue
        }

        const possibleTagStart = this.buffer.lastIndexOf("<")
        if (possibleTagStart !== -1 && "<think>".startsWith(this.buffer.substring(possibleTagStart))) {
          const toEmit = this.buffer.substring(0, possibleTagStart)
          if (toEmit) {
            events.push({ type: "text", content: toEmit })
          }
          this.buffer = this.buffer.substring(possibleTagStart)
          break
        } else {
          if (this.buffer) {
            events.push({ type: "text", content: this.buffer })
            this.buffer = ""
          }
          break
        }
      } else {
        const endThinkIndex = this.buffer.indexOf("</think>")
        if (endThinkIndex !== -1) {
          const thinkingContent = this.buffer.substring(0, endThinkIndex)
          if (thinkingContent) {
            events.push({ type: "thinking", content: thinkingContent })
          }
          this.inThinking = false
          this.buffer = this.buffer.substring(endThinkIndex + 8)
          continue
        }

        const possibleEndStart = this.buffer.lastIndexOf("<")
        if (possibleEndStart !== -1 && "</think>".startsWith(this.buffer.substring(possibleEndStart))) {
          const toEmit = this.buffer.substring(0, possibleEndStart)
          if (toEmit) {
            events.push({ type: "thinking", content: toEmit })
          }
          this.buffer = this.buffer.substring(possibleEndStart)
          break
        } else {
          if (this.buffer) {
            events.push({ type: "thinking", content: this.buffer })
            this.buffer = ""
          }
          break
        }
      }
    }
    return events
  }

  flush(): Array<{ type: "text" | "thinking"; content: string }> {
    const events: Array<{ type: "text" | "thinking"; content: string }> = []
    if (this.buffer) {
      events.push({ type: this.inThinking ? "thinking" : "text", content: this.buffer })
      this.buffer = ""
    }
    return events
  }
}
