import OpenAI from "openai"
import type { Message, ProviderDescriptor, RenderedContext, ToolSchema, StreamEvent, TextDelta, ThinkingDelta, ToolCallEvent, RuntimePolicy } from "../types.js"
import { OpenAIChatProvider } from "./openai.js"
import { endpointProfiles } from "./profiles.js"
import { omitExtensionKeys } from "./base.js"

const DEEPSEEK_BASE = endpointProfiles["deepseek.openai"].baseURL

const DEEPSEEK_POLICIES: Record<string, RuntimePolicy> = {
  "deepseek-chat":      { maxTurns: 25 },
  "deepseek-reasoner":  { maxTurns: 50 },
  "deepseek-v4-flash":  { maxTurns: 20 },
  "deepseek-v4-pro":    { maxTurns: 35 },
}

export class DeepSeekProvider extends OpenAIChatProvider {
  constructor(
    apiKey: string,
    model: string = "deepseek-v4-flash",
    retry?: { maxRetries: number; baseDelay: number },
    baseURL: string = DEEPSEEK_BASE,
  ) {
    super(apiKey, model, retry, baseURL)
  }

  override runtimePolicy(): RuntimePolicy {
    return DEEPSEEK_POLICIES[this.model] ?? {}
  }

  override descriptor(): ProviderDescriptor {
    return {
      provider: "deepseek",
      protocol: "openai-chat",
      model: this.model,
      reasoning: {
        supported: true,
        preserveAcrossToolTurns: true,
        requiresReplayForToolTurns: true,
      },
      toolCalls: {
        supported: true,
        requiresStrictPairing: true,
      },
    }
  }

  protected override requireNonEmptyReasoningReplayForToolTurns(extensions?: Record<string, unknown>): boolean {
    if (extensions?.__deepstrikeThinkingEnabled === false) return false
    return extensions?.thinking !== false
  }

  async complete(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): Promise<Message> {
    const thinking = extensions?.thinking === false ? "disabled" : "enabled"
    const thinkingEnabled = thinking !== "disabled"
    const reasoningEffort = extensions?.reasoningEffort === "max" ? "max" : "high"
    const requestExtensions = {
      ...omitExtensionKeys(extensions, ["thinking", "reasoningEffort", "exposeReasoning", "extra_body", "reasoning_effort"]),
      __deepstrikeThinkingEnabled: thinkingEnabled,
      // Re-thread the degrade control flag (omitExtensionKeys strips internal
      // keys) so buildChatMessages can honor it; the wire-request omit drops it.
      ...(extensions?.degradeMissingReasoningReplay === true ? { degradeMissingReasoningReplay: true } : {}),
      reasoning_effort: reasoningEffort,
      extra_body: { thinking: { type: thinking } },
    }
    if (this.circuit.isOpen()) throw new Error("Circuit breaker open")
    const msgs = this.buildChatMessages(context, requestExtensions)

    let lastErr: unknown
    for (let i = 0; i < this.maxRetries; i++) {
      try {
        const resp = await this.client.chat.completions.create({
          ...this.requestExtensions(requestExtensions),
          model: this.model,
          messages: msgs,
          ...(tools.length ? { tools: this.chat.buildTools(tools) } : {}),
        })
        this.circuit.recordSuccess()
        const choice = resp.choices[0].message as OpenAI.ChatCompletionMessage & Record<string, unknown>
        const nativeToolCalls = choice.tool_calls ?? []
        const toolCalls = this.chat.normalizeToolCalls(nativeToolCalls)
        const content = choice.content ?? ""
        this.rememberDeepSeekReplay(content, toolCalls, choice.reasoning_content, nativeToolCalls)
        return { role: "assistant", content, tokenCount: resp.usage?.completion_tokens ?? resp.usage?.total_tokens, toolCalls }
      } catch (err) {
        lastErr = err
        this.circuit.recordFailure()
        if (i < this.maxRetries - 1) await new Promise(r => setTimeout(r, this.baseDelay * 2 ** i))
      }
    }
    throw lastErr
  }

  async *stream(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): AsyncIterable<StreamEvent> {
    const exposeReasoning = extensions?.exposeReasoning ?? false
    const thinking = extensions?.thinking === false ? "disabled" : "enabled"
    const reasoningEffort = extensions?.reasoningEffort === "max" ? "max" : "high"
    const msgs = this.buildChatMessages(context, extensions)
    const toolCallBufs: Record<number, { id: string; name: string; argsBuf: string }> = {}
    const emittedToolCallIndexes = new Set<number>()
    let reasoningContent = ""
    let finalText = ""

    const stream = await this.client.chat.completions.create({
      ...omitExtensionKeys(extensions, [
        "model", "messages", "tools", "stream", "stream_options", "extra_body", "reasoning_effort",
        "exposeReasoning", "thinking", "reasoningEffort", "__deepstrikeThinkingEnabled",
      ]),
      model: this.model,
      messages: msgs,
      ...(tools.length ? { tools: this.chat.buildTools(tools) } : {}),
      stream: true,
      stream_options: { include_usage: true },
      reasoning_effort: reasoningEffort,
      extra_body: { thinking: { type: thinking } },
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
      if (exposeReasoning && delta.reasoning_content) {
        yield { type: "thinking_delta", delta: delta.reasoning_content } as ThinkingDelta
      }
      if (delta.reasoning_content) reasoningContent += String(delta.reasoning_content)
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
        this.rememberDeepSeekReplay(finalText, toolCalls, reasoningContent, nativeToolCallsFromBuffers(toolCallBufs))
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
    this.rememberDeepSeekReplay(finalText, toolCalls, reasoningContent, nativeToolCallsFromBuffers(toolCallBufs))
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

  private rememberDeepSeekReplay(
    content: string,
    toolCalls: Array<{ id: string; name: string; arguments: string }>,
    reasoningContent: unknown,
    nativeToolCalls: unknown[],
  ): void {
    if (typeof reasoningContent !== "string" || !reasoningContent.trim()) return
    this.chat.rememberReplayFields({ content, toolCalls }, {
      schema_version: 2,
      provider: "deepseek",
      protocol: "openai-chat",
      model: this.model,
      reasoning_content: reasoningContent,
      ...(nativeToolCalls.length ? { tool_calls: nativeToolCalls } : {}),
    })
  }
}

function nativeToolCallsFromBuffers(
  toolCallBufs: Record<number, { id: string; name: string; argsBuf: string }>,
): Array<Record<string, unknown>> {
  return Object.values(toolCallBufs).map(tb => ({
    id: tb.id,
    type: "function",
    function: { name: tb.name, arguments: tb.argsBuf || "{}" },
  }))
}
