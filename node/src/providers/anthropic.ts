import Anthropic from "@anthropic-ai/sdk"
import type { Message, ProviderReplay, RenderedContext, ToolSchema, StreamEvent, TextDelta, ThinkingDelta, ToolCallEvent, LLMProvider, RuntimePolicy } from "../types.js"
import { assistantReplayKey } from "../runtime/provider-replay.js"
import { withServerRuntimeGuard } from "../runtime/server.js"
import { CircuitBreaker, normalizeToolCall, omitExtensionKeys, toAnthropicMessages } from "./base.js"

const CLAUDE_POLICIES: Record<string, RuntimePolicy> = {
  "claude-opus-4-1":          { maxTurns: 50 },
  "claude-opus-4-7":          { maxTurns: 50 },
  "claude-opus-4-6":          { maxTurns: 50 },
  "claude-opus-4-0":          { maxTurns: 50 },
  "claude-sonnet-4-6":        { maxTurns: 25 },
  "claude-sonnet-4-0":        { maxTurns: 25 },
  "claude-haiku-4-5":         { maxTurns: 15 },
  "claude-3-5-haiku-latest":  { maxTurns: 15 },
}

interface AnthropicProviderOptions {
  baseURL?: string
  authMode?: "api-key" | "bearer"
}

export class AnthropicProvider implements LLMProvider {
  private client: Anthropic
  private circuit: CircuitBreaker
  private maxRetries: number
  private baseDelay: number
  private nativeAssistantBlocks = new Map<string, Array<Record<string, unknown>>>()

  constructor(
    apiKey: string,
    protected readonly model = "claude-sonnet-4-6",
    retry = { maxRetries: 3, baseDelay: 1000 },
    options: AnthropicProviderOptions = {},
  ) {
    this.client = withServerRuntimeGuard(() => new Anthropic({
      ...(options.authMode === "bearer"
        ? { authToken: apiKey, apiKey: null as unknown as string }
        : { apiKey, authToken: null as unknown as string }),
      ...(options.baseURL ? { baseURL: options.baseURL } : {}),
    }))
    this.circuit = new CircuitBreaker()
    this.maxRetries = retry.maxRetries
    this.baseDelay = retry.baseDelay
  }

  runtimePolicy(): RuntimePolicy {
    return CLAUDE_POLICIES[this.model] ?? {}
  }

  peekProviderReplay(message: Pick<Message, "content" | "toolCalls">): ProviderReplay | undefined {
    const blocks = this.nativeAssistantBlocks.get(assistantReplayKey(message))
    return blocks?.length ? { native_blocks: blocks } : undefined
  }

  seedProviderReplay(message: Pick<Message, "content" | "toolCalls">, replay: ProviderReplay): void {
    if (replay.native_blocks?.length) {
      this.nativeAssistantBlocks.set(assistantReplayKey(message), replay.native_blocks)
    }
  }

  private buildTools(tools: ToolSchema[]) {
    return tools.map((t, i) => ({
      name: t.name,
      description: t.description,
      input_schema: JSON.parse(t.parameters),
      ...(i === tools.length - 1 ? { cache_control: { type: "ephemeral" as const } } : {}),
    }))
  }

  async complete(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): Promise<Message> {
    if (this.circuit.isOpen()) throw new Error("Circuit breaker open")
    const system = this.buildSystem(context)
    const msgs = this.buildMessages(context)
    const requestExtensions = this.requestExtensions(extensions)

    let lastErr: unknown
    for (let i = 0; i < this.maxRetries; i++) {
      try {
        const resp = await this.createMessage({
          ...requestExtensions,
          model: this.model,
          max_tokens: typeof extensions?.max_tokens === "number" ? extensions.max_tokens : 8096,
          ...(system ? { system } : {}),
          messages: msgs,
          ...(tools.length ? { tools: this.buildTools(tools) } : {}),
        }, extensions)
        this.circuit.recordSuccess()
        let content = ""
        const toolCalls = []
        for (const block of resp.content) {
          if (block.type === "text") content += block.text
          else if (block.type === "tool_use") {
            const tc = normalizeToolCall(block.id, block.name, block.input)
            if (tc) toolCalls.push(tc)
          }
        }
        const message = { role: "assistant" as const, content, tokenCount: resp.usage.output_tokens, toolCalls }
        this.rememberNativeBlocks(message, resp.content as unknown as Array<Record<string, unknown>>)
        return message
      } catch (err) {
        lastErr = err
        this.circuit.recordFailure()
        if (i < this.maxRetries - 1) await new Promise(r => setTimeout(r, this.baseDelay * 2 ** i))
      }
    }
    throw lastErr
  }

  async *stream(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): AsyncIterable<StreamEvent> {
    const system = this.buildSystem(context)
    const msgs = this.buildMessages(context)
    const requestExtensions = this.requestExtensions(extensions)
    const toolBlocks: Record<number, { id: string; name: string; argsBuf: string }> = {}
    const nativeBlocks: Record<number, Record<string, unknown>> = {}
    let finalText = ""
    const finalToolCalls: Array<{ id: string; name: string; arguments: string }> = []

    const stream = this.streamMessage({
      ...requestExtensions,
      model: this.model,
      max_tokens: typeof extensions?.max_tokens === "number" ? extensions.max_tokens : 8096,
      ...(system ? { system } : {}),
      messages: msgs,
      ...(tools.length ? { tools: this.buildTools(tools) } : {}),
    }, extensions)

    let totalTokens = 0
    for await (const evt of stream) {
      if (evt.type === "message_start" || evt.type === "message_delta") {
        const usage = evt.usage ?? evt.message?.usage
        if (usage) {
          const inputTokens = usage.input_tokens ?? 0
          const outputTokens = usage.output_tokens ?? 0
          totalTokens = inputTokens + outputTokens
          yield { type: "usage", totalTokens, inputTokens, outputTokens } as StreamEvent
        }
      } else if (evt.type === "content_block_start") {
        nativeBlocks[evt.index] = { ...(evt.content_block as unknown as Record<string, unknown>) }
        if (evt.content_block.type === "tool_use") {
          toolBlocks[evt.index] = { id: evt.content_block.id, name: evt.content_block.name, argsBuf: "" }
        }
      } else if (evt.type === "content_block_delta") {
        const d = evt.delta
        if (d.type === "text_delta") {
          finalText += d.text
          nativeBlocks[evt.index] = { ...nativeBlocks[evt.index], text: String(nativeBlocks[evt.index]?.text ?? "") + d.text }
          yield { type: "text_delta", delta: d.text } as TextDelta
        } else if (d.type === "thinking_delta") {
          nativeBlocks[evt.index] = { ...nativeBlocks[evt.index], thinking: String(nativeBlocks[evt.index]?.thinking ?? "") + d.thinking }
          yield { type: "thinking_delta", delta: d.thinking } as ThinkingDelta
        } else if (d.type === "signature_delta") {
          nativeBlocks[evt.index] = { ...nativeBlocks[evt.index], signature: String(nativeBlocks[evt.index]?.signature ?? "") + d.signature }
        } else if (d.type === "input_json_delta" && toolBlocks[evt.index]) {
          toolBlocks[evt.index].argsBuf += d.partial_json
        }
      } else if (evt.type === "content_block_stop" && toolBlocks[evt.index] !== undefined) {
        const tb = toolBlocks[evt.index]
        delete toolBlocks[evt.index]
        let args: Record<string, unknown> = {}
        try { args = JSON.parse(tb.argsBuf || "{}") } catch { args = {} }
        nativeBlocks[evt.index] = { ...nativeBlocks[evt.index], input: args }
        finalToolCalls.push({ id: tb.id, name: tb.name, arguments: JSON.stringify(args) })
        yield { type: "tool_call", id: tb.id, name: tb.name, arguments: args } as ToolCallEvent
      }
    }

    this.rememberNativeBlocks(
      { content: finalText, toolCalls: finalToolCalls },
      Object.keys(nativeBlocks).map(Number).sort((a, b) => a - b).map(index => nativeBlocks[index]),
    )
  }

  private requestExtensions(extensions?: Record<string, unknown>): Record<string, unknown> {
    return omitExtensionKeys(extensions, ["model", "messages", "system", "tools", "max_tokens", "stream"])
  }

  private hasBetas(extensions?: Record<string, unknown>): boolean {
    const betas = extensions?.betas
    return Array.isArray(betas) && betas.length > 0
  }

  private createMessage(
    params: Record<string, unknown>,
    extensions?: Record<string, unknown>,
  ): Promise<any> {
    return this.hasBetas(extensions)
      ? this.client.beta.messages.create(params as unknown as Parameters<typeof this.client.beta.messages.create>[0])
      : this.client.messages.create(params as unknown as Anthropic.MessageCreateParamsNonStreaming)
  }

  private streamMessage(
    params: Record<string, unknown>,
    extensions?: Record<string, unknown>,
  ): AsyncIterable<any> {
    return (this.hasBetas(extensions)
      ? this.client.beta.messages.stream(params as unknown as Parameters<typeof this.client.beta.messages.stream>[0])
      : this.client.messages.stream(params as unknown as Anthropic.MessageStreamParams)
    ) as unknown as AsyncIterable<any>
  }

  private buildSystem(context: RenderedContext): Array<{ type: "text"; text: string; cache_control?: { type: "ephemeral" } }> | string | undefined {
    if (!context.systemStable && !context.systemKnowledge) {
      return context.systemText || undefined
    }
    const blocks: Array<{ type: "text"; text: string; cache_control: { type: "ephemeral" } }> = []
    if (context.systemStable) {
      blocks.push({ type: "text", text: context.systemStable, cache_control: { type: "ephemeral" } })
    }
    if (context.systemKnowledge) {
      blocks.push({ type: "text", text: context.systemKnowledge, cache_control: { type: "ephemeral" } })
    }
    return blocks.length ? blocks : undefined
  }

  private buildMessages(context: RenderedContext): Anthropic.MessageParam[] {
    const msgs = toAnthropicMessages(context.turns, message =>
      this.nativeAssistantBlocks.get(assistantReplayKey(message))
    ) as unknown as Anthropic.MessageParam[]

    if (msgs.length === 0) {
      msgs.push({ role: "user", content: "Proceed." })
    }

    return msgs
  }

  private rememberNativeBlocks(
    message: Pick<Message, "content" | "toolCalls">,
    blocks: Array<Record<string, unknown>>,
  ): void {
    if (!blocks.length) return
    if (!message.toolCalls?.length && !blocks.some(b => b.type === "thinking")) return
    this.nativeAssistantBlocks.set(assistantReplayKey(message), blocks)
  }
}
