import type { ProviderDescriptor, RuntimePolicy } from "../types.js"
import { OpenAIChatProvider, type OpenAIChatTurnReasoning } from "./openai.js"
import { AnthropicCompatibleProvider } from "./anthropic-compatible.js"
import { endpointProfiles } from "./profiles.js"
import { omitExtensionKeys } from "./base.js"
import { DEEPSEEK_POLICIES, anthropicVendorProfiles } from "./vendor-profiles.js"

/**
 * DeepSeek over its Anthropic-compatible endpoint.
 * @deprecated Prefer `deepseek({ protocol: "anthropic" })`. Behavior is now fully
 * data-driven via `anthropicVendorProfiles.deepseek`; this thin shim is kept for
 * backward compatibility and `instanceof` checks.
 */
export class DeepSeekAnthropicProvider extends AnthropicCompatibleProvider {
  constructor(
    apiKey: string,
    model?: string,
    retry?: { maxRetries: number; baseDelay: number },
    baseURL?: string,
  ) {
    super(anthropicVendorProfiles.deepseek, apiKey, model, retry, baseURL)
  }
}

/**
 * DeepSeek over its OpenAI-compatible endpoint. Reasoning is carried out-of-band as
 * `reasoning_content`; replay persists DeepSeek's schema_version-2 envelope (with the
 * native `tool_calls` blocks). Request shaping (`reasoning_effort` + `extra_body.thinking`)
 * and replay are supplied via the OpenAIChatProvider Template-Method hooks; the streaming /
 * tool-call machinery is inherited from the base class.
 */
export class DeepSeekProvider extends OpenAIChatProvider {
  constructor(
    apiKey: string,
    model: string = "deepseek-v4-flash",
    retry?: { maxRetries: number; baseDelay: number },
    baseURL: string = endpointProfiles["deepseek.openai"].baseURL,
  ) {
    super(apiKey, model, retry, baseURL)
  }

  override runtimePolicy(): RuntimePolicy {
    return DEEPSEEK_POLICIES[this.model] ?? {}
  }

  override descriptor(): ProviderDescriptor {
    return {
      provider: "deepseek",
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
    return extensions?.thinking !== false
  }

  // DeepSeek strictly validates the request body and 400s on unknown params, so never send
  // OpenAI's `prompt_cache_key` (DeepSeek auto prefix-caches anyway).
  // Ref: https://api-docs.deepseek.com/quick_start/error_codes
  protected override cacheKeyParams(): Record<string, unknown> {
    return {}
  }

  // Reasoning arrives out-of-band as `reasoning_content`, never as inline <thinking> tags.
  protected override usesInlineThinkingTags(): boolean {
    return false
  }

  protected override exposeReasoningDelta(extensions?: Record<string, unknown>): boolean {
    return (extensions?.exposeReasoning ?? false) as boolean
  }

  protected override prepareExtensions(extensions?: Record<string, unknown>): Record<string, unknown> {
    const thinking = extensions?.thinking === false ? "disabled" : "enabled"
    const thinkingEnabled = thinking !== "disabled"
    const reasoningEffort = extensions?.reasoningEffort === "max" ? "max" : "high"
    return {
      ...omitExtensionKeys(extensions, ["thinking", "reasoningEffort", "exposeReasoning", "extra_body", "reasoning_effort"]),
      __deepstrikeThinkingEnabled: thinkingEnabled,
      // Re-thread the degrade control flag (omitExtensionKeys strips internal keys) so
      // buildChatMessages can honor it; the base requestExtensions omit keeps it off nothing.
      ...(extensions?.degradeMissingReasoningReplay === true ? { degradeMissingReasoningReplay: true } : {}),
      reasoning_effort: reasoningEffort,
      extra_body: { thinking: { type: thinking } },
    }
  }

  protected override rememberCompleteReplay(content: string, toolCalls: Array<{ id: string; name: string; arguments: string }>, r: OpenAIChatTurnReasoning): void {
    this.rememberDeepSeekReplay(content, toolCalls, r.reasoningContent, r.nativeToolCalls)
  }

  protected override rememberStreamReplay(content: string, toolCalls: Array<{ id: string; name: string; arguments: string }>, r: OpenAIChatTurnReasoning): void {
    this.rememberDeepSeekReplay(content, toolCalls, r.reasoningContent, r.nativeToolCalls)
  }

  private rememberDeepSeekReplay(
    content: string,
    toolCalls: Array<{ id: string; name: string; arguments: string }>,
    reasoningContent: unknown,
    nativeToolCalls: unknown[],
  ): void {
    if (typeof reasoningContent !== "string" || !reasoningContent.trim()) return
    this.chat.rememberReplayFields({ content, toolCalls }, {
      schema_version: 2,
      provider: "deepseek",
      protocol: "openai-chat",
      model: this.model,
      reasoning_content: reasoningContent,
      ...(nativeToolCalls.length ? { tool_calls: nativeToolCalls } : {}),
    })
  }
}
