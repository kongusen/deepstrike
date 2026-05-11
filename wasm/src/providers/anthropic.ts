import type { Message, ToolSchema, StreamEvent, TextDelta, ThinkingDelta, ToolCallEvent, LLMProvider } from "../types.js"

function buildAnthropicTools(tools: ToolSchema[]) {
  return tools.map(t => ({ name: t.name, description: t.description, input_schema: JSON.parse(t.parameters) }))
}

export class AnthropicProvider implements LLMProvider {
  constructor(
    private readonly apiKey: string,
    private readonly model = "claude-sonnet-4-6",
    private readonly maxTokens = 8096,
  ) {}

  async *stream(messages: Message[], tools: ToolSchema[], extensions?: Record<string, unknown>): AsyncIterable<StreamEvent> {
    const system = messages.filter(m => m.role === "system").map(m => m.content).join("\n\n")
    const msgs = messages.filter(m => m.role !== "system").map(m => ({ role: m.role, content: m.content }))

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
      },
      body: JSON.stringify(body),
    })
    if (!resp.ok) throw new Error(`Anthropic ${resp.status}: ${await resp.text()}`)

    const toolBlocks: Record<number, { id: string; name: string; argsBuf: string }> = {}
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
          if (evt.type === "content_block_start") {
            const cb = evt.content_block as Record<string, unknown>
            if (cb.type === "tool_use")
              toolBlocks[evt.index as number] = { id: cb.id as string, name: cb.name as string, argsBuf: "" }
          } else if (evt.type === "content_block_delta") {
            const d = evt.delta as Record<string, unknown>
            const idx = evt.index as number
            if (d.type === "text_delta") yield { type: "text_delta", delta: d.text } as TextDelta
            else if (d.type === "thinking_delta") yield { type: "thinking_delta", delta: d.thinking } as ThinkingDelta
            else if (d.type === "input_json_delta" && toolBlocks[idx]) toolBlocks[idx].argsBuf += d.partial_json
          } else if (evt.type === "content_block_stop" && toolBlocks[evt.index as number]) {
            const tb = toolBlocks[evt.index as number]
            delete toolBlocks[evt.index as number]
            let args: Record<string, unknown> = {}
            try { args = JSON.parse(tb.argsBuf || "{}") } catch { args = {} }
            yield { type: "tool_call", id: tb.id, name: tb.name, arguments: args } as ToolCallEvent
          }
        } catch { /* skip malformed */ }
      }
    }
  }
}
