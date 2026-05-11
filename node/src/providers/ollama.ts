import type { Message, ToolSchema, StreamEvent, TextDelta, ToolCallEvent, LLMProvider } from "../types.js"
import { normalizeToolCall } from "./base.js"

export class OllamaProvider implements LLMProvider {
  constructor(
    private readonly model = "llama3",
    private readonly baseUrl = "http://localhost:11434",
  ) {}

  private toOllamaMessages(messages: Message[]) {
    return messages.map(m => ({ role: m.role, content: m.content }))
  }

  async complete(messages: Message[], tools: ToolSchema[]): Promise<Message> {
    const resp = await fetch(`${this.baseUrl}/api/chat`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ model: this.model, messages: this.toOllamaMessages(messages), stream: false }),
    })
    if (!resp.ok) throw new Error(`Ollama error: ${resp.status}`)
    const data = await resp.json() as { message: { content: string } }
    return { role: "assistant", content: data.message.content }
  }

  async *stream(messages: Message[], tools: ToolSchema[], extensions?: Record<string, unknown>): AsyncIterable<StreamEvent> {
    const resp = await fetch(`${this.baseUrl}/api/chat`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ model: this.model, messages: this.toOllamaMessages(messages), stream: true }),
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
