import type { ContentPart, Message, ProviderDescriptor, ProviderReplay, RenderedContext, ReplayabilityAssessment } from "../types.js"

export type { ReplayabilityAssessment }

export class ProviderReplayValidationError extends Error {
  constructor(message: string) {
    super(message)
    this.name = "ProviderReplayValidationError"
  }
}

/**
 * Placeholder reasoning injected for an assistant tool-call turn that has no
 * stored reasoning replay when the caller opted into graceful degradation
 * (`degradeMissingReasoning`). It keeps the wire message well-formed for a
 * thinking-on provider without fabricating substantive reasoning.
 */
export const DEGRADED_REASONING_PLACEHOLDER = "[reasoning unavailable on replay]"

export interface OpenAIChatReplayValidationOptions {
  descriptor?: ProviderDescriptor
  requireNonEmptyReasoningForToolCalls?: boolean
  /**
   * When true, an assistant tool-call turn that lacks reasoning replay is
   * degraded (serialized without/with a placeholder reasoning) instead of
   * throwing. Lets a recovery/fallback request succeed in degraded form
   * rather than fail outright.
   */
  degradeMissingReasoning?: boolean
  replayForAssistant?: (message: Pick<Message, "content" | "toolCalls">) => ProviderReplay | Record<string, unknown> | undefined
}

export function validateOpenAIChatReplay(
  context: RenderedContext,
  options: OpenAIChatReplayValidationOptions = {},
): void {
  validateStrictToolResultPairing(context.turns)
  if (options.requireNonEmptyReasoningForToolCalls && !options.degradeMissingReasoning) {
    const assessment = assessReasoningReplay(context.turns, options)
    if (!assessment.ok) {
      throw reasoningReplayError(assessment.offendingCallIds, options.descriptor)
    }
  }
}

/**
 * Pure, throw-free assessment: which assistant tool-call turns lack the
 * non-empty reasoning replay a reasoning-requiring provider needs. Lets an
 * embedder decide per-candidate whether to keep thinking on, disable it, or
 * skip the candidate — before sending.
 */
export function assessReasoningReplay(
  turns: Message[],
  options: Pick<OpenAIChatReplayValidationOptions, "replayForAssistant">,
): ReplayabilityAssessment {
  const offendingCallIds: string[] = []
  for (const message of turns) {
    if (message.role !== "assistant" || !message.toolCalls?.length) continue
    const replay = options.replayForAssistant?.(message)
    const reasoning = typeof replay?.reasoning_content === "string" ? replay.reasoning_content.trim() : ""
    if (!reasoning) {
      for (const tc of message.toolCalls) offendingCallIds.push(tc.id)
    }
  }
  return { ok: offendingCallIds.length === 0, offendingCallIds }
}

function reasoningReplayError(
  callIds: string[],
  descriptor: ProviderDescriptor | undefined,
): ProviderReplayValidationError {
  const provider = descriptor ? `${descriptor.provider}/${descriptor.model}` : "provider"
  return new ProviderReplayValidationError(
    `${provider} replay requires non-empty reasoning_content for assistant tool call turn ${callIds.join(", ")}. ` +
    "Disable thinking, rebuild this history with provider replay, switch to a provider that can replay this turn, " +
    "or pass extensions.degradeMissingReasoningReplay to send a degraded turn.",
  )
}

function toolResultParts(message: Message): Array<Extract<ContentPart, { type: "tool_result" }>> {
  return (message.contentParts ?? [])
    .filter((part): part is Extract<ContentPart, { type: "tool_result" }> => part.type === "tool_result")
}

function validateStrictToolResultPairing(turns: Message[]): void {
  let pendingIds: Set<string> | undefined
  let completedIds = new Set<string>()

  const assertAllCompleted = () => {
    if (!pendingIds) return
    const missing = [...pendingIds].filter(id => !completedIds.has(id))
    if (missing.length) {
      throw new ProviderReplayValidationError(
        `OpenAI-compatible replay has assistant tool_calls with no tool result for ${missing.join(", ")}: ` +
        "every tool_call must be answered by a tool message before the next assistant or user turn.",
      )
    }
  }

  for (const message of turns) {
    if (message.role === "assistant") {
      assertAllCompleted()
      const toolCalls = message.toolCalls ?? []
      pendingIds = toolCalls.length ? new Set(toolCalls.map(tc => tc.id)) : undefined
      completedIds = new Set()
      continue
    }

    if (message.role !== "tool") {
      assertAllCompleted()
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

  assertAllCompleted()
}
