import type { ProviderDescriptor, RuntimePolicy } from "../types.js"
import { OpenAIChatProvider, type OpenAIChatTurnReasoning } from "./openai.js"
import { AnthropicCompatibleProvider } from "./anthropic-compatible.js"
import { endpointProfiles } from "./profiles.js"
import { omitExtensionKeys } from "./base.js"
import { MINIMAX_POLICIES, anthropicVendorProfiles } from "./vendor-profiles.js"

/**
 * MiniMax over its Anthropic-compatible endpoint. Replay is carried as Anthropic
 * `native_blocks` (thinking/text/tool_use), identical to the first-party
 * Anthropic provider.
 * @deprecated Prefer `minimax({ protocol: "anthropic" })`. Behavior is now fully
 * data-driven via `anthropicVendorProfiles.minimax`; this thin shim is kept for
 * backward compatibility and `instanceof` checks.
 */
export class MiniMaxAnthropicProvider extends AnthropicCompatibleProvider {
  constructor(
    apiKey: string,
    model?: string,
    retry?: { maxRetries: number; baseDelay: number },
    baseURL?: string,
  ) {
    super(anthropicVendorProfiles.minimax, apiKey, model, retry, baseURL)
  }
}

/**
 * MiniMax over its OpenAI-compatible endpoint. Replay is carried as
 * `reasoning_content` / `reasoning_details` (split reasoning) in a schema_version-2
 * envelope; requests default to `reasoning_split: true` so reasoning is returned
 * out-of-band rather than embedded in the message content. Request shaping and replay
 * are supplied via the OpenAIChatProvider Template-Method hooks; the streaming /
 * tool-call machinery is inherited from the base class.
 */
export class MiniMaxOpenAIProvider extends OpenAIChatProvider {
  constructor(
    apiKey: string,
    model: string = "MiniMax-M2.7",
    retry?: { maxRetries: number; baseDelay: number },
    baseURL: string = endpointProfiles["minimax.openai"].baseURL,
  ) {
    super(apiKey, model, retry, baseURL)
  }

  override runtimePolicy(): RuntimePolicy {
    return MINIMAX_POLICIES[this.model] ?? {}
  }

  override descriptor(): ProviderDescriptor {
    return {
      provider: "minimax",
      protocol: "openai-chat",
      model: this.model,
      reasoning: {
        supported: true,
        preserveAcrossToolTurns: true,
        requiresReplayForToolTurns: true,
      },
      toolCalls: {
        supported: true,
        requiresStrictPairing: true,
      },
    }
  }

  protected override requireNonEmptyReasoningReplayForToolTurns(extensions?: Record<string, unknown>): boolean {
    if (extensions?.__deepstrikeThinkingEnabled === false) return false
    return extensions?.reasoning_split !== false
  }

  // MiniMax auto prefix-caches and does not accept OpenAI's `prompt_cache_key`; omit it.
  protected override cacheKeyParams(): Record<string, unknown> {
    return {}
  }

  // Reasoning arrives out-of-band (reasoning_content / reasoning_details), never inline tags.
  protected override usesInlineThinkingTags(): boolean {
    return false
  }

  protected override exposeReasoningDelta(extensions?: Record<string, unknown>): boolean {
    return (extensions?.exposeReasoning ?? false) as boolean
  }

  protected override prepareExtensions(extensions?: Record<string, unknown>): Record<string, unknown> {
    const reasoningSplit = extensions?.reasoning_split !== false
    return {
      ...omitExtensionKeys(extensions, ["reasoning_split", "exposeReasoning"]),
      __deepstrikeThinkingEnabled: reasoningSplit,
      // Re-thread the degrade control flag (omitExtensionKeys strips internal keys) so
      // buildChatMessages can honor it; the wire-request omit drops it.
      ...(extensions?.degradeMissingReasoningReplay === true ? { degradeMissingReasoningReplay: true } : {}),
      reasoning_split: reasoningSplit,
    }
  }

  protected override rememberCompleteReplay(content: string, toolCalls: Array<{ id: string; name: string; arguments: string }>, r: OpenAIChatTurnReasoning): void {
    this.rememberMiniMaxReplay(content, toolCalls, r.reasoningContent, r.reasoningDetails, r.nativeToolCalls)
  }

  protected override rememberStreamReplay(content: string, toolCalls: Array<{ id: string; name: string; arguments: string }>, r: OpenAIChatTurnReasoning): void {
    this.rememberMiniMaxReplay(content, toolCalls, r.reasoningContent, r.reasoningDetails, r.nativeToolCalls)
  }

  private rememberMiniMaxReplay(
    content: string,
    toolCalls: Array<{ id: string; name: string; arguments: string }>,
    reasoningContent: unknown,
    reasoningDetails: unknown,
    nativeToolCalls: unknown[],
  ): void {
    const hasReasoning = typeof reasoningContent === "string" && reasoningContent.trim().length > 0
    const hasDetails = reasoningDetails !== undefined && reasoningDetails !== null
    if (!hasReasoning && !hasDetails) return
    this.chat.rememberReplayFields({ content, toolCalls }, {
      schema_version: 2,
      provider: "minimax",
      protocol: "openai-chat",
      model: this.model,
      ...(hasReasoning ? { reasoning_content: reasoningContent } : {}),
      ...(hasDetails ? { reasoning_details: reasoningDetails } : {}),
      ...(nativeToolCalls.length ? { tool_calls: nativeToolCalls } : {}),
    })
  }
}
