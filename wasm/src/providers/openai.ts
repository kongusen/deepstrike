import type { RenderedContext, ToolSchema, StreamEvent, TextDelta, ThinkingDelta, ToolCallEvent, LLMProvider, Message, ProviderDescriptor } from "../types.js"
import { collectStreamMessage, toOpenAIMessages } from "./base.js"

const DEEPSEEK_REASONERS = new Set(["deepseek-reasoner", "deepseek-r1"])
const MINIMAX_REASONERS = new Set(["MiniMax-M1", "minimax-m1"])

type OpenAIProviderDialect = "openai" | "qwen" | "deepseek" | "minimax"

export interface OpenAIProviderOptions {
  apiKey: string
  model?: string
  baseURL?: string
  provider?: string
  endpointId?: string
  dialect?: OpenAIProviderDialect
}

export interface BackendProviderOptions {
  apiKey: string
  model?: string
  baseURL?: string
}

// OpenAI-compatible provider — works for OpenAI, Qwen (DashScope), DeepSeek, MiniMax, Kimi
export class OpenAIProvider implements LLMProvider {
  protected readonly apiKey: string
  protected readonly model: string
  protected readonly baseUrl: string
  private readonly provider: string
  private readonly endpoint: string
  private readonly dialect: OpenAIProviderDialect

  constructor(options: OpenAIProviderOptions) {
    this.apiKey = options.apiKey
    this.model = options.model ?? "gpt-4o"
    this.baseUrl = options.baseURL ?? "https://api.openai.com/v1"
    this.provider = options.provider ?? "openai"
    this.endpoint = options.endpointId ?? "openai.chat"
    this.dialect = options.dialect ?? "openai"
  }

  protected providerId(): string { return this.provider }
  protected endpointId(): string { return this.endpoint }

  descriptor(): ProviderDescriptor {
    const reasoning = DEEPSEEK_REASONERS.has(this.model) || MINIMAX_REASONERS.has(this.model)
    return {
      provider: this.providerId(),
      protocol: "openai-chat",
      model: this.model,
      reasoning: { supported: reasoning, preserveAcrossToolTurns: reasoning },
      toolCalls: { supported: true, requiresStrictPairing: true },
    }
  }

  requestPlanIdentity() {
    return {
      providerId: this.providerId(),
      modelId: this.model,
      endpoint: { id: this.endpointId(), protocol: "openai-chat" as const, baseURL: this.baseUrl },
    }
  }

  protected buildTools(tools: ToolSchema[]) {
    return tools.map(t => ({ type: "function", function: { name: t.name, description: t.description, parameters: JSON.parse(t.parameters) } }))
  }

  async complete(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): Promise<Message> {
    return collectStreamMessage(this.stream(context, tools, extensions))
  }

  protected async *streamInner(
    context: RenderedContext,
    tools: ToolSchema[],
    extraBody: Record<string, unknown>,
    exposeReasoning = false,
    signal?: AbortSignal,
  ): AsyncIterable<StreamEvent> {
    const body: Record<string, unknown> = {
      model: this.model,
      messages: toOpenAIMessages(context),
      stream: true,
      ...(tools.length ? { tools: this.buildTools(tools) } : {}),
      ...extraBody,
    }

    const resp = await fetch(`${this.baseUrl}/chat/completions`, {
      method: "POST",
      headers: { "Authorization": `Bearer ${this.apiKey}`, "Content-Type": "application/json" },
      body: JSON.stringify(body),
      ...(signal ? { signal } : {}), // #2-B-ii: a preempt aborts the in-flight request at the socket.
    })
    if (!resp.ok) {
      throw Object.assign(new Error(`OpenAI ${resp.status}: ${await resp.text()}`), { status: resp.status })
    }

    const toolAccum: Record<number, { id: string; name: string; argsBuf: string }> = {}
    const reader = resp.body!.getReader()
    const decoder = new TextDecoder()
    let buf = ""
    // Phase 4: OpenAI signals an output-cap truncation via finish_reason="length"; the kernel
    // treats it as a truncation (== Anthropic "max_tokens") and drives output-cap recovery.
    let finishReason: string | undefined

    while (true) {
      const { done, value } = await reader.read()
      if (done) break
      buf += decoder.decode(value, { stream: true })
      const lines = buf.split("\n")
      buf = lines.pop() ?? ""
      for (const line of lines) {
        if (!line.startsWith("data: ")) continue
        const data = line.slice(6).trim()
        if (data === "[DONE]") {
          // Surface an output-cap truncation (finish_reason="length") to the runner/kernel. Emitted
          // as a usage frame (the channel the runner reads stopReason from) before the early return.
          if (finishReason) yield { type: "usage", totalTokens: 0, stopReason: finishReason } as unknown as StreamEvent
          return
        }
        try {
          const chunk = JSON.parse(data) as { choices: Array<{ delta: Record<string, unknown>; finish_reason?: string | null }> }
          if (chunk.choices?.[0]?.finish_reason) finishReason = chunk.choices[0].finish_reason ?? undefined
          const delta = chunk.choices?.[0]?.delta
          if (!delta) continue
          if (exposeReasoning && typeof delta.reasoning_content === "string" && delta.reasoning_content)
            yield { type: "thinking_delta", delta: delta.reasoning_content } as ThinkingDelta
          if (typeof delta.content === "string" && delta.content)
            yield { type: "text_delta", delta: delta.content } as TextDelta
          for (const tc of (delta.tool_calls as Array<Record<string, unknown>> | undefined) ?? []) {
            const idx = tc.index as number
            if (!toolAccum[idx]) toolAccum[idx] = { id: (tc.id as string) ?? "", name: (tc.function as Record<string, string>)?.name ?? "", argsBuf: "" }
            toolAccum[idx].argsBuf += (tc.function as Record<string, string>)?.arguments ?? ""
          }
        } catch { /* skip */ }
      }
    }
    for (const tb of Object.values(toolAccum)) {
      let args: Record<string, unknown> = {}
      try { args = JSON.parse(tb.argsBuf || "{}") } catch { args = {} }
      yield { type: "tool_call", id: tb.id, name: tb.name, arguments: args } as ToolCallEvent
    }
  }

  async *stream(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>, _state?: unknown, signal?: AbortSignal): AsyncIterable<StreamEvent> {
    if (this.dialect === "qwen") {
      const enableThinking = Boolean(extensions?.enableThinking)
      const thinkingBudget = extensions?.thinkingBudget as number | undefined
      const { enableThinking: _, thinkingBudget: __, expose_reasoning: ___, exposeReasoning: ____, ...passthrough } = extensions ?? {}
      yield* this.streamInner(context, tools, {
        ...passthrough,
        ...(enableThinking ? { enable_thinking: true, ...(thinkingBudget ? { thinking_budget: thinkingBudget } : {}) } : {}),
      }, enableThinking, signal)
      return
    }

    if (this.dialect === "deepseek" || this.dialect === "minimax") {
      const exposeReasoning = Boolean(extensions?.exposeReasoning)
      const isReasoner = this.dialect === "deepseek"
        ? DEEPSEEK_REASONERS.has(this.model)
        : MINIMAX_REASONERS.has(this.model)
      const { exposeReasoning: _, expose_reasoning: __, ...passthrough } = extensions ?? {}
      yield* this.streamInner(context, isReasoner ? [] : tools, passthrough, exposeReasoning, signal)
      return
    }

    const { expose_reasoning: _, exposeReasoning: __, ...passthrough } = extensions ?? {}
    yield* this.streamInner(context, tools, passthrough, false, signal)
  }
}

export function qwen(options: BackendProviderOptions): LLMProvider {
  return new OpenAIProvider({
    ...options,
    model: options.model ?? "qwen-max",
    baseURL: options.baseURL ?? "https://dashscope.aliyuncs.com/compatible-mode/v1",
    provider: "qwen",
    endpointId: "qwen.dashscope",
    dialect: "qwen",
  })
}

export function deepseek(options: BackendProviderOptions): LLMProvider {
  return new OpenAIProvider({
    ...options,
    model: options.model ?? "deepseek-chat",
    baseURL: options.baseURL ?? "https://api.deepseek.com/v1",
    provider: "deepseek",
    endpointId: "deepseek.openai",
    dialect: "deepseek",
  })
}

export function minimax(options: BackendProviderOptions): LLMProvider {
  return new OpenAIProvider({
    ...options,
    model: options.model ?? "MiniMax-Text-01",
    baseURL: options.baseURL ?? "https://api.minimax.chat/v1",
    provider: "minimax",
    endpointId: "minimax.openai",
    dialect: "minimax",
  })
}

export function kimi(options: BackendProviderOptions): LLMProvider {
  return new OpenAIProvider({
    ...options,
    model: options.model ?? "moonshot-v1-8k",
    baseURL: options.baseURL ?? "https://api.moonshot.cn/v1",
    provider: "kimi",
    endpointId: "kimi.openai",
  })
}
