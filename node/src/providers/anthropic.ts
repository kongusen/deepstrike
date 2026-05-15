import Anthropic from "@anthropic-ai/sdk"
import type { Message, ToolSchema, StreamEvent, TextDelta, ThinkingDelta, ToolCallEvent, LLMProvider } from "../types.js"
import { withServerRuntimeGuard } from "../runtime/server.js"
import { CircuitBreaker, normalizeToolCall, splitAnthropicSystem, toAnthropicMessages } from "./base.js"

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
      ...(options.authMode === "bearer" ? { authToken: apiKey } : { apiKey }),
      ...(options.baseURL ? { baseURL: options.baseURL } : {}),
    }))
    this.circuit = new CircuitBreaker()
    this.maxRetries = retry.maxRetries
    this.baseDelay = retry.baseDelay
  }

  private buildTools(tools: ToolSchema[]) {
    return tools.map(t => ({
      name: t.name,
      description: t.description,
      input_schema: JSON.parse(t.parameters),
    }))
  }

  async complete(messages: Message[], tools: ToolSchema[]): Promise<Message> {
    if (this.circuit.isOpen()) throw new Error("Circuit breaker open")
    const system = splitAnthropicSystem(messages)
    const msgs = this.buildMessages(messages)

    let lastErr: unknown
    for (let i = 0; i < this.maxRetries; i++) {
      try {
        const resp = await this.client.messages.create({
          model: this.model,
          max_tokens: 8096,
          ...(system ? { system } : {}),
          messages: msgs,
          ...(tools.length ? { tools: this.buildTools(tools) } : {}),
        })
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
        const message = { role: "assistant" as const, content, tokenCount: resp.usage.input_tokens + resp.usage.output_tokens, toolCalls }
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

  async *stream(messages: Message[], tools: ToolSchema[], extensions?: Record<string, unknown>): AsyncIterable<StreamEvent> {
    const system = splitAnthropicSystem(messages)
    const msgs = this.buildMessages(messages)
    const toolBlocks: Record<number, { id: string; name: string; argsBuf: string }> = {}
    const nativeBlocks: Record<number, Record<string, unknown>> = {}
    let finalText = ""
    const finalToolCalls: Array<{ id: string; name: string; arguments: string }> = []

    const stream = this.client.messages.stream({
      model: this.model,
      max_tokens: 8096,
      ...(system ? { system } : {}),
      messages: msgs,
      ...(tools.length ? { tools: this.buildTools(tools) } : {}),
    })

    for await (const evt of stream) {
      if (evt.type === "content_block_start") {
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

  private buildMessages(messages: Message[]): Anthropic.MessageParam[] {
    return toAnthropicMessages(messages, message =>
      this.nativeAssistantBlocks.get(this.assistantReplayKey(message))
    ) as unknown as Anthropic.MessageParam[]
  }

  private rememberNativeBlocks(
    message: Pick<Message, "content" | "toolCalls">,
    blocks: Array<Record<string, unknown>>,
  ): void {
    if (!message.toolCalls?.length) return
    this.nativeAssistantBlocks.set(this.assistantReplayKey(message), blocks)
  }

  private assistantReplayKey(message: Pick<Message, "content" | "toolCalls">): string {
    return JSON.stringify({
      content: message.content,
      toolCalls: message.toolCalls ?? [],
    })
  }
}
