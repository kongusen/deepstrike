import OpenAI from "openai"
import type { Message, ToolSchema, StreamEvent, TextDelta, ThinkingDelta, ToolCallEvent, LLMProvider } from "../types.js"
import { CircuitBreaker, normalizeToolCall } from "./base.js"

export class OpenAIProvider implements LLMProvider {
  protected client: OpenAI
  protected circuit: CircuitBreaker
  protected maxRetries: number
  protected baseDelay: number

  constructor(
    apiKey: string,
    protected readonly model = "gpt-4o",
    retry = { maxRetries: 3, baseDelay: 1000 },
    baseURL = "https://api.openai.com/v1",
  ) {
    this.client = new OpenAI({ apiKey, baseURL })
    this.circuit = new CircuitBreaker()
    this.maxRetries = retry.maxRetries
    this.baseDelay = retry.baseDelay
  }

  protected buildTools(tools: ToolSchema[]) {
    return tools.map(t => ({
      type: "function" as const,
      function: { name: t.name, description: t.description, parameters: JSON.parse(t.parameters) },
    }))
  }

  async complete(messages: Message[], tools: ToolSchema[]): Promise<Message> {
    if (this.circuit.isOpen()) throw new Error("Circuit breaker open")
    const msgs = messages.map(m => ({ role: m.role, content: m.content })) as OpenAI.ChatCompletionMessageParam[]

    let lastErr: unknown
    for (let i = 0; i < this.maxRetries; i++) {
      try {
        const resp = await this.client.chat.completions.create({
          model: this.model,
          messages: msgs,
          ...(tools.length ? { tools: this.buildTools(tools) } : {}),
        })
        this.circuit.recordSuccess()
        const choice = resp.choices[0].message
        const toolCalls = (choice.tool_calls ?? []).map(tc =>
          normalizeToolCall(tc.id, tc.function.name, tc.function.arguments)
        ).filter(Boolean) as { id: string; name: string; arguments: string }[]
        return { role: "assistant", content: choice.content ?? "", tokenCount: resp.usage?.total_tokens, toolCalls }
      } catch (err) {
        lastErr = err
        this.circuit.recordFailure()
        if (i < this.maxRetries - 1) await new Promise(r => setTimeout(r, this.baseDelay * 2 ** i))
      }
    }
    throw lastErr
  }

  async *stream(messages: Message[], tools: ToolSchema[], extensions?: Record<string, unknown>): AsyncIterable<StreamEvent> {
    const msgs = messages.map(m => ({ role: m.role, content: m.content })) as OpenAI.ChatCompletionMessageParam[]
    const toolCallBufs: Record<number, { id: string; name: string; argsBuf: string }> = {}

    const stream = await this.client.chat.completions.create({
      model: this.model,
      messages: msgs,
      ...(tools.length ? { tools: this.buildTools(tools) } : {}),
      stream: true,
    })

    for await (const chunk of stream) {
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
        for (const tb of Object.values(toolCallBufs)) {
          let args: Record<string, unknown> = {}
          try { args = JSON.parse(tb.argsBuf || "{}") } catch { args = {} }
          yield { type: "tool_call", id: tb.id, name: tb.name, arguments: args } as ToolCallEvent
        }
      }
    }
  }
}

const DASHSCOPE_BASE = "https://dashscope.aliyuncs.com/compatible-mode/v1"
const DEEPSEEK_BASE = "https://api.deepseek.com/v1"
const MINIMAX_BASE = "https://api.minimax.chat/v1"
const DEEPSEEK_REASONERS = new Set(["deepseek-reasoner", "deepseek-r1"])
const MINIMAX_REASONERS = new Set(["MiniMax-M1", "minimax-m1"])

export class QwenProvider extends OpenAIProvider {
  constructor(apiKey: string, model = "qwen-plus", retry?: { maxRetries: number; baseDelay: number }) {
    super(apiKey, model, retry, DASHSCOPE_BASE)
  }
}

export class DeepSeekProvider extends OpenAIProvider {
  constructor(apiKey: string, model = "deepseek-chat", retry?: { maxRetries: number; baseDelay: number }) {
    super(apiKey, model, retry, DEEPSEEK_BASE)
  }

  async *stream(messages: Message[], tools: ToolSchema[], extensions?: Record<string, unknown>): AsyncIterable<StreamEvent> {
    const exposeReasoning = extensions?.exposeReasoning ?? false
    const msgs = messages.map(m => ({ role: m.role, content: m.content })) as OpenAI.ChatCompletionMessageParam[]
    const isReasoner = DEEPSEEK_REASONERS.has(this.model)

    const stream = await this.client.chat.completions.create({
      model: this.model,
      messages: msgs,
      ...(!isReasoner && tools.length ? { tools: this.buildTools(tools) } : {}),
      stream: true,
    } as OpenAI.ChatCompletionCreateParamsStreaming)

    for await (const chunk of stream) {
      const delta = chunk.choices[0]?.delta as Record<string, unknown>
      if (exposeReasoning && delta.reasoning_content) {
        yield { type: "thinking_delta", delta: delta.reasoning_content } as ThinkingDelta
      }
      if (delta.content) yield { type: "text_delta", delta: delta.content } as TextDelta
    }
  }
}

export class MiniMaxProvider extends OpenAIProvider {
  constructor(apiKey: string, model = "MiniMax-Text-01", retry?: { maxRetries: number; baseDelay: number }) {
    super(apiKey, model, retry, MINIMAX_BASE)
  }

  async *stream(messages: Message[], tools: ToolSchema[], extensions?: Record<string, unknown>): AsyncIterable<StreamEvent> {
    const exposeReasoning = extensions?.exposeReasoning ?? false
    const msgs = messages.map(m => ({ role: m.role, content: m.content })) as OpenAI.ChatCompletionMessageParam[]
    const isReasoner = MINIMAX_REASONERS.has(this.model)

    const stream = await this.client.chat.completions.create({
      model: this.model,
      messages: msgs,
      ...(!isReasoner && tools.length ? { tools: this.buildTools(tools) } : {}),
      stream: true,
    } as OpenAI.ChatCompletionCreateParamsStreaming)

    for await (const chunk of stream) {
      const delta = chunk.choices[0]?.delta as Record<string, unknown>
      if (exposeReasoning && delta.reasoning_content) {
        yield { type: "thinking_delta", delta: delta.reasoning_content } as ThinkingDelta
      }
      if (delta.content) yield { type: "text_delta", delta: delta.content } as TextDelta
    }
  }
}
