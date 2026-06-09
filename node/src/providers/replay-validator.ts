import type { ContentPart, Message, ProviderDescriptor, ProviderReplay, RenderedContext } from "../types.js"

export class ProviderReplayValidationError extends Error {
  constructor(message: string) {
    super(message)
    this.name = "ProviderReplayValidationError"
  }
}

export interface OpenAIChatReplayValidationOptions {
  descriptor?: ProviderDescriptor
  requireNonEmptyReasoningForToolCalls?: boolean
  replayForAssistant?: (message: Pick<Message, "content" | "toolCalls">) => ProviderReplay | Record<string, unknown> | undefined
}

export function validateOpenAIChatReplay(
  context: RenderedContext,
  options: OpenAIChatReplayValidationOptions = {},
): void {
  validateStrictToolResultPairing(context.turns)
  if (options.requireNonEmptyReasoningForToolCalls) {
    validateReasoningReplayForAssistantToolCalls(context.turns, options)
  }
}

function toolResultParts(message: Message): Array<Extract<ContentPart, { type: "tool_result" }>> {
  return (message.contentParts ?? [])
    .filter((part): part is Extract<ContentPart, { type: "tool_result" }> => part.type === "tool_result")
}

function validateStrictToolResultPairing(turns: Message[]): void {
  let pendingIds: Set<string> | undefined
  let completedIds = new Set<string>()

  for (const message of turns) {
    if (message.role === "assistant") {
      const toolCalls = message.toolCalls ?? []
      pendingIds = toolCalls.length ? new Set(toolCalls.map(tc => tc.id)) : undefined
      completedIds = new Set()
      continue
    }

    if (message.role !== "tool") {
      pendingIds = undefined
      completedIds = new Set()
      continue
    }

    for (const part of toolResultParts(message)) {
      if (!pendingIds?.has(part.callId)) {
        throw new ProviderReplayValidationError(
          `OpenAI-compatible replay has orphan tool result ${part.callId}: no preceding assistant tool_call with the same id.`,
        )
      }
      if (completedIds.has(part.callId)) {
        throw new ProviderReplayValidationError(
          `OpenAI-compatible replay has duplicate tool result ${part.callId}.`,
        )
      }
      completedIds.add(part.callId)
    }
  }
}

function validateReasoningReplayForAssistantToolCalls(
  turns: Message[],
  options: OpenAIChatReplayValidationOptions,
): void {
  const descriptor = options.descriptor
  for (const message of turns) {
    if (message.role !== "assistant" || !message.toolCalls?.length) continue
    const replay = options.replayForAssistant?.(message)
    const reasoning = typeof replay?.reasoning_content === "string" ? replay.reasoning_content.trim() : ""
    if (!reasoning) {
      const callIds = message.toolCalls.map(tc => tc.id).join(", ")
      const provider = descriptor ? `${descriptor.provider}/${descriptor.model}` : "provider"
      throw new ProviderReplayValidationError(
        `${provider} replay requires non-empty reasoning_content for assistant tool call turn ${callIds}. ` +
        "Disable thinking, rebuild this history with provider replay, or switch to a provider that can replay this turn.",
      )
    }
  }
}
