import type { RenderedContext, ToolSchema, StreamEvent, TextDelta, ThinkingDelta, ToolCallEvent, UsageEvent, LLMProvider, Message, ProviderReplay } from "../types.js"
import { assistantReplayKey, collectStreamMessage, toAnthropicMessages } from "./base.js"

function buildAnthropicTools(tools: ToolSchema[]) {
  return tools.map((t, i) => ({
    name: t.name,
    description: t.description,
    input_schema: JSON.parse(t.parameters),
    ...(i === tools.length - 1 ? { cache_control: { type: "ephemeral" as const } } : {}),
  }))
}

export class AnthropicProvider implements LLMProvider {
  private nativeAssistantBlocks = new Map<string, Array<Record<string, unknown>>>()

  constructor(
    private readonly apiKey: string,
    private readonly model = "claude-sonnet-4-6",
    private readonly maxTokens = 8096,
  ) {}

  peekProviderReplay(message: Pick<Message, "content" | "toolCalls">): ProviderReplay | undefined {
    const blocks = this.nativeAssistantBlocks.get(assistantReplayKey(message))
    return blocks?.length ? { native_blocks: blocks } : undefined
  }

  seedProviderReplay(message: Pick<Message, "content" | "toolCalls">, replay: ProviderReplay): void {
    if (replay.native_blocks?.length) {
      this.nativeAssistantBlocks.set(assistantReplayKey(message), replay.native_blocks)
    }
  }

  async complete(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): Promise<Message> {
    return collectStreamMessage(this.stream(context, tools, extensions))
  }

  async *stream(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): AsyncIterable<StreamEvent> {
    const systemBlocks: Array<{ type: "text"; text: string; cache_control?: { type: "ephemeral" } }> = []
    if (context.systemStable) {
      systemBlocks.push({ type: "text", text: context.systemStable, cache_control: { type: "ephemeral" } })
    }
    if (context.systemKnowledge) {
      systemBlocks.push({ type: "text", text: context.systemKnowledge, cache_control: { type: "ephemeral" } })
    }
    const system = systemBlocks.length ? systemBlocks : (context.systemText || undefined)
    const msgs = toAnthropicMessages(context, message =>
      this.nativeAssistantBlocks.get(assistantReplayKey(message))
    )

    const body: Record<string, unknown> = {
      model: this.model,
      max_tokens: this.maxTokens,
      messages: msgs,
      stream: true,
      ...(system ? { system } : {}),
      ...(tools.length ? { tools: buildAnthropicTools(tools) } : {}),
    }
    if (extensions?.enable_thinking) {
      body.thinking = { type: "enabled", budget_tokens: 8000 }
    }

    const resp = await fetch("https://api.anthropic.com/v1/messages", {
      method: "POST",
      headers: {
        "x-api-key": this.apiKey,
        "anthropic-version": "2023-06-01",
        "content-type": "application/json",
        "anthropic-beta": "prompt-caching-2024-07-31",
      },
      body: JSON.stringify(body),
    })
    if (!resp.ok) throw new Error(`Anthropic ${resp.status}: ${await resp.text()}`)

    const toolBlocks: Record<number, { id: string; name: string; argsBuf: string }> = {}
    const nativeBlocks: Record<number, Record<string, unknown>> = {}
    let finalText = ""
    const finalToolCalls: Array<{ id: string; name: string; arguments: string }> = []
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
          const evt = JSON.parse(data) as Record<string, unknown>
          if (evt.type === "message_start" || evt.type === "message_delta") {
            const usage = (evt.usage ?? (evt.message as Record<string, unknown> | undefined)?.usage) as
              | { input_tokens?: number; output_tokens?: number }
              | undefined
            if (usage?.input_tokens != null) {
              const inputTokens = usage.input_tokens ?? 0
              const outputTokens = usage.output_tokens ?? 0
              yield {
                type: "usage",
                totalTokens: inputTokens + outputTokens,
                inputTokens,
                outputTokens,
              } as UsageEvent
            }
          } else if (evt.type === "content_block_start") {
            const idx = evt.index as number
            nativeBlocks[idx] = { ...(evt.content_block as Record<string, unknown>) }
            const cb = evt.content_block as Record<string, unknown>
            if (cb.type === "tool_use")
              toolBlocks[idx] = { id: cb.id as string, name: cb.name as string, argsBuf: "" }
          } else if (evt.type === "content_block_delta") {
            const d = evt.delta as Record<string, unknown>
            const idx = evt.index as number
            if (d.type === "text_delta") {
              finalText += String(d.text)
              nativeBlocks[idx] = { ...nativeBlocks[idx], text: String(nativeBlocks[idx]?.text ?? "") + d.text }
              yield { type: "text_delta", delta: d.text } as TextDelta
            } else if (d.type === "thinking_delta") {
              nativeBlocks[idx] = { ...nativeBlocks[idx], thinking: String(nativeBlocks[idx]?.thinking ?? "") + d.thinking }
              yield { type: "thinking_delta", delta: d.thinking } as ThinkingDelta
            } else if (d.type === "signature_delta") {
              nativeBlocks[idx] = { ...nativeBlocks[idx], signature: String(nativeBlocks[idx]?.signature ?? "") + d.signature }
            } else if (d.type === "input_json_delta" && toolBlocks[idx]) {
              toolBlocks[idx].argsBuf += d.partial_json
            }
          } else if (evt.type === "content_block_stop") {
            const idx = evt.index as number
            if (toolBlocks[idx]) {
              const tb = toolBlocks[idx]
              delete toolBlocks[idx]
              let args: Record<string, unknown> = {}
              try { args = JSON.parse(tb.argsBuf || "{}") } catch { args = {} }
              nativeBlocks[idx] = { ...nativeBlocks[idx], input: args }
              finalToolCalls.push({ id: tb.id, name: tb.name, arguments: JSON.stringify(args) })
              yield { type: "tool_call", id: tb.id, name: tb.name, arguments: args } as ToolCallEvent
            }
          }
        } catch { /* skip malformed */ }
      }
    }

    this.rememberNativeBlocks({ content: finalText, toolCalls: finalToolCalls }, Object.keys(nativeBlocks).map(Number).sort((a, b) => a - b).map(index => nativeBlocks[index]))
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
