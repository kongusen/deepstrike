import type OpenAI from "openai"
import type { Message, ToolSchema } from "../types.js"
import { normalizeToolCall, toOpenAIMessageParams } from "./base.js"

export class OpenAIChatAdapter {
  private replayFields = new Map<string, Record<string, unknown>>()

  buildTools(tools: ToolSchema[]) {
    return tools.map(t => ({
      type: "function" as const,
      function: { name: t.name, description: t.description, parameters: JSON.parse(t.parameters) },
    }))
  }

  buildMessages(messages: Message[]): OpenAI.ChatCompletionMessageParam[] {
    const serialized = toOpenAIMessageParams(messages)
    let cursor = 0

    for (const source of messages) {
      if (source.role === "tool") {
        cursor += (source.contentParts ?? []).filter(p => p.type === "tool_result").length
        continue
      }
      if (source.role === "assistant") {
        const replay = this.replayFields.get(this.assistantReplayKey(source))
        if (replay) serialized[cursor] = { ...serialized[cursor], ...replay }
      }
      cursor += 1
    }

    return serialized as unknown as OpenAI.ChatCompletionMessageParam[]
  }

  normalizeToolCalls(toolCalls: Array<{
    id: string
    function: { name: string; arguments: string }
  }> = []) {
    return toolCalls
      .map(tc => normalizeToolCall(tc.id, tc.function.name, tc.function.arguments))
      .filter(Boolean) as Array<{ id: string; name: string; arguments: string }>
  }

  rememberReplayFields(
    message: Pick<Message, "content" | "toolCalls">,
    fields: Record<string, unknown>,
  ): void {
    this.replayFields.set(this.assistantReplayKey(message), fields)
  }

  private assistantReplayKey(message: Pick<Message, "content" | "toolCalls">): string {
    return JSON.stringify({
      content: message.content,
      toolCalls: message.toolCalls ?? [],
    })
  }
}
