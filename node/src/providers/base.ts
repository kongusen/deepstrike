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

import type { Message, ContentPart } from "../types.js"

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

function parseToolArguments(args: string): Record<string, unknown> {
  try { return JSON.parse(args || "{}") as Record<string, unknown> } catch { return {} }
}

export function splitAnthropicSystem(messages: Message[]): string {
  return messages.filter(m => m.role === "system").map(m => m.content).join("\n\n")
}

export function toAnthropicMessages(
  messages: Message[],
  nativeReplay?: (message: Message) => Array<Record<string, unknown>> | undefined,
): Array<Record<string, unknown>> {
  const result: Array<Record<string, unknown>> = []

  for (const msg of messages.filter(m => m.role !== "system")) {
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

    result.push({
      role: msg.role,
      content: toAnthropicContent(msg),
    })
  }

  return result
}

export function toOpenAIMessageParams(messages: Message[]): Array<Record<string, unknown>> {
  const result: Array<Record<string, unknown>> = []

  for (const msg of messages) {
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
