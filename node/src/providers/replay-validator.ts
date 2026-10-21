import type { ContentPart, ModelMessage, ProviderDescriptor, ProviderReplay, RenderedContext, ReplayabilityAssessment } from "../types.js"

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

/**
 * Synthetic tool result output injected for a tool call whose result was never
 * recorded because the run was interrupted before the tool completed. Signals
 * to the model that the action must not be assumed to have taken effect.
 */
export const DEGRADED_TOOL_RESULT_OUTPUT = "[tool call interrupted — no result was recorded before this run ended. Do not assume the action took effect.]"

/** Result of a pre-flight tool-result pairing assessment. */
export interface ToolResultPairingAssessment {
  /** True when every assistant tool-call turn has a matching tool result. */
  ok: boolean
  /** Call ids that have no corresponding tool result in the history. */
  missingCallIds: string[]
}

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
  /**
   * When true, an assistant tool-call turn whose tool results are absent from
   * the history — because the run was interrupted before the tool completed —
   * is patched with a synthetic error result instead of throwing. The context's
   * `turns` array is updated in place so that downstream serialization sees a
   * well-formed message sequence.
   *
   * Analogous to `degradeMissingReasoning` for tool-result pairing.
   */
  degradeMissingToolResult?: boolean
  replayForAssistant?: (message: Pick<ModelMessage, "content" | "toolCalls">) => ProviderReplay | Record<string, unknown> | undefined
}

export function validateOpenAIChatReplay(
  context: RenderedContext,
  options: OpenAIChatReplayValidationOptions = {},
): void {
  // Degrade mode patches missing tool results (interrupted-run recovery) but
  // does NOT suppress orphans/duplicates — those are structural corruption a
  // synthetic result cannot repair. validateStrictToolResultPairing still runs
  // afterwards to catch them.
  if (options.degradeMissingToolResult) {
    const patched = patchMissingToolResults(context.turns)
    if (patched !== context.turns) {
      // Update turns in place so that downstream normalizeCanonicalContext and
      // wire serialization see the synthetic results without requiring callers
      // to thread a new RenderedContext through the call stack.
      context.turns.splice(0, context.turns.length, ...patched)
    }
  }
  validateStrictToolResultPairing(context.turns)
  if (options.requireNonEmptyReasoningForToolCalls && !options.degradeMissingReasoning) {
    const assessment = assessReasoningReplay(context.turns, options)
    if (!assessment.ok) {
      throw reasoningReplayError(assessment.offendingCallIds, options.descriptor)
    }
  }
}

/**
 * Pure, throw-free assessment: which assistant tool-call ids lack a matching
 * tool result in the history. Mirrors `assessReasoningReplay` so an embedder
 * can decide — before sending — whether to degrade, skip, or refuse.
 */
export function assessToolResultPairing(turns: ModelMessage[]): ToolResultPairingAssessment {
  const missingCallIds: string[] = []
  for (let i = 0; i < turns.length; i++) {
    const m = turns[i]!
    if (m.role !== "assistant" || !m.toolCalls?.length) continue
    const answered = new Set<string>()
    for (let j = i + 1; j < turns.length && turns[j]!.role === "tool"; j++) {
      for (const p of toolResultParts(turns[j]!)) answered.add(p.callId)
    }
    for (const tc of m.toolCalls) {
      if (!answered.has(tc.id)) missingCallIds.push(tc.id)
    }
  }
  return { ok: missingCallIds.length === 0, missingCallIds }
}

/**
 * Returns a new `turns` array with synthetic error tool-result messages
 * injected after every assistant tool-call turn whose results are absent.
 * Returns the original array reference unchanged when no patching is needed.
 *
 * Intended for recovery after an interrupted run: the model is told each
 * unanswered call did not complete so it does not assume the action took effect.
 */
export function patchMissingToolResults(turns: ModelMessage[]): ModelMessage[] {
  const out: ModelMessage[] = []
  let changed = false
  let i = 0
  while (i < turns.length) {
    const m = turns[i]!
    out.push(m)
    if (m.role !== "assistant" || !m.toolCalls?.length) { i++; continue }
    // Collect ids answered by the immediately-following run of tool messages.
    const answered = new Set<string>()
    let j = i + 1
    for (; j < turns.length && turns[j]!.role === "tool"; j++) {
      for (const p of toolResultParts(turns[j]!)) answered.add(p.callId)
    }
    // Carry over the existing tool messages, then inject synthetic results
    // for any tool calls that never received a result.
    for (let k = i + 1; k < j; k++) out.push(turns[k]!)
    for (const tc of m.toolCalls) {
      if (answered.has(tc.id)) continue
      out.push({
        role: "tool",
        content: "",
        toolCalls: [],
        contentParts: [{
          type: "tool_result",
          callId: tc.id,
          output: DEGRADED_TOOL_RESULT_OUTPUT,
          isError: true,
        }],
      })
      changed = true
    }
    i = j
  }
  return changed ? out : turns
}

/**
 * Pure, throw-free assessment: which assistant tool-call turns lack the
 * non-empty reasoning replay a reasoning-requiring provider needs. Lets an
 * embedder decide per-candidate whether to keep thinking on, disable it, or
 * skip the candidate — before sending.
 */
export function assessReasoningReplay(
  turns: ModelMessage[],
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

function toolResultParts(message: ModelMessage): Array<Extract<ContentPart, { type: "tool_result" }>> {
  return (message.contentParts ?? [])
    .filter((part): part is Extract<ContentPart, { type: "tool_result" }> => part.type === "tool_result")
}

function validateStrictToolResultPairing(turns: ModelMessage[]): void {
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
