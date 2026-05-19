import type { RenderedContext, ToolSchema, StreamEvent, TextDelta, ThinkingDelta, ToolCallEvent, LLMProvider, Message } from "../types.js"
import { collectStreamMessage, toOpenAIMessages } from "./base.js"

const DEEPSEEK_REASONERS = new Set(["deepseek-reasoner", "deepseek-r1"])
const MINIMAX_REASONERS = new Set(["MiniMax-M1", "minimax-m1"])

// OpenAI-compatible provider — works for OpenAI, Qwen (DashScope), DeepSeek, MiniMax, Kimi
export class OpenAIProvider implements LLMProvider {
  constructor(
    protected readonly apiKey: string,
    protected readonly model = "gpt-4o",
    protected readonly baseUrl = "https://api.openai.com/v1",
  ) {}

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
    })
    if (!resp.ok) throw new Error(`OpenAI ${resp.status}: ${await resp.text()}`)

    const toolAccum: Record<number, { id: string; name: string; argsBuf: string }> = {}
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
        if (!line.startsWith("data: ")) continue
        const data = line.slice(6).trim()
        if (data === "[DONE]") return
        try {
          const chunk = JSON.parse(data) as { choices: Array<{ delta: Record<string, unknown> }> }
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

  async *stream(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): AsyncIterable<StreamEvent> {
    const { expose_reasoning: _, exposeReasoning: __, ...passthrough } = extensions ?? {}
    yield* this.streamInner(context, tools, passthrough)
  }
}

export class QwenProvider extends OpenAIProvider {
  constructor(apiKey: string, model = "qwen-max") {
    super(apiKey, model, "https://dashscope.aliyuncs.com/compatible-mode/v1")
  }

  async *stream(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): AsyncIterable<StreamEvent> {
    const enableThinking = Boolean(extensions?.enableThinking)
    const thinkingBudget = extensions?.thinkingBudget as number | undefined
    const { enableThinking: _, thinkingBudget: __, expose_reasoning: ___, exposeReasoning: ____, ...passthrough } = extensions ?? {}
    const extra: Record<string, unknown> = {
      ...passthrough,
      ...(enableThinking ? { enable_thinking: true, ...(thinkingBudget ? { thinking_budget: thinkingBudget } : {}) } : {}),
    }
    yield* this.streamInner(context, tools, extra, enableThinking)
  }
}

export class DeepSeekProvider extends OpenAIProvider {
  constructor(apiKey: string, model = "deepseek-chat") {
    super(apiKey, model, "https://api.deepseek.com/v1")
  }

  async *stream(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): AsyncIterable<StreamEvent> {
    const exposeReasoning = Boolean(extensions?.exposeReasoning)
    const isReasoner = DEEPSEEK_REASONERS.has(this.model)
    const filteredTools = isReasoner ? [] : tools
    const { exposeReasoning: _, expose_reasoning: __, ...passthrough } = extensions ?? {}
    yield* this.streamInner(context, filteredTools, passthrough, exposeReasoning)
  }
}

export class MiniMaxProvider extends OpenAIProvider {
  constructor(apiKey: string, model = "MiniMax-Text-01") {
    super(apiKey, model, "https://api.minimax.chat/v1")
  }

  async *stream(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): AsyncIterable<StreamEvent> {
    const exposeReasoning = Boolean(extensions?.exposeReasoning)
    const isReasoner = MINIMAX_REASONERS.has(this.model)
    const filteredTools = isReasoner ? [] : tools
    const { exposeReasoning: _, expose_reasoning: __, ...passthrough } = extensions ?? {}
    yield* this.streamInner(context, filteredTools, passthrough, exposeReasoning)
  }
}

export class KimiProvider extends OpenAIProvider {
  constructor(apiKey: string, model = "moonshot-v1-8k") {
    super(apiKey, model, "https://api.moonshot.cn/v1")
  }
}
