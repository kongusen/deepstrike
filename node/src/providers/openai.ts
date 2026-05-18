import OpenAI from "openai"
import type { Message, RenderedContext, ToolSchema, StreamEvent, TextDelta, ToolCallEvent, LLMProvider } from "../types.js"
import { withServerRuntimeGuard } from "../runtime/server.js"
import { CircuitBreaker, omitExtensionKeys } from "./base.js"
import { OpenAIChatAdapter } from "./openai-chat.js"

export class OpenAIChatProvider implements LLMProvider {
  protected client: OpenAI
  protected circuit: CircuitBreaker
  protected maxRetries: number
  protected baseDelay: number
  protected readonly chat = new OpenAIChatAdapter()

  constructor(
    apiKey: string,
    protected readonly model = "gpt-4o",
    retry = { maxRetries: 3, baseDelay: 1000 },
    baseURL = "https://api.openai.com/v1",
  ) {
    this.client = withServerRuntimeGuard(() => new OpenAI({ apiKey, baseURL }))
    this.circuit = new CircuitBreaker()
    this.maxRetries = retry.maxRetries
    this.baseDelay = retry.baseDelay
  }

  async complete(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): Promise<Message> {
    if (this.circuit.isOpen()) throw new Error("Circuit breaker open")
    const msgs = this.chat.buildMessages(context)

    let lastErr: unknown
    for (let i = 0; i < this.maxRetries; i++) {
      try {
        const resp = await this.client.chat.completions.create({
          ...this.requestExtensions(extensions),
          model: this.model,
          messages: msgs,
          ...(tools.length ? { tools: this.chat.buildTools(tools) } : {}),
        })
        this.circuit.recordSuccess()
        const choice = resp.choices[0].message
        const toolCalls = this.chat.normalizeToolCalls(choice.tool_calls ?? [])
        return { role: "assistant", content: choice.content ?? "", tokenCount: resp.usage?.total_tokens, toolCalls }
      } catch (err) {
        lastErr = err
        this.circuit.recordFailure()
        if (i < this.maxRetries - 1) await new Promise(r => setTimeout(r, this.baseDelay * 2 ** i))
      }
    }
    throw lastErr
  }

  async *stream(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): AsyncIterable<StreamEvent> {
    const msgs = this.chat.buildMessages(context)
    const toolCallBufs: Record<number, { id: string; name: string; argsBuf: string }> = {}
    const emittedToolCallIndexes = new Set<number>()

    const stream = await this.client.chat.completions.create({
      ...this.requestExtensions(extensions),
      model: this.model,
      messages: msgs,
      ...(tools.length ? { tools: this.chat.buildTools(tools) } : {}),
      stream: true,
      stream_options: { include_usage: true },
    })

    let totalTokens = 0
    for await (const chunk of stream) {
      if (chunk.usage) { totalTokens = chunk.usage.total_tokens; continue }
      const choice = chunk.choices[0]
      if (!choice) continue
      const delta = choice.delta

      if (delta.content) yield { type: "text_delta", delta: delta.content } as TextDelta

      for (const tc of delta.tool_calls ?? []) {
        const idx = tc.index
        if (!toolCallBufs[idx]) toolCallBufs[idx] = { id: tc.id ?? "", name: "", argsBuf: "" }
        if (tc.function?.name) toolCallBufs[idx].name += tc.function.name
        toolCallBufs[idx].argsBuf += tc.function?.arguments ?? ""
      }

      if (choice.finish_reason === "tool_calls") {
        for (const [index, tb] of Object.entries(toolCallBufs)) {
          const idx = Number(index)
          if (emittedToolCallIndexes.has(idx)) continue
          let args: Record<string, unknown> = {}
          try { args = JSON.parse(tb.argsBuf || "{}") } catch { args = {} }
          emittedToolCallIndexes.add(idx)
          yield { type: "tool_call", id: tb.id, name: tb.name, arguments: args } as ToolCallEvent
        }
      }
    }
    for (const [index, tb] of Object.entries(toolCallBufs)) {
      const idx = Number(index)
      if (emittedToolCallIndexes.has(idx)) continue
      let args: Record<string, unknown> = {}
      try { args = JSON.parse(tb.argsBuf || "{}") } catch { args = {} }
      emittedToolCallIndexes.add(idx)
      yield { type: "tool_call", id: tb.id, name: tb.name, arguments: args } as ToolCallEvent
    }
    if (totalTokens > 0) yield { type: "usage", totalTokens } as StreamEvent
  }

  protected requestExtensions(extensions?: Record<string, unknown>): Record<string, unknown> {
    return omitExtensionKeys(extensions, ["model", "messages", "tools", "stream", "stream_options"])
  }
}

export { OpenAIChatProvider as OpenAIProvider }

