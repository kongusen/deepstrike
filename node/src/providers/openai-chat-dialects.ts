import type { ProviderDescriptor, ProviderReplay, ToolCall } from "../types.js"
import type { EndpointProfileId, ProviderId } from "./endpoints.js"

export interface OpenAIChatTurnReasoning {
  reasoningContent: string
  reasoningDetails?: unknown
  nativeToolCalls: unknown[]
}

export type OpenAIChatReplayStrategy = "none" | "generic_stream" | "deepseek" | "minimax"

export interface OpenAIChatWireDialect {
  readonly id: string
  readonly providerId: ProviderId
  readonly endpointId: EndpointProfileId
  readonly descriptor: { reasoning: ProviderDescriptor["reasoning"] }
  readonly prepareExtensions: (extensions: Readonly<Record<string, unknown>>) => Record<string, unknown>
  readonly serverTools?: (extensions: Readonly<Record<string, unknown>>) => unknown[]
  readonly cacheKey: "openai" | "none"
  readonly inlineThinkingTags: boolean
  readonly exposeReasoning: (extensions: Readonly<Record<string, unknown>>) => boolean
  readonly requireReasoningReplay: (extensions: Readonly<Record<string, unknown>>) => boolean
  readonly replay: OpenAIChatReplayStrategy
}

function omit(
  extensions: Readonly<Record<string, unknown>>,
  keys: readonly string[],
): Record<string, unknown> {
  const blocked = new Set([
    ...keys,
    "model", "messages", "tools", "stream", "stream_options",
    "__deepstrikeThinkingEnabled", "degradeMissingReasoningReplay",
  ])
  return Object.fromEntries(Object.entries(extensions).filter(([key]) => !blocked.has(key)))
}

const portableReasoning: ProviderDescriptor["reasoning"] = {
  supported: true,
  preserveAcrossToolTurns: false,
}

const outOfBandReasoning: ProviderDescriptor["reasoning"] = {
  supported: true,
  preserveAcrossToolTurns: true,
}

const strictReasoning: ProviderDescriptor["reasoning"] = {
  supported: true,
  preserveAcrossToolTurns: true,
  requiresReplayForToolTurns: true,
}

const passthrough = (extensions: Readonly<Record<string, unknown>>) => omit(extensions, [])
const never = () => false
const always = () => true

export const openAIChatDialects = {
  openai: {
    id: "openai",
    providerId: "openai",
    endpointId: "openai.chat",
    descriptor: { reasoning: portableReasoning },
    prepareExtensions: passthrough,
    cacheKey: "openai",
    inlineThinkingTags: true,
    exposeReasoning: always,
    requireReasoningReplay: never,
    replay: "generic_stream",
  },
  deepseek: {
    id: "deepseek",
    providerId: "deepseek",
    endpointId: "deepseek.openai",
    descriptor: { reasoning: strictReasoning },
    prepareExtensions: extensions => {
      const thinking = extensions.thinking === false ? "disabled" : "enabled"
      return {
        ...omit(extensions, ["thinking", "reasoningEffort", "exposeReasoning", "extra_body", "reasoning_effort"]),
        ...(extensions.degradeMissingReasoningReplay === true
          ? { degradeMissingReasoningReplay: true }
          : {}),
        __deepstrikeThinkingEnabled: thinking !== "disabled",
        reasoning_effort: extensions.reasoningEffort === "max" ? "max" : "high",
        extra_body: { thinking: { type: thinking } },
      }
    },
    cacheKey: "none",
    inlineThinkingTags: false,
    exposeReasoning: extensions => extensions.exposeReasoning === true,
    requireReasoningReplay: extensions =>
      extensions.__deepstrikeThinkingEnabled !== false && extensions.thinking !== false,
    replay: "deepseek",
  },
  kimi: {
    id: "kimi",
    providerId: "kimi",
    endpointId: "kimi.openai",
    descriptor: { reasoning: portableReasoning },
    prepareExtensions: passthrough,
    cacheKey: "openai",
    inlineThinkingTags: true,
    exposeReasoning: always,
    requireReasoningReplay: never,
    replay: "generic_stream",
  },
  qwen: {
    id: "qwen",
    providerId: "qwen",
    endpointId: "qwen.dashscope",
    descriptor: { reasoning: outOfBandReasoning },
    prepareExtensions: extensions => {
      const enableThinking = Boolean(extensions.enableThinking ?? extensions.enable_thinking)
      const thinkingBudget = extensions.thinkingBudget ?? extensions.thinking_budget
      const extraBody: Record<string, unknown> = {}
      if (enableThinking) {
        extraBody.enable_thinking = true
        if (typeof thinkingBudget === "number") extraBody.thinking_budget = thinkingBudget
      }
      if (extensions.enable_search) {
        extraBody.enable_search = true
        if (extensions.search_options != null) extraBody.search_options = extensions.search_options
      }
      return {
        ...omit(extensions, [
          "extra_body", "enableThinking", "enable_thinking", "thinkingBudget", "thinking_budget",
          "enable_search", "search_options",
        ]),
        ...(Object.keys(extraBody).length ? { extra_body: extraBody } : {}),
      }
    },
    cacheKey: "none",
    inlineThinkingTags: false,
    exposeReasoning: always,
    requireReasoningReplay: never,
    replay: "generic_stream",
  },
  glm: {
    id: "glm",
    providerId: "glm",
    endpointId: "glm.openai",
    descriptor: { reasoning: portableReasoning },
    prepareExtensions: extensions => omit(extensions, ["web_search"]),
    serverTools: extensions => extensions.web_search
      ? [{
          type: "web_search",
          web_search: typeof extensions.web_search === "object" ? extensions.web_search : {},
        }]
      : [],
    cacheKey: "openai",
    inlineThinkingTags: true,
    exposeReasoning: always,
    requireReasoningReplay: never,
    replay: "generic_stream",
  },
  minimax: {
    id: "minimax",
    providerId: "minimax",
    endpointId: "minimax.openai",
    descriptor: { reasoning: strictReasoning },
    prepareExtensions: extensions => {
      const reasoningSplit = extensions.reasoning_split !== false
      return {
        ...omit(extensions, ["reasoning_split", "exposeReasoning"]),
        ...(extensions.degradeMissingReasoningReplay === true
          ? { degradeMissingReasoningReplay: true }
          : {}),
        __deepstrikeThinkingEnabled: reasoningSplit,
        reasoning_split: reasoningSplit,
      }
    },
    cacheKey: "none",
    inlineThinkingTags: false,
    exposeReasoning: extensions => extensions.exposeReasoning === true,
    requireReasoningReplay: extensions =>
      extensions.__deepstrikeThinkingEnabled !== false && extensions.reasoning_split !== false,
    replay: "minimax",
  },
} as const satisfies Record<string, OpenAIChatWireDialect>

export type OpenAIChatDialectId = keyof typeof openAIChatDialects

export function replayForTurn(
  dialect: OpenAIChatWireDialect,
  phase: "complete" | "stream",
  model: string,
  content: string,
  toolCalls: ToolCall[],
  reasoning: OpenAIChatTurnReasoning,
): ProviderReplay | undefined {
  switch (dialect.replay) {
    case "none": return undefined
    case "generic_stream":
      return phase === "stream" && (toolCalls.length > 0 || reasoning.reasoningContent)
        ? { reasoning_content: reasoning.reasoningContent }
        : undefined
    case "deepseek":
      if (!reasoning.reasoningContent.trim()) return undefined
      return {
        schema_version: 2,
        provider: dialect.providerId,
        protocol: "openai-chat",
        model,
        reasoning_content: reasoning.reasoningContent,
        ...(reasoning.nativeToolCalls.length ? { tool_calls: reasoning.nativeToolCalls } : {}),
      }
    case "minimax": {
      const hasReasoning = reasoning.reasoningContent.trim().length > 0
      const hasDetails = reasoning.reasoningDetails !== undefined && reasoning.reasoningDetails !== null
      if (!hasReasoning && !hasDetails) return undefined
      return {
        schema_version: 2,
        provider: dialect.providerId,
        protocol: "openai-chat",
        model,
        ...(hasReasoning ? { reasoning_content: reasoning.reasoningContent } : {}),
        ...(hasDetails ? { reasoning_details: reasoning.reasoningDetails } : {}),
        ...(reasoning.nativeToolCalls.length ? { tool_calls: reasoning.nativeToolCalls } : {}),
      }
    }
  }
}
