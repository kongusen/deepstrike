import OpenAI from "openai"
import type { LLMProvider, Message, StreamEvent, TextDelta, ThinkingDelta, ToolCallEvent, ToolSchema } from "../types.js"
import { withServerRuntimeGuard } from "../runtime/server.js"
import { CircuitBreaker } from "./base.js"
import { OpenAIChatAdapter } from "./openai-chat.js"
import { endpointProfiles } from "./profiles.js"

const QWEN_BASE = (endpointProfiles as Record<string, { baseURL: string }>)["qwen.dashscope"].baseURL

export class QwenProvider implements LLMProvider {
  protected client: OpenAI
  protected circuit: CircuitBreaker
  protected maxRetries: number
  protected baseDelay: number
  protected readonly chat = new OpenAIChatAdapter()

  constructor(
    apiKey: string,
    protected readonly model = "qwen-max",
    retry = { maxRetries: 3, baseDelay: 1000 },
    baseURL: string = QWEN_BASE,
  ) {
    this.client = withServerRuntimeGuard(() => new OpenAI({ apiKey, baseURL }))
    this.circuit = new CircuitBreaker()
    this.maxRetries = retry.maxRetries
    this.baseDelay = retry.baseDelay
  }

  async complete(messages: Message[], tools: ToolSchema[]): Promise<Message> {
    if (this.circuit.isOpen()) throw new Error("Circuit breaker open")
    const msgs = this.chat.buildMessages(messages)

    let lastErr: unknown
    for (let i = 0; i < this.maxRetries; i++) {
      try {
        const resp = await this.client.chat.completions.create({
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

  async *stream(messages: Message[], tools: ToolSchema[], extensions?: Record<string, unknown>): AsyncIterable<StreamEvent> {
    const enableThinking = Boolean(extensions?.enableThinking ?? extensions?.enable_thinking)
    const thinkingBudget = extensions?.thinkingBudget ?? extensions?.thinking_budget
    const msgs = this.chat.buildMessages(messages)
    const toolCallBufs: Record<number, { id: string; name: string; argsBuf: string }> = {}

    const stream = await this.client.chat.completions.create({
      model: this.model,
      messages: msgs,
      ...(tools.length ? { tools: this.chat.buildTools(tools) } : {}),
      stream: true,
      stream_options: { include_usage: true },
      ...(enableThinking ? {
        extra_body: {
          enable_thinking: true,
          ...(typeof thinkingBudget === "number" ? { thinking_budget: thinkingBudget } : {}),
        },
      } : {}),
    } as OpenAI.ChatCompletionCreateParamsStreaming)

    let totalTokens = 0
    for await (const chunk of stream) {
      if (chunk.usage) { totalTokens = chunk.usage.total_tokens; continue }
      const choice = chunk.choices[0]
      if (!choice) continue
      const delta = choice.delta as Record<string, unknown>
      if (delta.reasoning_content) yield { type: "thinking_delta", delta: delta.reasoning_content } as ThinkingDelta
      if (delta.content) yield { type: "text_delta", delta: delta.content } as TextDelta
      for (const tc of (delta.tool_calls as OpenAI.ChatCompletionChunk.Choice.Delta.ToolCall[] | undefined) ?? []) {
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
    if (totalTokens > 0) yield { type: "usage", totalTokens } as StreamEvent
  }
}
