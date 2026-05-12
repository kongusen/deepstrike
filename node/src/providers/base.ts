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
    return { type: "text", text: "" }
  })
}
