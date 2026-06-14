import type { Message, RenderedContext, ToolSchema, StreamEvent, TextDelta, ToolCallEvent, LLMProvider, RuntimePolicy } from "../types.js"
import { normalizeToolCall, omitExtensionKeys, turnsWithStateAppended } from "./base.js"

// Prefix-based policy for local models (first match wins)
const OLLAMA_PREFIX_POLICIES: Array<[string, RuntimePolicy]> = [
  ["deepseek-r1",  { maxTurns: 40 }],
  ["qwq",          { maxTurns: 35 }],
  ["llama3.3",     { maxTurns: 25 }],
  ["llama3.2",     { maxTurns: 20 }],
  ["llama3.1",     { maxTurns: 20 }],
  ["llama3",       { maxTurns: 20 }],
  ["mistral",      { maxTurns: 20 }],
  ["gemma2",       { maxTurns: 20 }],
  ["phi4",         { maxTurns: 20 }],
  ["phi3",         { maxTurns: 15 }],
  ["codellama",    { maxTurns: 20 }],
]

export class OllamaProvider implements LLMProvider {
  constructor(
    private readonly model = "llama3",
    private readonly baseUrl = "http://localhost:11434",
  ) {}

  runtimePolicy(): RuntimePolicy {
    const m = this.model.toLowerCase()
    for (const [prefix, policy] of OLLAMA_PREFIX_POLICIES) {
      if (m.startsWith(prefix)) return policy
    }
    return { maxTurns: 20 }
  }

  private toOllamaMessages(context: RenderedContext) {
    const result = []
    if (context.systemText) result.push({ role: "system", content: context.systemText })
    for (const m of turnsWithStateAppended(context)) {
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

  private buildTools(tools: ToolSchema[]) {
    return tools.map(t => ({
      type: "function",
      function: { name: t.name, description: t.description, parameters: JSON.parse(t.parameters) },
    }))
  }

  private requestExtensions(extensions?: Record<string, unknown>): Record<string, unknown> {
    return omitExtensionKeys(extensions, ["model", "messages", "tools", "stream"])
  }

  async complete(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): Promise<Message> {
    const resp = await fetch(`${this.baseUrl}/api/chat`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        ...this.requestExtensions(extensions),
        model: this.model,
        messages: this.toOllamaMessages(context),
        ...(tools.length ? { tools: this.buildTools(tools) } : {}),
        stream: false,
      }),
    })
    if (!resp.ok) throw new Error(`Ollama error: ${resp.status}`)
    const data = await resp.json() as { message: { content: string } }
    return { role: "assistant", content: data.message.content }
  }

  async *stream(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): AsyncIterable<StreamEvent> {
    const resp = await fetch(`${this.baseUrl}/api/chat`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        ...this.requestExtensions(extensions),
        model: this.model,
        messages: this.toOllamaMessages(context),
        ...(tools.length ? { tools: this.buildTools(tools) } : {}),
        stream: true,
      }),
    })
    if (!resp.ok) throw new Error(`Ollama error: ${resp.status}`)
    const reader = resp.body!.getReader()
    const decoder = new TextDecoder()
    let buf = ""
    const pendingToolCalls = new Map<string, { id: string; name: string; arguments: Record<string, unknown> }>()
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
            const norm = normalizeToolCall("", tc.function.name, tc.function.arguments)
            if (!norm) continue
            const args = JSON.parse(norm.arguments) as Record<string, unknown>
            const key = `${norm.name}:${norm.arguments}`
            if (!pendingToolCalls.has(key)) {
              pendingToolCalls.set(key, {
                id: `call_${pendingToolCalls.size + 1}`,
                name: norm.name,
                arguments: args,
              })
            }
          }
        } catch { /* skip malformed lines */ }
      }
    }
    for (const tc of pendingToolCalls.values()) {
      yield { type: "tool_call", id: tc.id, name: tc.name, arguments: tc.arguments } as ToolCallEvent
    }
  }
}
