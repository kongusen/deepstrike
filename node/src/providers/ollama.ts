import type { Message, RenderedContext, ToolSchema, StreamEvent, TextDelta, ToolCallEvent, LLMProvider, RuntimePolicy, UsageEvent } from "../types.js"
import { normalizeToolCall, omitExtensionKeys, turnsWithStateAppended, UnsupportedModalityError } from "./base.js"
import { normalizeOllamaUsage } from "./usage-normalizer.js"
import { normalizeToolResultPart, projectToolOutputToText } from "./content-normalization.js"

export class OllamaProvider implements LLMProvider {
  constructor(
    private readonly model = "llama3",
    private readonly baseUrl = "http://localhost:11434",
    private readonly resolvedRuntimePolicy: RuntimePolicy = {},
  ) {}

  runtimePolicy(): RuntimePolicy {
    return this.resolvedRuntimePolicy
  }

  private toOllamaMessages(context: RenderedContext) {
    // spc_012-N-04: Ollama's wire is OpenAI-chat-like — tool-role messages are text-only.
    // Explicit degradation via the text projection (`content`/`output` carry a visible
    // `[modality]` placeholder for structured blocks, INV-012-01), same class as openai-chat.
    const result = []
    if (context.systemText) result.push({ role: "system", content: context.systemText })
    for (const m of turnsWithStateAppended(context)) {
      const images: string[] = []
      let content = m.content
      if (m.contentParts?.length) {
        for (const p of m.contentParts) {
          if (p.type === "image" && p.data) images.push(p.data)
          else if (p.type === "audio") throw new UnsupportedModalityError("audio", "ollama")
          else if (p.type === "tool_result") {
            content = projectToolOutputToText(normalizeToolResultPart(p).blocks)
          }
        }
      }
      result.push({ role: m.role, content, ...(images.length ? { images } : {}) })
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
    let finalChunk: { done?: boolean; prompt_eval_count?: number; eval_count?: number } | undefined
    while (true) {
      const { done, value } = await reader.read()
      if (done) break
      buf += decoder.decode(value, { stream: true })
      const lines = buf.split("\n")
      buf = lines.pop() ?? ""
      for (const line of lines) {
        if (!line.trim()) continue
        try {
          const chunk = JSON.parse(line) as {
            message?: { content?: string; tool_calls?: Array<{ function: { name: string; arguments: unknown } }> }
            done?: boolean
            prompt_eval_count?: number
            eval_count?: number
          }
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
          // spc_011-C-07: Ollama's `done: true` chunk carries the request's usage figures — this
          // provider had zero usage extraction before this card.
          if (chunk.done) finalChunk = chunk
        } catch { /* skip malformed lines */ }
      }
    }
    for (const tc of pendingToolCalls.values()) {
      yield { type: "tool_call", id: tc.id, name: tc.name, arguments: tc.arguments } as ToolCallEvent
    }
    if (finalChunk?.prompt_eval_count !== undefined || finalChunk?.eval_count !== undefined) {
      const providerUsage = normalizeOllamaUsage(finalChunk)!
      yield {
        type: "usage",
        totalTokens: providerUsage.inputTokens + providerUsage.outputTokens,
        inputTokens: providerUsage.inputTokens,
        outputTokens: providerUsage.outputTokens,
        providerUsage,
      } as UsageEvent
    }
  }
}
