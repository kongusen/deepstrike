import type OpenAI from "openai"
import type { Message, ProviderDescriptor, RenderedContext, ToolSchema } from "../types.js"
import { assistantReplayKey } from "../runtime/provider-replay.js"
import { normalizeToolCall, toOpenAIMessageParams } from "./base.js"
import { validateOpenAIChatReplay } from "./replay-validator.js"

export interface OpenAIChatBuildMessageOptions {
  descriptor?: ProviderDescriptor
  requireNonEmptyReasoningForToolCalls?: boolean
}

export class OpenAIChatAdapter {
  private replayFields = new Map<string, Record<string, unknown>>()

  buildTools(tools: ToolSchema[]) {
    return tools.map(t => ({
      type: "function" as const,
      function: { name: t.name, description: t.description, parameters: JSON.parse(t.parameters) },
    }))
  }

  buildMessages(context: RenderedContext, options: OpenAIChatBuildMessageOptions = {}): OpenAI.ChatCompletionMessageParam[] {
    validateOpenAIChatReplay(context, {
      descriptor: options.descriptor,
      requireNonEmptyReasoningForToolCalls: options.requireNonEmptyReasoningForToolCalls,
      replayForAssistant: message => this.replayFields.get(assistantReplayKey(message)),
    })
    // toOpenAIMessageParams prepends systemText as messages[0], then turns.
    const serialized = toOpenAIMessageParams(context)
    // Cursor starts at 1 to skip the system message injected by toOpenAIMessageParams.
    let cursor = context.systemText ? 1 : 0

    for (const source of context.turns) {
      if (source.role === "tool") {
        cursor += (source.contentParts ?? []).filter(p => p.type === "tool_result").length
        continue
      }
      if (source.role === "assistant") {
        const replay = this.replayFields.get(assistantReplayKey(source))
        const wireReplay = openAIChatWireReplayFields(replay)
        if (wireReplay) serialized[cursor] = { ...serialized[cursor], ...wireReplay }
      }
      cursor += 1
    }

    return serialized as unknown as OpenAI.ChatCompletionMessageParam[]
  }

  normalizeToolCalls(toolCalls: OpenAI.ChatCompletionMessageToolCall[] = []) {
    return toolCalls
      .filter((tc): tc is OpenAI.ChatCompletionMessageFunctionToolCall => tc.type === "function")
      .map(tc => normalizeToolCall(tc.id, tc.function.name, tc.function.arguments))
      .filter(Boolean) as Array<{ id: string; name: string; arguments: string }>
  }

  rememberReplayFields(
    message: Pick<Message, "content" | "toolCalls">,
    fields: Record<string, unknown>,
  ): void {
    this.replayFields.set(assistantReplayKey(message), fields)
  }

  peekReplayFields(message: Pick<Message, "content" | "toolCalls">): Record<string, unknown> | undefined {
    return this.replayFields.get(assistantReplayKey(message))
  }
}

function openAIChatWireReplayFields(replay: Record<string, unknown> | undefined): Record<string, unknown> | undefined {
  if (!replay) return undefined
  const fields: Record<string, unknown> = {}
  if (typeof replay.reasoning_content === "string") fields.reasoning_content = replay.reasoning_content
  if (replay.reasoning_details !== undefined) fields.reasoning_details = replay.reasoning_details
  return Object.keys(fields).length ? fields : undefined
}
