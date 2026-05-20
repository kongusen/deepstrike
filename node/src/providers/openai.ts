import OpenAI from "openai"
import type { Message, ProviderReplay, RenderedContext, ToolSchema, StreamEvent, TextDelta, ThinkingDelta, ToolCallEvent, LLMProvider, RuntimePolicy } from "../types.js"
import { withServerRuntimeGuard } from "../runtime/server.js"
import { CircuitBreaker, omitExtensionKeys, ThinkingTagStreamExtractor } from "./base.js"
import { OpenAIChatAdapter } from "./openai-chat.js"

const OPENAI_POLICIES: Record<string, RuntimePolicy> = {
  "gpt-5.5":       { maxTurns: 60 },
  "gpt-5.4":       { maxTurns: 50 },
  "gpt-5.4-mini":  { maxTurns: 25 },
  "gpt-5.4-nano":  { maxTurns: 15 },
  "gpt-5.2":       { maxTurns: 50 },
  "gpt-5.2-pro":   { maxTurns: 60 },
  "gpt-5.1":       { maxTurns: 50 },
  "gpt-4o":        { maxTurns: 25 },
  "gpt-4o-mini":   { maxTurns: 15 },
  "gpt-4.1":       { maxTurns: 35 },
  "gpt-4.1-mini":  { maxTurns: 20 },
  "gpt-4.1-nano":  { maxTurns: 15 },
  "gpt-5":         { maxTurns: 50 },
  "gpt-5-pro":     { maxTurns: 60 },
  "gpt-5-mini":    { maxTurns: 25 },
  "gpt-5-nano":    { maxTurns: 15 },
  "o1":            { maxTurns: 50 },
  "o1-mini":       { maxTurns: 25 },
  "o3":            { maxTurns: 50 },
  "o3-mini":       { maxTurns: 25 },
  "o4-mini":       { maxTurns: 25 },
}

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

  runtimePolicy(): RuntimePolicy {
    return OPENAI_POLICIES[this.model] ?? {}
  }

  peekProviderReplay(message: Pick<Message, "content" | "toolCalls">): ProviderReplay | undefined {
    const fields = this.chat.peekReplayFields(message)
    if (!fields || !("reasoning_content" in fields)) return undefined
    return { reasoning_content: String(fields.reasoning_content ?? "") }
  }

  seedProviderReplay(message: Pick<Message, "content" | "toolCalls">, replay: ProviderReplay): void {
    if (replay.reasoning_content !== undefined) {
      this.chat.rememberReplayFields(message, { reasoning_content: replay.reasoning_content })
    }
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
    const extractor = new ThinkingTagStreamExtractor()
    let accumulatedReasoning = ""
    let accumulatedContent = ""

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
      const delta = choice.delta as any

      if (delta.reasoning_content) {
        accumulatedReasoning += delta.reasoning_content
        yield { type: "thinking_delta", delta: delta.reasoning_content } as ThinkingDelta
      }

      if (delta.content) {
        for (const part of extractor.feed(delta.content)) {
          if (part.type === "thinking") {
            accumulatedReasoning += part.content
            yield { type: "thinking_delta", delta: part.content } as ThinkingDelta
          } else {
            accumulatedContent += part.content
            yield { type: "text_delta", delta: part.content } as TextDelta
          }
        }
      }

      for (const tc of delta.tool_calls ?? []) {
        const idx = tc.index
        if (!toolCallBufs[idx]) toolCallBufs[idx] = { id: tc.id ?? "", name: "", argsBuf: "" }
        if (tc.function?.name) toolCallBufs[idx].name += tc.function.name
        toolCallBufs[idx].argsBuf += tc.function?.arguments ?? ""
      }

      if (choice.finish_reason === "tool_calls") {
        const toolCalls = Object.values(toolCallBufs).map(tb => ({
          id: tb.id, name: tb.name, arguments: tb.argsBuf || "{}",
        }))
        this.chat.rememberReplayFields({ content: accumulatedContent, toolCalls }, { reasoning_content: accumulatedReasoning })
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

    for (const part of extractor.flush()) {
      if (part.type === "thinking") {
        accumulatedReasoning += part.content
        yield { type: "thinking_delta", delta: part.content } as ThinkingDelta
      } else {
        accumulatedContent += part.content
        yield { type: "text_delta", delta: part.content } as TextDelta
      }
    }

    const toolCalls = Object.values(toolCallBufs).map(tb => ({
      id: tb.id, name: tb.name, arguments: tb.argsBuf || "{}",
    }))
    if (toolCalls.length || accumulatedReasoning) {
      this.chat.rememberReplayFields({ content: accumulatedContent, toolCalls }, { reasoning_content: accumulatedReasoning })
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
