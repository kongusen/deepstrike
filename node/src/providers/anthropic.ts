import Anthropic from "@anthropic-ai/sdk"
import type { Message, ToolSchema, StreamEvent, TextDelta, ThinkingDelta, ToolCallEvent, LLMProvider } from "../types.js"
import { CircuitBreaker, normalizeToolCall } from "./base.js"

export class AnthropicProvider implements LLMProvider {
  private client: Anthropic
  private circuit: CircuitBreaker
  private maxRetries: number
  private baseDelay: number

  constructor(
    apiKey: string,
    private readonly model = "claude-sonnet-4-6",
    retry = { maxRetries: 3, baseDelay: 1000 },
  ) {
    this.client = new Anthropic({ apiKey })
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
    const system = messages.filter(m => m.role === "system").map(m => m.content).join("\n\n")
    const msgs = messages.filter(m => m.role !== "system").map(m => ({ role: m.role as "user" | "assistant", content: m.content }))

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
        return { role: "assistant", content, tokenCount: resp.usage.input_tokens + resp.usage.output_tokens, toolCalls }
      } catch (err) {
        lastErr = err
        this.circuit.recordFailure()
        if (i < this.maxRetries - 1) await new Promise(r => setTimeout(r, this.baseDelay * 2 ** i))
      }
    }
    throw lastErr
  }

  async *stream(messages: Message[], tools: ToolSchema[], extensions?: Record<string, unknown>): AsyncIterable<StreamEvent> {
    const system = messages.filter(m => m.role === "system").map(m => m.content).join("\n\n")
    const msgs = messages.filter(m => m.role !== "system").map(m => ({ role: m.role as "user" | "assistant", content: m.content }))
    const toolBlocks: Record<number, { id: string; name: string; argsBuf: string }> = {}

    const stream = this.client.messages.stream({
      model: this.model,
      max_tokens: 8096,
      ...(system ? { system } : {}),
      messages: msgs,
      ...(tools.length ? { tools: this.buildTools(tools) } : {}),
    })

    for await (const evt of stream) {
      if (evt.type === "content_block_start" && evt.content_block.type === "tool_use") {
        toolBlocks[evt.index] = { id: evt.content_block.id, name: evt.content_block.name, argsBuf: "" }
      } else if (evt.type === "content_block_delta") {
        const d = evt.delta
        if (d.type === "text_delta") {
          yield { type: "text_delta", delta: d.text } as TextDelta
        } else if (d.type === "thinking_delta") {
          yield { type: "thinking_delta", delta: d.thinking } as ThinkingDelta
        } else if (d.type === "input_json_delta" && toolBlocks[evt.index]) {
          toolBlocks[evt.index].argsBuf += d.partial_json
        }
      } else if (evt.type === "content_block_stop" && toolBlocks[evt.index] !== undefined) {
        const tb = toolBlocks[evt.index]
        delete toolBlocks[evt.index]
        let args: Record<string, unknown> = {}
        try { args = JSON.parse(tb.argsBuf || "{}") } catch { args = {} }
        yield { type: "tool_call", id: tb.id, name: tb.name, arguments: args } as ToolCallEvent
      }
    }
  }
}
