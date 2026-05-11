import type { Message, ToolSchema, StreamEvent, TextDelta, ToolCallEvent, LLMProvider } from "../types.js"

// OpenAI-compatible provider — works for OpenAI, Qwen (DashScope), DeepSeek, MiniMax
export class OpenAIProvider implements LLMProvider {
  constructor(
    private readonly apiKey: string,
    private readonly model = "gpt-4o",
    private readonly baseUrl = "https://api.openai.com/v1",
  ) {}

  async *stream(messages: Message[], tools: ToolSchema[], extensions?: Record<string, unknown>): AsyncIterable<StreamEvent> {
    const msgs = messages.map(m => ({ role: m.role, content: m.content }))
    const body: Record<string, unknown> = {
      model: this.model,
      messages: msgs,
      stream: true,
      ...(tools.length ? { tools: tools.map(t => ({ type: "function", function: { name: t.name, description: t.description, parameters: JSON.parse(t.parameters) } })) } : {}),
      ...(extensions ?? {}),
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
}

export class QwenProvider extends OpenAIProvider {
  constructor(apiKey: string, model = "qwen-max") {
    super(apiKey, model, "https://dashscope.aliyuncs.com/compatible-mode/v1")
  }
}

export class DeepSeekProvider extends OpenAIProvider {
  constructor(apiKey: string, model = "deepseek-chat") {
    super(apiKey, model, "https://api.deepseek.com/v1")
  }
}

export class MiniMaxProvider extends OpenAIProvider {
  constructor(apiKey: string, model = "MiniMax-Text-01") {
    super(apiKey, model, "https://api.minimax.chat/v1")
  }
}
