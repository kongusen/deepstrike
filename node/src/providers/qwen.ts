import type { Message, ProviderDescriptor, ProviderReplay, RuntimePolicy } from "../types.js"
import { OpenAIChatProvider } from "./openai.js"
import { AnthropicCompatibleProvider } from "./anthropic-compatible.js"
import { omitExtensionKeys } from "./base.js"
import { endpointProfiles } from "./profiles.js"
import { QWEN_POLICIES, anthropicVendorProfiles } from "./vendor-profiles.js"

/**
 * Qwen over its Anthropic-compatible endpoint.
 * @deprecated Prefer `qwen({ protocol: "anthropic" })`. Behavior is now fully
 * data-driven via `anthropicVendorProfiles.qwen`; this thin shim is kept for
 * backward compatibility and `instanceof` checks.
 */
export class QwenAnthropicProvider extends AnthropicCompatibleProvider {
  constructor(
    apiKey: string,
    model?: string,
    retry?: { maxRetries: number; baseDelay: number },
    baseURL?: string,
  ) {
    super(anthropicVendorProfiles.qwen, apiKey, model, retry, baseURL)
  }
}

/**
 * Qwen / DashScope over its OpenAI-compatible (DashScope) endpoint. Reasoning is carried
 * out-of-band as `reasoning_content`; thinking is opted into via `enable_thinking` /
 * `thinking_budget` (sent under `extra_body`). The streaming / tool-call machinery and the
 * default `{ reasoning_content }` replay are inherited from OpenAIChatProvider; only request
 * shaping and the (string-coerced) replay peek differ, supplied via the Template-Method hooks.
 */
export class QwenProvider extends OpenAIChatProvider {
  constructor(
    apiKey: string,
    model: string = "qwen3.6-plus",
    retry?: { maxRetries: number; baseDelay: number },
    baseURL: string = endpointProfiles["qwen.dashscope"].baseURL,
  ) {
    super(apiKey, model, retry, baseURL)
  }

  override runtimePolicy(): RuntimePolicy {
    return QWEN_POLICIES[this.model] ?? {}
  }

  override descriptor(): ProviderDescriptor {
    return {
      provider: "qwen",
      protocol: "openai-chat",
      model: this.model,
      reasoning: {
        supported: true,
        preserveAcrossToolTurns: true,
      },
      toolCalls: {
        supported: true,
        requiresStrictPairing: true,
      },
    }
  }

  // DashScope auto prefix-caches and does not accept OpenAI's `prompt_cache_key`; omit it.
  protected override cacheKeyParams(): Record<string, unknown> {
    return {}
  }

  // Reasoning arrives out-of-band as `reasoning_content`, never as inline <thinking> tags.
  protected override usesInlineThinkingTags(): boolean {
    return false
  }

  protected override requestBodyExtras(extensions?: Record<string, unknown>): Record<string, unknown> {
    const extraBody = this.thinkingExtraBody(extensions)
    return extraBody ? { extra_body: extraBody } : {}
  }

  protected override requestExtensions(extensions?: Record<string, unknown>): Record<string, unknown> {
    return omitExtensionKeys(extensions, [
      "model", "messages", "tools", "stream", "stream_options", "extra_body",
      "enableThinking", "enable_thinking", "thinkingBudget", "thinking_budget",
    ])
  }

  override peekProviderReplay(message: Pick<Message, "content" | "toolCalls">): ProviderReplay | undefined {
    const fields = this.chat.peekReplayFields(message)
    if (!fields || !("reasoning_content" in fields)) return undefined
    return { reasoning_content: String(fields.reasoning_content ?? "") }
  }

  override seedProviderReplay(message: Pick<Message, "content" | "toolCalls">, replay: ProviderReplay): void {
    if (replay.reasoning_content !== undefined) {
      this.chat.rememberReplayFields(message, { reasoning_content: replay.reasoning_content })
    }
  }

  private thinkingExtraBody(extensions?: Record<string, unknown>): Record<string, unknown> | undefined {
    const enableThinking = Boolean(extensions?.enableThinking ?? extensions?.enable_thinking)
    const thinkingBudget = extensions?.thinkingBudget ?? extensions?.thinking_budget
    if (!enableThinking) return undefined
    return {
      enable_thinking: true,
      ...(typeof thinkingBudget === "number" ? { thinking_budget: thinkingBudget } : {}),
    }
  }
}
