import OpenAI from "openai"
import type { LLMProvider, Message, RenderedContext, StreamEvent, TextDelta, ThinkingDelta, ToolCallEvent, ToolSchema, RuntimePolicy, ProviderReplay } from "../types.js"
import { withServerRuntimeGuard } from "../runtime/server.js"
import { CircuitBreaker, omitExtensionKeys } from "./base.js"
import { OpenAIChatAdapter } from "./openai-chat.js"
import { endpointProfiles } from "./profiles.js"

const QWEN_BASE = (endpointProfiles as Record<string, { baseURL: string }>)["qwen.dashscope"].baseURL

const QWEN_POLICIES: Record<string, RuntimePolicy> = {
  "qwen3.7-max-preview": { maxTurns: 45 },
  "qwen3.7-plus-preview": { maxTurns: 40 },
  "qwen3.6-max-preview": { maxTurns: 40 },
  "qwen3.6-plus": { maxTurns: 35 },
  "qwen3.6-flash": { maxTurns: 20 },
  "qwen3.6-35b-a3b": { maxTurns: 25 },
  "qwen3.6-27b": { maxTurns: 25 },
  "qwen3.5-plus": { maxTurns: 35 },
  "qwen3.5-flash": { maxTurns: 20 },
  "qwen3.5-397b-a17b": { maxTurns: 35 },
  "qwen3.5-122b-a10b": { maxTurns: 25 },
  "qwen3.5-35b-a3b": { maxTurns: 20 },
  "qwen3.5-27b": { maxTurns: 20 },
}

export class QwenProvider implements LLMProvider {
  protected client: OpenAI
  protected circuit: CircuitBreaker
  protected maxRetries: number
  protected baseDelay: number
  protected readonly chat = new OpenAIChatAdapter()

  constructor(
    apiKey: string,
    protected readonly model = "qwen3.6-plus",
    retry = { maxRetries: 3, baseDelay: 1000 },
    baseURL: string = QWEN_BASE,
  ) {
    this.client = withServerRuntimeGuard(() => new OpenAI({ apiKey, baseURL }))
    this.circuit = new CircuitBreaker()
    this.maxRetries = retry.maxRetries
    this.baseDelay = retry.baseDelay
  }

  runtimePolicy(): RuntimePolicy {
    return QWEN_POLICIES[this.model] ?? {}
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
    const extraBody = this.thinkingExtraBody(extensions)

    let lastErr: unknown
    for (let i = 0; i < this.maxRetries; i++) {
      try {
        const resp = await this.client.chat.completions.create({
          ...this.requestExtensions(extensions),
          model: this.model,
          messages: msgs,
          ...(tools.length ? { tools: this.chat.buildTools(tools) } : {}),
          ...(extraBody ? { extra_body: extraBody } : {}),
        })
        this.circuit.recordSuccess()
        const choice = resp.choices[0].message
        const toolCalls = this.chat.normalizeToolCalls(choice.tool_calls ?? [])
        return { role: "assistant", content: choice.content ?? "", tokenCount: resp.usage?.completion_tokens ?? resp.usage?.total_tokens, toolCalls }
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
    let reasoningContent = ""
    let finalText = ""

    const extraBody = this.thinkingExtraBody(extensions)
    const stream = await this.client.chat.completions.create({
      ...this.requestExtensions(extensions),
      model: this.model,
      messages: msgs,
      ...(tools.length ? { tools: this.chat.buildTools(tools) } : {}),
      stream: true,
      stream_options: { include_usage: true },
      ...(extraBody ? { extra_body: extraBody } : {}),
    } as OpenAI.ChatCompletionCreateParamsStreaming)

    let totalTokens = 0
    let inputTokens = 0
    let outputTokens = 0
    for await (const chunk of stream) {
      if (chunk.usage) {
        totalTokens = chunk.usage.total_tokens
        inputTokens = chunk.usage.prompt_tokens ?? 0
        outputTokens = chunk.usage.completion_tokens ?? 0
        continue
      }
      const choice = chunk.choices[0]
      if (!choice) continue
      const delta = choice.delta as Record<string, unknown>
      if (!delta) continue
      if (delta.reasoning_content) {
        reasoningContent += String(delta.reasoning_content)
        yield { type: "thinking_delta", delta: delta.reasoning_content } as ThinkingDelta
      }
      if (delta.content) {
        finalText += String(delta.content)
        yield { type: "text_delta", delta: delta.content } as TextDelta
      }
      for (const tc of (delta.tool_calls as OpenAI.ChatCompletionChunk.Choice.Delta.ToolCall[] | undefined) ?? []) {
        const idx = tc.index
        if (!toolCallBufs[idx]) toolCallBufs[idx] = { id: tc.id ?? "", name: "", argsBuf: "" }
        if (tc.function?.name) toolCallBufs[idx].name += tc.function.name
        toolCallBufs[idx].argsBuf += tc.function?.arguments ?? ""
      }
      if (choice.finish_reason === "tool_calls") {
        const toolCalls = Object.values(toolCallBufs).map(tb => ({
          id: tb.id, name: tb.name, arguments: tb.argsBuf || "{}",
        }))
        this.chat.rememberReplayFields({ content: finalText, toolCalls }, { reasoning_content: reasoningContent })
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
    const toolCalls = Object.values(toolCallBufs).map(tb => ({
      id: tb.id, name: tb.name, arguments: tb.argsBuf || "{}",
    }))
    if (toolCalls.length || reasoningContent) {
      this.chat.rememberReplayFields({ content: finalText, toolCalls }, { reasoning_content: reasoningContent })
    }
    for (const [index, tb] of Object.entries(toolCallBufs)) {
      const idx = Number(index)
      if (emittedToolCallIndexes.has(idx)) continue
      let args: Record<string, unknown> = {}
      try { args = JSON.parse(tb.argsBuf || "{}") } catch { args = {} }
      emittedToolCallIndexes.add(idx)
      yield { type: "tool_call", id: tb.id, name: tb.name, arguments: args } as ToolCallEvent
    }
    if (totalTokens > 0) yield { type: "usage", totalTokens, inputTokens, outputTokens } as StreamEvent
  }

  private thinkingExtraBody(extensions?: Record<string, unknown>): Record<string, unknown> | undefined {
    const enableThinking = Boolean(extensions?.enableThinking ?? extensions?.enable_thinking)
    const thinkingBudget = extensions?.thinkingBudget ?? extensions?.thinking_budget
    if (!enableThinking) return undefined
    return {
      enable_thinking: true,
      ...(typeof thinkingBudget === "number" ? { thinking_budget: thinkingBudget } : {}),
    }
  }

  private requestExtensions(extensions?: Record<string, unknown>): Record<string, unknown> {
    return omitExtensionKeys(extensions, [
      "model", "messages", "tools", "stream", "stream_options", "extra_body",
      "enableThinking", "enable_thinking", "thinkingBudget", "thinking_budget",
    ])
  }
}
