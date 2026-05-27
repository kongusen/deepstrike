import OpenAI from "openai"
import type { Message, RenderedContext, ToolSchema, StreamEvent, TextDelta, ThinkingDelta, ToolCallEvent, RuntimePolicy } from "../types.js"
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

  async complete(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): Promise<Message> {
    const thinking = extensions?.thinking === false ? "disabled" : "enabled"
    const reasoningEffort = extensions?.reasoningEffort === "max" ? "max" : "high"
    return super.complete(context, tools, {
      ...omitExtensionKeys(extensions, ["thinking", "reasoningEffort", "exposeReasoning", "extra_body", "reasoning_effort"]),
      reasoning_effort: reasoningEffort,
      extra_body: { thinking: { type: thinking } },
    })
  }

  async *stream(context: RenderedContext, tools: ToolSchema[], extensions?: Record<string, unknown>): AsyncIterable<StreamEvent> {
    const exposeReasoning = extensions?.exposeReasoning ?? false
    const thinking = extensions?.thinking === false ? "disabled" : "enabled"
    const reasoningEffort = extensions?.reasoningEffort === "max" ? "max" : "high"
    const msgs = this.chat.buildMessages(context)
    const toolCallBufs: Record<number, { id: string; name: string; argsBuf: string }> = {}
    const emittedToolCallIndexes = new Set<number>()
    let reasoningContent = ""
    let finalText = ""

    const stream = await this.client.chat.completions.create({
      ...omitExtensionKeys(extensions, [
        "model", "messages", "tools", "stream", "extra_body", "reasoning_effort",
        "exposeReasoning", "thinking", "reasoningEffort",
      ]),
      model: this.model,
      messages: msgs,
      ...(tools.length ? { tools: this.chat.buildTools(tools) } : {}),
      stream: true,
      reasoning_effort: reasoningEffort,
      extra_body: { thinking: { type: thinking } },
    } as OpenAI.ChatCompletionCreateParamsStreaming)

    for await (const chunk of stream) {
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
        this.chat.rememberReplayFields({ content: finalText, toolCalls }, { reasoning_content: reasoningContent })
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
    if (toolCalls.length || reasoningContent) {
      this.chat.rememberReplayFields({ content: finalText, toolCalls }, { reasoning_content: reasoningContent })
    }
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
