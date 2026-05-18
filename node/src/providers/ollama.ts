import type { Message, RenderedContext, ToolSchema, StreamEvent, TextDelta, ToolCallEvent, LLMProvider } from "../types.js"
import { normalizeToolCall } from "./base.js"

export class OllamaProvider implements LLMProvider {
  constructor(
    private readonly model = "llama3",
    private readonly baseUrl = "http://localhost:11434",
  ) {}

  private toOllamaMessages(context: RenderedContext) {
    const result = []
    if (context.systemText) result.push({ role: "system", content: context.systemText })
    for (const m of context.turns) {
      const images: string[] = []
      if (m.contentParts?.length) {
        for (const p of m.contentParts) {
          if (p.type === "image" && p.data) images.push(p.data)
        }
      }
      result.push({ role: m.role, content: m.content, ...(images.length ? { images } : {}) })
    }
    return result
  }

  async complete(context: RenderedContext, tools: ToolSchema[]): Promise<Message> {
    const resp = await fetch(`${this.baseUrl}/api/chat`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ model: this.model, messages: this.toOllamaMessages(context), stream: false }),
    })
    if (!resp.ok) throw new Error(`Ollama error: ${resp.status}`)
    const data = await resp.json() as { message: { content: string } }
    return { role: "assistant", content: data.message.content }
  }

  async *stream(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): AsyncIterable<StreamEvent> {
    const resp = await fetch(`${this.baseUrl}/api/chat`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ model: this.model, messages: this.toOllamaMessages(context), stream: true }),
    })
    if (!resp.ok) throw new Error(`Ollama error: ${resp.status}`)
    const reader = resp.body!.getReader()
    const decoder = new TextDecoder()
    let buf = ""
    while (true) {
      const { done, value } = await reader.read()
      if (done) break
      buf += decoder.decode(value, { stream: true })
      const lines = buf.split("\n")
      buf = lines.pop() ?? ""
      for (const line of lines) {
        if (!line.trim()) continue
        try {
          const chunk = JSON.parse(line) as { message?: { content?: string; tool_calls?: Array<{ function: { name: string; arguments: unknown } }> } }
          if (chunk.message?.content) yield { type: "text_delta", delta: chunk.message.content } as TextDelta
          for (const tc of chunk.message?.tool_calls ?? []) {
            const norm = normalizeToolCall(crypto.randomUUID(), tc.function.name, tc.function.arguments)
            if (norm) yield { type: "tool_call", id: norm.id, name: norm.name, arguments: JSON.parse(norm.arguments) } as ToolCallEvent
          }
        } catch { /* skip malformed lines */ }
      }
    }
  }
}
